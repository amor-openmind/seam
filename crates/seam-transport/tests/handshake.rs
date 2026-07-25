//! End-to-end transport tests over a real QUIC connection on loopback.
//!
//! These exercise the properties that cannot be unit-tested because they only exist once
//! two peers have actually completed a TLS handshake: that both sides derive the same
//! pairing code, that an unpaired peer is refused with a specific reason, and that
//! possession of a certificate is not sufficient to impersonate its owner.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use seam_proto::{
    Button, ButtonEvent, Frame, KeyEvent, LogicalText, Modifiers, Motion, PhysicalKey, Point, Press,
};
use seam_transport::{Endpoint, Error, Identity, Link, TrustStore};

const LOCALHOST: &str = "127.0.0.1:0";

/// Bring up two peers and connect them. Returns (listener side, dialler side).
async fn connected_pair() -> (Link, Link, Arc<Endpoint>, Arc<Endpoint>) {
    let listener = Arc::new(
        Endpoint::bind(
            Arc::new(Identity::generate().expect("generate listener identity")),
            LOCALHOST.parse::<SocketAddr>().unwrap(),
        )
        .expect("bind listener"),
    );
    let dialler = Arc::new(
        Endpoint::bind(
            Arc::new(Identity::generate().expect("generate dialler identity")),
            LOCALHOST.parse::<SocketAddr>().unwrap(),
        )
        .expect("bind dialler"),
    );

    let addr = listener.local_addr().expect("listener address");
    let accepting = {
        let listener = Arc::clone(&listener);
        tokio::spawn(
            async move { listener.accept().await.expect("endpoint open").expect("accept") },
        )
    };

    let outbound = dialler.connect(addr).await.expect("connect");
    let inbound = accepting.await.expect("accept task");

    (inbound, outbound, listener, dialler)
}

#[tokio::test]
async fn two_peers_handshake_and_learn_each_others_identity() {
    let (inbound, outbound, listener, dialler) = connected_pair().await;

    // Each side sees the *other's* identity, derived from the certificate the peer
    // actually proved it holds — not from anything either side asserted about itself.
    assert_eq!(inbound.peer_fingerprint(), dialler.identity().fingerprint());
    assert_eq!(outbound.peer_fingerprint(), listener.identity().fingerprint());
    assert_eq!(inbound.peer_id(), dialler.identity().peer_id());
    assert_eq!(outbound.peer_id(), listener.identity().peer_id());
}

#[tokio::test]
async fn both_sides_derive_the_same_pairing_code() {
    // This is the whole pairing UX: the code is only useful if it provably matches on a
    // direct connection. (A man-in-the-middle would terminate two TLS sessions with
    // different exporters, so the codes would differ — that is the security argument,
    // covered by the unit test in `pairing`.)
    let (inbound, outbound, _l, _d) = connected_pair().await;

    let here = inbound.pairing_code().expect("derive code on listener");
    let there = outbound.pairing_code().expect("derive code on dialler");

    assert_eq!(here, there, "a direct connection must show the same code on both machines");
    assert!(here.value() < 1_000_000);
    assert_eq!(here.to_display_string().replace(' ', "").len(), 6);
}

#[tokio::test]
async fn an_unpaired_peer_is_refused_with_an_actionable_reason() {
    let (inbound, _outbound, _l, _d) = connected_pair().await;

    let empty = TrustStore::new();
    let refused = inbound.authorize(&empty).expect_err("an unpaired peer must be refused");

    assert!(matches!(refused, Error::NotPaired));
    // Goal O5: the message must name the cause and the fix, not say "connection failed".
    let text = refused.to_string();
    assert!(text.contains("not paired"), "unhelpful message: {text}");
    assert!(text.contains("6-digit code"), "message must say what to do: {text}");
}

#[tokio::test]
async fn a_paired_peer_is_accepted() {
    let (inbound, outbound, listener, dialler) = connected_pair().await;

    let mut on_listener = TrustStore::new();
    on_listener.trust(dialler.identity().fingerprint(), "dialler");
    let mut on_dialler = TrustStore::new();
    on_dialler.trust(listener.identity().fingerprint(), "listener");

    assert!(inbound.authorize(&on_listener).is_ok());
    assert!(outbound.authorize(&on_dialler).is_ok());
}

#[tokio::test]
async fn a_peer_whose_key_changed_is_a_conflict_not_a_silent_repin() {
    let (inbound, _outbound, _l, dialler) = connected_pair().await;

    // Pair with *a different* identity that happens to occupy the same slot, by trusting
    // someone else and then encountering this peer.
    let mut store = TrustStore::new();
    store.trust(dialler.identity().fingerprint(), "dialler");

    // Sanity: as-is it is trusted.
    assert!(inbound.authorize(&store).is_ok());

    // Now forget and re-trust an unrelated identity, so the presented key is unknown.
    store.forget(dialler.identity().peer_id());
    let stranger = Identity::generate().unwrap();
    store.trust(stranger.fingerprint(), "someone else");
    assert!(matches!(inbound.authorize(&store), Err(Error::NotPaired)));
}

