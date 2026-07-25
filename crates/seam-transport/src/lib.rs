//! # seam-transport
//!
//! Peer identity, pairing, and the QUIC transport.
//!
//! ## Why QUIC
//!
//! Input has two kinds of message with opposite failure modes, and they need opposite
//! delivery guarantees:
//!
//! - **Pointer motion** must never be delayed. It is also self-correcting (see
//!   [`seam_proto::Motion`]), so a lost packet costs one skipped sample. It rides
//!   **unreliable datagrams**.
//! - **Key and button events** cannot be resynchronised from a later packet. A lost
//!   key-up is a stuck modifier; a lost button-up is a stuck drag. They ride a
//!   **reliable stream**.
//!
//! A single TCP connection cannot express both: a retransmitted clipboard chunk
//! head-of-line-blocks pointer motion behind it, which is the stutter users report in
//! Synergy and Barrier. Raw UDP cannot express the second half without reimplementing
//! retransmission, ordering and a crypto handshake — more work than using quinn, and the
//! part most likely to be wrong.
//!
//! QUIC also survives IP changes and wake-from-sleep without reconnecting, because it
//! identifies a connection by connection ID rather than by 4-tuple. Neither TCP nor
//! DTLS 1.2 (which lan-mouse uses) can do that.
//!
//! ## Zero configuration
//!
//! Nothing in this crate is a setting. Identity is derived from a generated key pair
//! ([`identity`]), trust is established by one confirmed 6-digit code ([`pairing`]), and
//! encryption is mandatory with no fallback. See goal Z1–Z7.

#![forbid(unsafe_code)]

pub mod endpoint;
pub mod identity;
pub mod pairing;
mod tls;

pub use endpoint::{Endpoint, Link};
pub use identity::{Fingerprint, Identity, Trust, TrustStore, TrustedPeer};
pub use pairing::{CODE_DIGITS, EXPORTER_LABEL, PairingCode};

/// An error from the transport layer.
///
/// Every variant is phrased so it can be shown to a user verbatim. Goal O5 requires that
/// a failure to connect names its cause — "connection failed" is exactly the message
/// that makes people retry blindly, which is the complaint this project exists to fix.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("could not create this machine's identity key: {0}")]
    Identity(String),

    #[error(
        "this peer is not paired with your machine yet — run pairing on both machines \
         and confirm the 6-digit code"
    )]
    NotPaired,

    #[error(
        "the pairing codes did not match ({ours} here, {theirs} on the peer) — someone \
         may be intercepting the connection, so pairing was refused"
    )]
    PairingCodeMismatch { ours: String, theirs: String },

    #[error(
        "this peer's identity has changed since you paired with it — if you reinstalled \
         seam there, remove the old pairing first; otherwise someone is impersonating it"
    )]
    IdentityConflict,

    #[error("the peer speaks a different version of the seam protocol: {0}")]
    Protocol(#[from] seam_proto::Error),

    #[error("could not set up the encrypted connection: {0}")]
    Tls(String),

    #[error("could not bind to {addr}: {reason}")]
    Bind { addr: std::net::SocketAddr, reason: String },

    #[error("could not reach the peer at {addr}: {reason}")]
    Connect { addr: std::net::SocketAddr, reason: String },

    #[error("the peer presented no certificate, so its identity could not be established")]
    NoPeerCertificate,

    #[error(
        "refused to send this frame as an unreliable datagram: only pointer motion can \
         recover from being dropped"
    )]
    NotDatagramSafe,

    #[error("could not send to the peer: {0}")]
    Send(String),

    #[error("could not receive from the peer: {0}")]
    Recv(String),
}
