//! The QUIC endpoint and the authenticated link built on it.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use seam_proto::{Frame, PeerId};

use crate::{Error, Fingerprint, Identity, PairingCode, Trust, TrustStore, pairing, tls};

/// Largest datagram seam ever sends. Motion frames are ~24 bytes; this is headroom, and
/// is well under any plausible path MTU so a motion datagram is never dropped for size.
const MAX_DATAGRAM: usize = 512;

/// Transport tuning.
///
/// Every line here overrides a quinn default that is wrong for input forwarding. The
/// defaults are tuned for bulk transfer over the open internet; we are moving tiny
/// messages across a LAN, and want them to arrive now or not at all.
fn transport_config() -> quinn::TransportConfig {
    let mut tc = quinn::TransportConfig::default();

    // Default is 333 ms — an internet-scale guess. On a LAN the real RTT is well under a
    // millisecond, and the initial estimate drives loss-detection timers, so this is the
    // single largest latency win available in the config.
    tc.initial_rtt(Duration::from_millis(1));

    // Default is 1 MiB. At ~60 B per motion frame that is roughly 17 *seconds* of queued
    // input — a buffer that large converts a brief stall into a long replay of stale
    // cursor positions. Keep it small so quinn's drop-oldest policy discards stale motion
    // instead of faithfully delivering it late.
    tc.datagram_send_buffer_size(4 * 1024);
    tc.datagram_receive_buffer_size(Some(16 * 1024));

    // Default is None. A heartbeat keeps NAT and firewall state alive, and — more
    // importantly here — keeps the receiver's Wi-Fi radio out of deep power save, which
    // the research identified as the single largest source of latency in this class of
    // app (a measured 78 ms RTT on this very LAN).
    tc.keep_alive_interval(Some(Duration::from_secs(1)));

    // Default is 30 s. A peer that has genuinely gone must be noticed quickly, because
    // the receiving side releases held keys and returns the cursor on that signal (R2).
    //
    // But not in 5 s. A power-managed Wi-Fi adapter naps for longer than that while a
    // link is idle, and both sides then declare "timed out" while both processes are
    // demonstrably fine — a laptop spent a whole morning connecting and dying every few
    // seconds-to-minutes, dropping out of the desk each time, its console and the
    // server's log each blaming the other side's silence. Fifteen seconds rides out an
    // adapter nap; with a 1 s keep-alive it still means fifteen missed heartbeats
    // before anyone is declared dead, and a machine that really is gone still hands
    // input back well inside a person's patience.
    tc.max_idle_timeout(Some(
        Duration::from_secs(15).try_into().expect("15s is a valid idle timeout"),
    ));

    // Default is 1200, then MTU discovery ramps up. On a LAN the path supports 1500, and
    // the ramp costs nothing to skip.
    tc.initial_mtu(1400);

    tc
}

/// A bound QUIC endpoint. One per machine; it both dials and accepts, because peers are
/// symmetric and neither is "the server" (goal O3).
pub struct Endpoint {
    inner: quinn::Endpoint,
    identity: Arc<Identity>,
}

impl core::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Endpoint")
            .field("local_addr", &self.inner.local_addr().ok())
            .field("identity", &self.identity)
            .finish()
    }
}

impl Endpoint {

