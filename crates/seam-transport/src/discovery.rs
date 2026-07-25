//! Zero-configuration peer discovery over mDNS/DNS-SD.
//!
//! # The security property that matters
//!
//! **Nothing advertised here is trusted.** mDNS is unauthenticated: any device on the
//! LAN can claim any name, any peer id and any fingerprint. The values in a service
//! record exist only so the UI can show *something* before a connection is made.
//!
//! Authority rests entirely with the TLS handshake and the trust store. A peer is who it
//! proves it is, never who it says it is. This is precisely the mistake behind
//! CVE-2021-42073, where any Barrier client that sent a name matching the server's config
//! — default `"Unnamed"` — was admitted and could read the server's keystrokes.
//!
//! So [`DiscoveredPeer::advertised_peer_id`] is deliberately named as a claim, and
//! [`DiscoveredPeer::advertised_fingerprint`] is a *hint* used to decide whether to
//! bother dialling, never to decide whether to trust.
//!
//! # Why mDNS, and what it does not cover
//!
//! mDNS resolved all three machines on the development LAN correctly, including two
//! Windows hosts whose firewalls drop ICMP entirely — which is also why **liveness must
//! never be tested with ping**. It does fail in real environments though: AP client
//! isolation, enterprise Wi-Fi that blocks multicast, and VPN interfaces all break it.
//! Syncthing and KDE Connect both use UDP broadcast instead for exactly this reason.
//! A broadcast fallback and manual address entry are planned; see goal Z1.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr, UdpSocket};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use seam_proto::PeerId;

use crate::{Error, Fingerprint};

/// DNS-SD service type. `_udp` because seam speaks QUIC.
pub const SERVICE_TYPE: &str = "_seam._udp.local.";

const PROP_PEER_ID: &str = "id";
const PROP_FINGERPRINT: &str = "fp";
const PROP_PROTOCOL: &str = "proto";
const PROP_NAME: &str = "name";

/// A peer seen on the network. **Everything here is an unverified claim** — see the
/// module docs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DiscoveredPeer {
    /// The id the peer *claims*. Confirmed only by a successful handshake.
    pub advertised_peer_id: Option<PeerId>,
    /// The fingerprint the peer *claims*. Used to decide whether we appear to know this
    /// peer already; never used to decide trust.
    pub advertised_fingerprint: Option<Fingerprint>,
    /// Display name. Cosmetic, and never load-bearing (goal O4).
    pub name: String,
    /// Every address it can be reached on.
    ///
    /// All of them, not one: a machine with Wi-Fi and Ethernet advertises several, and
    /// picking the wrong one is a classic silent failure. The dialler races them and
    /// takes the first that connects.
    pub addresses: Vec<SocketAddr>,
    /// The protocol version it claims to speak, if it said.
    pub advertised_protocol: Option<u16>,
    /// The DNS-SD instance name, used to correlate a later removal event.
    pub instance: String,
}

impl DiscoveredPeer {
    /// Whether this peer plausibly matches a fingerprint we already trust.
    ///
    /// A hint for the UI and for deciding whether to auto-dial. It is **not** an
    /// authorisation check — [`crate::Link::authorize`] is.
    #[must_use]
    pub fn claims_to_be(&self, known: Fingerprint) -> bool {
        self.advertised_fingerprint == Some(known)
    }
}

/// Something changed on the network.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DiscoveryEvent {
    /// A peer appeared, or its record was updated.
    Found(Box<DiscoveredPeer>),
    /// A peer's record went away. Carries the DNS-SD instance name, because a removal
    /// event carries no properties to read an id from.
    Lost { instance: String },
}

/// Advertises this machine and watches for others.
pub struct Discovery {
    daemon: ServiceDaemon,
    advertised: Option<String>,
}

impl core::fmt::Debug for Discovery {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Discovery").field("advertised", &self.advertised).finish_non_exhaustive()
    }
}

impl Discovery {
    pub fn new() -> Result<Self, Error> {
        let daemon = ServiceDaemon::new().map_err(|e| Error::Discovery(e.to_string()))?;
        Ok(Self { daemon, advertised: None })
    }