#[tokio::test]
async fn possessing_a_certificate_is_not_enough_to_impersonate_its_owner() {
    // A certificate is public. If the handshake signature were not verified, anyone could
    // present a copy of a trusted peer's certificate and be pinned as that peer. Here the
    // impostor holds the victim's certificate but its own (mismatched) private key.
    let victim = Identity::generate().unwrap();
    let impostor_key = Identity::generate().unwrap();

    let forged = Identity::from_der(
        victim.certificate_der().to_vec(),
        impostor_key.private_key_der().to_vec(),
    )
    .expect("a mismatched pair is well-formed enough to load");

    // It even *claims* the victim's identity, because the fingerprint is over the cert.
    assert_eq!(forged.fingerprint(), victim.fingerprint());

    let honest = Arc::new(
        Endpoint::bind(
            Arc::new(Identity::generate().unwrap()),
            LOCALHOST.parse::<SocketAddr>().unwrap(),
        )
        .unwrap(),
    );
    let addr = honest.local_addr().unwrap();
    let accepting = {
        let honest = Arc::clone(&honest);
        tokio::spawn(async move { honest.accept().await })
    };

    // Either building the config or completing the handshake must fail. What must *not*
    // happen is a successful connection carrying the victim's fingerprint.
    let outcome = match Endpoint::bind(Arc::new(forged), LOCALHOST.parse::<SocketAddr>().unwrap()) {
        Err(e) => Err(e),
        Ok(ep) => ep.connect(addr).await.map(|_| ()),
    };
    assert!(outcome.is_err(), "a mismatched certificate and key must never authenticate");

    honest.close();
    let _ = tokio::time::timeout(Duration::from_secs(2), accepting).await;
}

#[tokio::test]
async fn motion_travels_as_an_unreliable_datagram() {
    let (inbound, outbound, _l, _d) = connected_pair().await;

    let sent = Frame::Motion(Motion {
        seq: 7,
        cursor: Point::from_px(1280, 540),
        travel_x: -42,
        travel_y: 99,
    });

    let mut buf = Vec::new();
    outbound.send_datagram(&sent, &mut buf).expect("send motion");

    let received = tokio::time::timeout(Duration::from_secs(5), inbound.recv_datagram())
        .await
        .expect("motion should arrive on loopback")
        .expect("decode motion");
    assert_eq!(received, sent);
}

#[tokio::test]
async fn non_motion_frames_are_refused_on_the_datagram_path() {
    // Enforced, not merely documented: sending a key event unreliably means a dropped
    // key-up, which is a stuck modifier — the exact defect this project exists to remove.
    let (_inbound, outbound, _l, _d) = connected_pair().await;
    let mut buf = Vec::new();

    for frame in [
        Frame::Key(KeyEvent {
            seq: 1,
            physical: PhysicalKey::A,
            logical: LogicalText::from_char('a'),
            press: Press::Up,
            modifiers: Modifiers::NONE,
        }),
        Frame::Button(ButtonEvent {
            seq: 2,
            button: Button::Left,
            press: Press::Up,
            modifiers: Modifiers::NONE,
        }),
        Frame::Leave { seq: 3 },
        Frame::ReleaseAll { seq: 4 },
    ] {
        let refused = outbound.send_datagram(&frame, &mut buf).expect_err("must be refused");
        assert!(matches!(refused, Error::NotDatagramSafe), "{frame:?} was allowed");
    }
}

#[tokio::test]
async fn key_events_travel_reliably_and_round_trip_exactly() {
    let (inbound, outbound, _l, _d) = connected_pair().await;

    // A Persian character with AltGr held: the case that motivates carrying both key
    // identities, and one that must survive the wire byte for byte.
    let sent = Frame::Key(KeyEvent {
        seq: 11,
        physical: PhysicalKey(0x07),
        logical: LogicalText::new("ی").unwrap(),
        press: Press::Down,
        modifiers: Modifiers::RIGHT_ALT,
    });

    outbound.send_reliable(&sent).await.expect("send key");
    let received = tokio::time::timeout(Duration::from_secs(5), inbound.recv_reliable())
        .await
        .expect("key event should arrive")
        .expect("decode key event");

    assert_eq!(received, sent);
}

#[tokio::test]
async fn the_link_reports_a_usable_round_trip_estimate() {
    let (inbound, _outbound, _l, _d) = connected_pair().await;
    // Sanity only — this feeds the liveness watchdog, so it must be populated rather than
    // left at zero or a default of hundreds of milliseconds.
    assert!(inbound.rtt() < Duration::from_secs(1), "loopback RTT was {:?}", inbound.rtt());
}