    /// Replace the UDP socket underneath this endpoint, keeping its identity and config.
    ///
    /// A laptop that sleeps wakes with a different network underneath it: the socket bound
    /// before standby still exists, still accepts writes, and reaches nothing. Every
    /// reconnect attempt then fails forever while looking like an unreachable peer, which
    /// is why sharing only came back after restarting seam.
    ///
    /// Rebinding is cheap and safe to do when nothing is wrong — the endpoint keeps its
    /// certificate, so peers still recognise this machine.
    pub fn rebind(&self, port: u16) -> Result<(), Error> {
        let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, port));
        let socket = std::net::UdpSocket::bind(addr)
            .map_err(|e| Error::Bind { addr, reason: e.to_string() })?;
        self.inner.rebind(socket).map_err(|e| Error::Bind { addr, reason: e.to_string() })
    }
    /// Bind to `addr`. Use port 0 to let the OS choose (tests, and any peer that does not
    /// need a stable port because it is found by discovery rather than by address).
    pub fn bind(identity: Arc<Identity>, addr: SocketAddr) -> Result<Self, Error> {
        let transport = Arc::new(transport_config());

        let mut server = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(tls::server_config(&identity)?)
                .map_err(|e| Error::Tls(e.to_string()))?,
        ));
        server.transport_config(Arc::clone(&transport));

        let mut client = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls::client_config(&identity)?)
                .map_err(|e| Error::Tls(e.to_string()))?,
        ));
        client.transport_config(transport);

        let mut inner = quinn::Endpoint::server(server, addr)
            .map_err(|e| Error::Bind { addr, reason: e.to_string() })?;
        inner.set_default_client_config(client);

        Ok(Self { inner, identity })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        self.inner.local_addr().map_err(|e| Error::Bind {
            addr: "0.0.0.0:0".parse().expect("literal is a valid address"),
            reason: e.to_string(),
        })
    }

    #[must_use]
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Dial a peer.
    ///
    /// Succeeds as soon as the connection is cryptographically established. It does
    /// **not** check whether the peer is paired — that is [`Link::authorize`], kept
    /// separate so the pairing flow can inspect an unpaired link long enough to show its
    /// [`Link::pairing_code`].
    pub async fn connect(&self, addr: SocketAddr) -> Result<Link, Error> {
        let connecting = self
            .inner
            .connect(addr, tls::SERVER_NAME)
            .map_err(|e| Error::Connect { addr, reason: e.to_string() })?;
        let connection =
            connecting.await.map_err(|e| Error::Connect { addr, reason: e.to_string() })?;
        Link::from_connection(connection)
    }

    /// Accept an inbound connection. `None` once the endpoint is closed.
    pub async fn accept(&self) -> Option<Result<Link, Error>> {
        let incoming = self.inner.accept().await?;
        let addr = incoming.remote_address();
        Some(match incoming.await {
            Ok(connection) => Link::from_connection(connection),
            // Not `Error::Connect`: that one says "could not reach the peer at …", which
            // reads as an outbound failure. An afternoon was spent chasing a log that
            // said a machine could not *reach* an address it was in fact being dialled
            // from.
            Err(e) => Err(Error::Handshake { addr, reason: e.to_string() }),
        })
    }

    /// Close the endpoint and every connection on it.
    pub fn close(&self) {
        self.inner.close(0u32.into(), b"seam shutting down");
    }
}

/// An established, cryptographically authenticated connection to one peer.
///
/// Authenticated is not the same as *authorised*: the peer has proved which key it holds,
/// but whether that key is one we have paired with is [`Link::authorize`].
pub struct Link {
    connection: quinn::Connection,
    peer: Fingerprint,
}

impl core::fmt::Debug for Link {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Link")
            .field("peer", &self.peer)
            .field("remote", &self.connection.remote_address())
            .field("rtt", &self.connection.rtt())
            .finish()
    }
}

impl Link {
    fn from_connection(connection: quinn::Connection) -> Result<Self, Error> {
        // The TLS config requires a client certificate, so this cannot be None. Reported
        // rather than asserted so that an unauthenticated peer fails closed if that
        // configuration ever changes.
        let peer = tls::peer_fingerprint(&connection).ok_or(Error::NoPeerCertificate)?;
        Ok(Self { connection, peer })
    }

    #[must_use]
    pub const fn peer_fingerprint(&self) -> Fingerprint {
        self.peer
    }

    #[must_use]
    pub fn peer_id(&self) -> PeerId {
        self.peer.peer_id()
    }