    /// The machine's own name, used purely for display.
    ///
    /// Detected, never configured (goal Z2). If the hostname cannot be read we use a
    /// short form of the peer id rather than asking: a machine with an awkward name is a
    /// cosmetic problem, whereas a setup that stops to ask a question is a real one.
    #[must_use]
    pub fn default_display_name(peer_id: PeerId) -> String {
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .map(|h| h.trim_end_matches(".local").to_owned())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| format!("seam-{peer_id}"))
    }

    /// Start advertising this machine on `port`.
    pub fn advertise(
        &mut self,
        name: &str,
        peer_id: PeerId,
        fingerprint: Fingerprint,
        port: u16,
    ) -> Result<(), Error> {
        // The instance name is derived from the peer id, not the hostname: two machines
        // that happen to share a hostname must not collide, and renaming a machine must
        // not look like a different peer appearing.
        let instance = format!("seam-{peer_id}");

        let properties = [
            (PROP_PEER_ID, peer_id.to_string()),
            (PROP_FINGERPRINT, hex(fingerprint.as_bytes())),
            (PROP_PROTOCOL, seam_proto::PROTOCOL_VERSION.to_string()),
            (PROP_NAME, name.to_owned()),
        ];

        // Addresses are announced explicitly, and `enable_addr_auto` is deliberately NOT
        // used. Two failures observed on this machine while building this:
        //   1. addr_auto alone announced a pile of link-local `fe80::` addresses and no
        //      routable IPv4 at all — a record that looks healthy and that nothing can
        //      dial, because link-local needs a scope id that an mDNS record cannot carry.
        //   2. addr_auto *overrides* explicitly supplied addresses, and on a machine with
        //      container bridges it chose a Docker bridge (192.168.155.0) over the real
        //      LAN address (192.168.2.69).
        // Announcing exactly what the routing table says is reachable is both correct and
        // the only version that a peer can act on.
        let routable = primary_local_address();
        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &format!("{instance}.local."),
            routable.as_slice(),
            port,
            &properties[..],
        )
        .map_err(|e| Error::Discovery(e.to_string()))?;

        let fullname = service.get_fullname().to_owned();
        self.daemon.register(service).map_err(|e| Error::Discovery(e.to_string()))?;
        self.advertised = Some(fullname);
        Ok(())
    }

    /// Watch for peers. The returned stream yields events until dropped.
    pub fn browse(&self) -> Result<PeerStream, Error> {
        let receiver =
            self.daemon.browse(SERVICE_TYPE).map_err(|e| Error::Discovery(e.to_string()))?;
        Ok(PeerStream { receiver, own_instance: self.advertised.clone() })
    }

    pub fn shutdown(&self) {
        if let Some(fullname) = &self.advertised {
            let _ = self.daemon.unregister(fullname);
        }
        let _ = self.daemon.shutdown();
    }
}

/// A stream of discovery events.
pub struct PeerStream {
    receiver: mdns_sd::Receiver<ServiceEvent>,
    own_instance: Option<String>,
}

impl core::fmt::Debug for PeerStream {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PeerStream")
            .field("own_instance", &self.own_instance)
            .finish_non_exhaustive()
    }
}

impl PeerStream {
    /// Next event, or `None` once discovery has shut down.
    ///
    /// Skips this machine's own advertisement, so callers never have to filter themselves
    /// out — forgetting to is how a peer ends up trying to connect to itself.
    pub async fn next(&self) -> Option<DiscoveryEvent> {
        loop {
            let event = self.receiver.recv_async().await.ok()?;
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    if Some(info.get_fullname()) == self.own_instance.as_deref() {
                        continue;
                    }
                    return Some(DiscoveryEvent::Found(Box::new(resolve(&info))));
                }
                ServiceEvent::ServiceRemoved(_ty, fullname) => {
                    if Some(fullname.as_str()) == self.own_instance.as_deref() {
                        continue;
                    }
                    return Some(DiscoveryEvent::Lost { instance: fullname });
                }
                // SearchStarted / ServiceFound / SearchStopped are progress notifications
                // with no resolved addresses yet; nothing to report to a caller.
                _ => {}
            }
        }
    }
}

/// This machine's primary routable address.
///
/// Found by asking the OS which local address it *would* use to reach the network, via a
/// connectionless UDP socket. Nothing is sent — `connect` on a UDP socket only sets the
/// default destination, which makes the kernel run its routing table and pick a source
/// address. That is exactly the question we want answered, and it beats enumerating
/// interfaces and guessing which one matters.
fn primary_local_address() -> Vec<IpAddr> {
    let mut found = Vec::new();
    // Routing targets only; no packet is ever sent to them. Several per family, because a
    // machine with no default route still needs an answer: the first that the routing
    // table can resolve wins. (An earlier version probed a broadcast address, which
    // `connect` refuses outright — so IPv4 was silently never advertised.)
    for probe in ["8.8.8.8:9", "192.168.0.1:9", "10.0.0.1:9", "[2001:4860:4860::8888]:9"] {
        let Ok(target) = probe.parse::<SocketAddr>() else { continue };
        let bind = if target.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
        let Ok(socket) = UdpSocket::bind(bind) else { continue };
        if socket.connect(target).is_ok()
            && let Ok(local) = socket.local_addr()
            && is_routable(local.ip())
            && !found.contains(&local.ip())
        {
            found.push(local.ip());
        }
    }
    found
}

/// Whether an address is worth advertising or dialling.
///
/// Loopback is useless to a peer, and link-local needs a scope id that does not survive
/// the trip through an mDNS record.
#[must_use]
pub fn is_routable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified(),
        IpAddr::V6(v6) => {
            !v6.is_loopback()
                && !v6.is_unspecified()
                && !matches!(v6.segments()[0] & 0xffc0, 0xfe80)
        }
    }
}

