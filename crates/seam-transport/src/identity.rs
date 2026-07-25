//! Peer identity: keys, fingerprints, and the trust store.
//!
//! # Identity is derived from the key, not assigned
//!
//! A peer's [`PeerId`] is the first 16 bytes of the SHA-256 of its public key (SPKI).
//! Nothing generates or stores an identifier separately, which has three consequences
//! that matter:
//!
//! - **It cannot be forged.** Claiming another peer's id requires their private key.
//!   Compare CVE-2021-42073, where any Barrier client sending the default label
//!   `"Unnamed"` could join a session and read the server's keystrokes.
//! - **It cannot drift.** There is no id file to lose, no hostname to match, no screen
//!   name in a config that must equal a `--name` flag. Barrier's name-mismatch rejection
//!   is both its most common first-connection failure and — on this machine — the code
//!   path that segfaults.
//! - **There is nothing to configure.** Goal Z1/Z2: the identity is detected, not asked.

use std::collections::BTreeMap;

use rcgen::PublicKeyData;
use seam_proto::PeerId;
use sha2::{Digest, Sha256};

use crate::Error;

/// SHA-256 of a peer's DER-encoded `SubjectPublicKeyInfo`.
///
/// This is the value pinned after pairing, and the value both sides feed into the
/// pairing code. Comparing full fingerprints is a machine's job; humans compare the
/// 6-digit code in [`crate::pairing`] instead.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// Hash a DER-encoded `SubjectPublicKeyInfo`.
    #[must_use]
    pub fn of_spki(spki_der: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        // Domain separation: this hash is used as an identity, so it must never collide
        // with a hash of the same bytes computed for some other purpose.
        hasher.update(b"seam-peer-spki-v1");
        hasher.update(spki_der);
        Self(hasher.finalize().into())
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The [`PeerId`] this fingerprint yields.
    ///
    /// 128 bits of a SHA-256 preimage-resistant hash: finding a key pair that collides
    /// with a given peer's id is a ~2^64 birthday search, and doing so buys nothing on
    /// its own because the TLS handshake still requires the matching private key.
    #[must_use]
    pub fn peer_id(&self) -> PeerId {
        let mut id = [0u8; 16];
        id.copy_from_slice(&self.0[..16]);
        PeerId(id)
    }

    /// Lowercase hex, grouped in fours for the rare case a human must read it
    /// (a support log, a mismatch report). Not the pairing UX — see [`crate::pairing`].
    #[must_use]
    pub fn to_grouped_hex(&self) -> String {
        use core::fmt::Write as _;
        let mut out = String::with_capacity(32 * 2 + 15);
        for (i, byte) in self.0.iter().enumerate() {
            if i > 0 && i % 2 == 0 {
                out.push(' ');
            }
            // Writing into a String is infallible.
            let _ = write!(out, "{byte:02x}");
        }
        out
    }
}

impl core::fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Truncated: full fingerprints in logs are noise, and the prefix is enough to
        // correlate. `to_grouped_hex` is there when the whole value is genuinely wanted.
        write!(
            f,
            "Fingerprint({:02x}{:02x}{:02x}{:02x}…)",
            self.0[0], self.0[1], self.0[2], self.0[3]
        )
    }
}

/// This machine's long-lived key pair and self-signed certificate.
///
/// Generated once on first run and persisted. There is no CA, no enrolment and no
/// expiry to renew: peers authenticate each other by pinned public key, so a certificate
/// authority would add ceremony without adding any guarantee.
pub struct Identity {
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
    spki_der: Vec<u8>,
    fingerprint: Fingerprint,
}

impl Identity {
    /// Generate a fresh identity.
    pub fn generate() -> Result<Self, Error> {
        // The SAN is required for a well-formed certificate but is never validated:
        // verification is by pinned public key, not by name. A fixed placeholder makes
        // that explicit — a hostname here would imply a check that does not happen, and
        // would break the moment the machine is renamed (goal Z2).
        let certified = rcgen::generate_simple_self_signed(vec!["seam.invalid".to_owned()])
            .map_err(|e| Error::Identity(e.to_string()))?;

        let spki_der = certified.signing_key.subject_public_key_info();
        let fingerprint = Fingerprint::of_spki(&spki_der);

        Ok(Self {
            cert_der: certified.cert.der().to_vec(),
            key_der: certified.signing_key.serialize_der(),
            spki_der,
            fingerprint,
        })
    }