    #[must_use]
    pub fn remote_address(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    /// Current smoothed round-trip estimate, for diagnostics and the liveness watchdog.
    #[must_use]
    pub fn rtt(&self) -> Duration {
        self.connection.rtt()
    }

    /// The 6-digit code to display during pairing.
    ///
    /// Derived from this connection's TLS exporter, so a man-in-the-middle — who must
    /// terminate two separate TLS sessions — produces a different code on each side and
    /// the mismatch is visible. See [`crate::pairing`].
    pub fn pairing_code(&self) -> Result<PairingCode, Error> {
        let mut secret = [0u8; 32];
        self.connection
            .export_keying_material(&mut secret, pairing::EXPORTER_LABEL, &[])
            .map_err(|_| Error::Tls("connection is not ready to derive a pairing code".into()))?;
        Ok(PairingCode::from_exporter(&secret))
    }

    /// Check this peer against the trust store.
    ///
    /// Deliberately separate from connecting: each outcome needs different handling, and
    /// collapsing them into one boolean is how tools end up silently accepting impostors.
    pub fn authorize(&self, store: &TrustStore) -> Result<(), Error> {
        match store.classify(self.peer) {
            Trust::Trusted => Ok(()),
            Trust::Unknown => Err(Error::NotPaired),
            Trust::Conflict => Err(Error::IdentityConflict),
        }
    }

    /// Send a frame as an **unreliable datagram**.
    ///
    /// Only motion may travel this way — everything else cannot reconstruct itself from a
    /// later packet. The check is enforced, not documented, because getting it wrong
    /// means a silently lost key-up, i.e. a stuck modifier.
    pub fn send_datagram(&self, frame: &Frame, buf: &mut Vec<u8>) -> Result<(), Error> {
        if !frame.is_datagram_safe() {
            return Err(Error::NotDatagramSafe);
        }
        buf.clear();
        frame.encode(buf)?;
        // `send_datagram`, never `send_datagram_wait`: under back-pressure quinn discards
        // the *oldest* queued datagrams to make room, which is exactly right for motion.
        // `send_datagram_wait` would prioritise stale cursor positions over fresh ones.
        self.connection
            .send_datagram(bytes::Bytes::copy_from_slice(buf))
            .map_err(|e| Error::Send(e.to_string()))
    }

    /// Receive the next datagram and decode it.
    pub async fn recv_datagram(&self) -> Result<Frame, Error> {
        let bytes =
            self.connection.read_datagram().await.map_err(|e| Error::Recv(e.to_string()))?;
        if bytes.len() > MAX_DATAGRAM {
            return Err(Error::Recv(format!("datagram of {} bytes is oversized", bytes.len())));
        }
        Ok(Frame::decode(&bytes)?)
    }

    /// Send a frame on a **reliable** stream.
    ///
    /// Opens a fresh unidirectional stream per frame. Streams are cheap in QUIC, and one
    /// per frame means a stalled or failed frame cannot head-of-line-block the next —
    /// which is the whole reason for not using TCP.
    pub async fn send_reliable(&self, frame: &Frame) -> Result<(), Error> {
        self.send_with_priority(frame, 0).await
    }

    /// Send a frame as **bulk**: reliable, but scheduled behind everything else.
    ///
    /// On a healthy LAN this is indistinguishable from `send_reliable`. On a thin or
    /// degraded path it is the difference between a usable desk and a glitching one:
    /// QUIC schedules scarce bandwidth by stream priority, so a megabyte screenshot
    /// marked bulk trickles while keystrokes and clicks — tiny, priority zero — go
    /// first. A client on a bad network is a normal machine, not a broken one, and
    /// input must feel local there too.
    pub async fn send_bulk(&self, frame: &Frame) -> Result<(), Error> {
        self.send_with_priority(frame, -1).await
    }

    async fn send_with_priority(&self, frame: &Frame, priority: i32) -> Result<(), Error> {
        let mut buf = Vec::with_capacity(64);
        frame.encode_framed(&mut buf)?;
        let mut stream =
            self.connection.open_uni().await.map_err(|e| Error::Send(e.to_string()))?;
        if priority != 0 {
            let _ = stream.set_priority(priority);
        }
        stream.write_all(&buf).await.map_err(|e| Error::Send(e.to_string()))?;
        stream.finish().map_err(|e| Error::Send(e.to_string()))?;
        Ok(())
    }

    /// Receive the next frame from a reliable stream.
    pub async fn recv_reliable(&self) -> Result<Frame, Error> {
        let mut stream =
            self.connection.accept_uni().await.map_err(|e| Error::Recv(e.to_string()))?;
        // Bounded before allocating: a peer-supplied length must never size a buffer.
        let bytes = stream
            .read_to_end(seam_proto::MAX_FRAME_LEN + 4)
            .await
            .map_err(|e| Error::Recv(e.to_string()))?;
        match Frame::decode_framed(&bytes)? {
            Some((frame, _)) => Ok(frame),
            None => Err(Error::Recv("stream ended mid-frame".into())),
        }
    }

    pub fn close(&self, reason: &str) {
        self.connection.close(0u32.into(), reason.as_bytes());
    }
}