fn resolve(info: &mdns_sd::ResolvedService) -> DiscoveredPeer {
    let port = info.get_port();
    // Drop addresses a peer could never actually reach, so the dialler does not spend its
    // timeout budget on a list of link-local entries.
    let mut addresses: Vec<SocketAddr> = info
        .get_addresses()
        .iter()
        .map(|scoped| SocketAddr::new(scoped.to_ip_addr(), port))
        .filter(|addr| is_routable(addr.ip()))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    // IPv4 first: on this fleet it is the path that works, and trying it first keeps the
    // common case fast rather than waiting out an IPv6 attempt.
    addresses.sort_by_key(|a| (a.is_ipv6(), a.to_string()));

    DiscoveredPeer {
        advertised_peer_id: info.get_property_val_str(PROP_PEER_ID).and_then(parse_peer_id_prefix),
        advertised_fingerprint: info
            .get_property_val_str(PROP_FINGERPRINT)
            .and_then(parse_fingerprint),
        advertised_protocol: info.get_property_val_str(PROP_PROTOCOL).and_then(|v| v.parse().ok()),
        name: info
            .get_property_val_str(PROP_NAME)
            .unwrap_or_else(|| info.get_fullname())
            .to_owned(),
        addresses,
        instance: info.get_fullname().to_owned(),
    }
}

/// The advertised id is the short display form, so it only pins the first 4 bytes.
/// That is enough to correlate a record with a known peer for display; the full identity
/// comes from the handshake.
fn parse_peer_id_prefix(text: &str) -> Option<PeerId> {
    let bytes = unhex(text)?;
    let mut id = [0u8; 16];
    if bytes.len() > id.len() {
        return None;
    }
    id[..bytes.len()].copy_from_slice(&bytes);
    Some(PeerId(id))
}

fn parse_fingerprint(text: &str) -> Option<Fingerprint> {
    let bytes = unhex(text)?;
    let raw: [u8; 32] = bytes.try_into().ok()?;
    Some(Fingerprint::from_bytes(raw))
}

fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;

    #[test]
    fn hex_roundtrips() {
        let id = Identity::generate().unwrap();
        let text = hex(id.fingerprint().as_bytes());
        assert_eq!(text.len(), 64);
        assert_eq!(unhex(&text).unwrap(), id.fingerprint().as_bytes());
    }

    #[test]
    fn malformed_hex_is_rejected_rather_than_guessed() {
        // These arrive from any device on the LAN, so they are untrusted input.
        for bad in ["abc", "zz", "ab cd", "0x1234", ""] {
            let parsed = unhex(bad);
            assert!(parsed.is_none() || bad.is_empty(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn an_advertised_fingerprint_roundtrips_but_is_only_a_claim() {
        let id = Identity::generate().unwrap();
        let advertised = parse_fingerprint(&hex(id.fingerprint().as_bytes())).unwrap();
        assert_eq!(advertised, id.fingerprint());

        let peer = DiscoveredPeer {
            advertised_peer_id: Some(id.peer_id()),
            advertised_fingerprint: Some(advertised),
            name: "claims to be us".into(),
            addresses: vec![],
            advertised_protocol: Some(seam_proto::PROTOCOL_VERSION),
            instance: "seam-x._seam._udp.local.".into(),
        };
        // It matches — and that still grants nothing. Trust comes from `Link::authorize`.
        assert!(peer.claims_to_be(id.fingerprint()));
        assert!(!peer.claims_to_be(Identity::generate().unwrap().fingerprint()));
    }

    #[test]
    fn a_fingerprint_of_the_wrong_length_is_rejected() {
        assert!(parse_fingerprint(&hex(&[0u8; 31])).is_none());
        assert!(parse_fingerprint(&hex(&[0u8; 33])).is_none());
        assert!(parse_fingerprint(&hex(&[0u8; 32])).is_some());
    }

    #[test]
    fn link_local_and_loopback_are_not_advertised_or_dialled() {
        // Observed for real: the first working build advertised only `fe80::` addresses,
        // so the record looked healthy and no peer could ever connect.
        assert!(!is_routable("127.0.0.1".parse().unwrap()));
        assert!(!is_routable("::1".parse().unwrap()));
        assert!(!is_routable("169.254.10.1".parse().unwrap()));
        assert!(!is_routable("fe80::1".parse().unwrap()));
        assert!(!is_routable("0.0.0.0".parse().unwrap()));

        assert!(is_routable("192.168.2.69".parse().unwrap()));
        assert!(is_routable("10.0.0.5".parse().unwrap()));
        assert!(is_routable("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn this_machine_has_a_routable_address_to_advertise() {
        let addresses = primary_local_address();
        assert!(!addresses.is_empty(), "no routable address found to advertise");
        assert!(addresses.iter().copied().all(is_routable));
        // Regression: an earlier probe used a broadcast address, which `connect` refuses,
        // so IPv4 was silently absent from every advertisement and no peer could dial in.
        assert!(
            addresses.iter().any(std::net::IpAddr::is_ipv4),
            "a machine on an IPv4 LAN must advertise its IPv4 address, got {addresses:?}"
        );
    }

    #[test]
    fn display_name_falls_back_without_asking() {
        // Goal Z2: an unreadable hostname must never turn into a prompt.
        let name = Discovery::default_display_name(PeerId([0xAB; 16]));
        assert!(!name.is_empty());
    }
}