    /// Reconstruct from persisted DER bytes.
    pub fn from_der(cert_der: Vec<u8>, key_der: Vec<u8>) -> Result<Self, Error> {
        let key = rcgen::KeyPair::try_from(key_der.as_slice())
            .map_err(|e| Error::Identity(format!("stored private key is unusable: {e}")))?;
        let spki_der = key.subject_public_key_info();
        let fingerprint = Fingerprint::of_spki(&spki_der);
        Ok(Self { cert_der, key_der, spki_der, fingerprint })
    }

    #[must_use]
    pub fn certificate_der(&self) -> &[u8] {
        &self.cert_der
    }

    #[must_use]
    pub fn private_key_der(&self) -> &[u8] {
        &self.key_der
    }

    #[must_use]
    pub fn spki_der(&self) -> &[u8] {
        &self.spki_der
    }

    #[must_use]
    pub const fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    #[must_use]
    pub fn peer_id(&self) -> PeerId {
        self.fingerprint.peer_id()
    }
}

impl core::fmt::Debug for Identity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never derive Debug here: it would print the private key.
        f.debug_struct("Identity").field("fingerprint", &self.fingerprint).finish_non_exhaustive()
    }
}

/// A peer we have paired with.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TrustedPeer {
    pub fingerprint: Fingerprint,
    /// Last known display name. Cosmetic only — it is **never** used to identify or
    /// authorise a peer, so renaming a machine can never break a pairing (goal O4).
    pub name: String,
}

/// The set of peers this machine has paired with.
///
/// Pairing is the only way in. There is no "accept unknown peers" mode, no shared
/// password, and no unauthenticated fallback: every mode that exists is one users end up
/// running in, and this software forwards keystrokes.
#[derive(Clone, Default, Debug)]
pub struct TrustStore {
    peers: BTreeMap<PeerId, TrustedPeer>,
}

impl TrustStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a peer as trusted. Returns the entry it replaced, if any.
    pub fn trust(
        &mut self,
        fingerprint: Fingerprint,
        name: impl Into<String>,
    ) -> Option<TrustedPeer> {
        self.peers.insert(fingerprint.peer_id(), TrustedPeer { fingerprint, name: name.into() })
    }

    pub fn forget(&mut self, id: PeerId) -> Option<TrustedPeer> {
        self.peers.remove(&id)
    }

    #[must_use]
    pub fn get(&self, id: PeerId) -> Option<&TrustedPeer> {
        self.peers.get(&id)
    }

    /// Whether this exact public key is trusted.
    ///
    /// Checks the **fingerprint**, not just the id. A peer presenting a known id with a
    /// different key is an impostor, and must be rejected rather than silently re-pinned.
    #[must_use]
    pub fn is_trusted(&self, fingerprint: Fingerprint) -> bool {
        self.peers.get(&fingerprint.peer_id()).is_some_and(|p| p.fingerprint == fingerprint)
    }

    /// Classify a presented key. The three outcomes need different UX, and collapsing
    /// them is how tools end up accepting impostors.
    #[must_use]
    pub fn classify(&self, fingerprint: Fingerprint) -> Trust {
        match self.peers.get(&fingerprint.peer_id()) {
            None => Trust::Unknown,
            Some(p) if p.fingerprint == fingerprint => Trust::Trusted,
            Some(_) => Trust::Conflict,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&PeerId, &TrustedPeer)> {
        self.peers.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

/// The result of checking a presented key against the trust store.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trust {
    /// Paired, and the key matches. Connect silently.
    Trusted,
    /// Never seen. Offer pairing — this is the only path to `Trusted`.
    Unknown,
    /// **The id is known but the key does not match.** Either the peer reinstalled, or
    /// someone is impersonating it. Never auto-accept: require the user to explicitly
    /// forget the old pairing first.
    Conflict,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_identities_are_distinct() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.peer_id(), b.peer_id());
    }

    #[test]
    fn peer_id_is_derived_deterministically_from_the_key() {
        let id = Identity::generate().unwrap();
        // Same key in, same identity out — no stored id, nothing to drift.
        let reloaded =
            Identity::from_der(id.certificate_der().to_vec(), id.private_key_der().to_vec())
                .unwrap();
        assert_eq!(reloaded.fingerprint(), id.fingerprint());
        assert_eq!(reloaded.peer_id(), id.peer_id());
    }

    #[test]
    fn fingerprint_is_bound_to_the_public_key() {
        let a = Identity::generate().unwrap();
        assert_eq!(Fingerprint::of_spki(a.spki_der()), a.fingerprint());
        assert_ne!(Fingerprint::of_spki(b"not the key"), a.fingerprint());
    }

    #[test]
    fn peer_id_is_the_fingerprint_prefix() {
        let fp = Fingerprint::of_spki(b"example");
        assert_eq!(fp.peer_id().0, fp.as_bytes()[..16]);
    }

    #[test]
    fn trust_store_recognises_a_paired_peer() {
        let peer = Identity::generate().unwrap();
        let mut store = TrustStore::new();
        assert_eq!(store.classify(peer.fingerprint()), Trust::Unknown);

        store.trust(peer.fingerprint(), "Mac-mini");
        assert_eq!(store.classify(peer.fingerprint()), Trust::Trusted);
        assert!(store.is_trusted(peer.fingerprint()));
        assert_eq!(store.get(peer.peer_id()).unwrap().name, "Mac-mini");
    }

    #[test]
    fn renaming_a_machine_never_breaks_a_pairing() {
        // Goal O4: names are cosmetic. This is the failure Barrier turns into a
        // rejection — and the rejection path is the one that crashes.
        let peer = Identity::generate().unwrap();
        let mut store = TrustStore::new();
        store.trust(peer.fingerprint(), "old-name");
        store.trust(peer.fingerprint(), "totally-different-name");
        assert!(store.is_trusted(peer.fingerprint()));
        assert_eq!(store.len(), 1, "a rename must not create a second entry");
    }

    #[test]
    fn a_different_key_claiming_a_known_id_is_a_conflict_not_a_silent_repin() {
        let real = Identity::generate().unwrap();
        let mut store = TrustStore::new();
        store.trust(real.fingerprint(), "peer");

        // Forge a fingerprint that shares the id prefix but differs in the tail — the
        // shape an impersonation attempt would take.
        let mut forged_bytes = *real.fingerprint().as_bytes();
        forged_bytes[31] ^= 0xFF;
        let forged = Fingerprint(forged_bytes);

        assert_eq!(forged.peer_id(), real.peer_id(), "test setup: ids must collide");
        assert_ne!(forged, real.fingerprint());
        assert_eq!(store.classify(forged), Trust::Conflict);
        assert!(!store.is_trusted(forged), "an impostor must never be trusted");
    }

    #[test]
    fn forgetting_a_peer_revokes_it() {
        let peer = Identity::generate().unwrap();
        let mut store = TrustStore::new();
        store.trust(peer.fingerprint(), "peer");
        assert!(store.forget(peer.peer_id()).is_some());
        assert_eq!(store.classify(peer.fingerprint()), Trust::Unknown);
        assert!(store.is_empty());
    }

    #[test]
    fn debug_never_leaks_the_private_key() {
        let id = Identity::generate().unwrap();
        let rendered = format!("{id:?}");
        let key_hex = id.private_key_der().iter().fold(String::new(), |mut acc, b| {
            use core::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        });
        assert!(!rendered.contains(&key_hex));
        assert!(rendered.contains("Fingerprint"));
    }

    #[test]
    fn grouped_hex_is_readable_and_complete() {
        let fp = Fingerprint::of_spki(b"example");
        let text = fp.to_grouped_hex();
        assert_eq!(text.replace(' ', "").len(), 64);
        assert_eq!(text.split(' ').count(), 16);
    }
}
