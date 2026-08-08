//! # seam-proto
//!
//! The seam wire protocol: types, codec and versioning.
//!
//! This crate is deliberately **zero-dependency** and **platform-free**. It defines the
//! bytes on the wire, so the encoding must be a property of this source tree and not of
//! a serialization library's release notes. It also means the protocol and all of the
//! correctness-critical state machines can be tested on any machine, with no OS
//! permissions and no network — which is where the majority of this project's bugs
//! would otherwise hide.
//!
//! ## What lives here
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`keys`] | Physical/logical key identity and the layout policy that makes mismatched keyboards work |
//! | [`event`] | Input events, including the drift-free motion model |
//! | [`state`] | Authoritative key state and reconciliation ("no stuck modifiers") |
//! | [`frame`] | The frame enum and its codec |
//! | [`wire`] | Big-endian read/write primitives |
//!
//! See `docs/PROTOCOL.md` for the wire format and `docs/GOAL.md` for why each design
//! decision is the way it is.

#![forbid(unsafe_code)]

pub mod event;
pub mod frame;
pub mod keys;
pub mod state;
pub mod wire;

pub use event::{
    AppliedMotion, Button, ButtonEvent, KeyEvent, Motion, MotionTracker, Point, Press,
    SUBPIXEL_BITS, SUBPIXEL_ONE, ScrollEvent, ScrollUnit,
};
pub use frame::{Edge, Enter, Frame, Hello, HelloAck, MAX_FRAME_LEN, PeerId};
pub use keys::{LayoutPolicy, LogicalText, Modifiers, PhysicalKey, Replay, resolve_replay};
pub use state::{KeyState, Reconciliation};

/// Wire protocol version.
///
/// Bumped on any incompatible change to the frame encoding. Peers exchange this in
/// [`Hello`] and refuse to proceed on mismatch — a version check that only warns is how
/// you get silent misinterpretation of key events, which is far worse than not connecting.
pub const PROTOCOL_VERSION: u16 = 2;

/// Magic bytes opening a control stream, so a wrong-protocol peer fails immediately
/// and legibly rather than at the first malformed frame.
pub const MAGIC: [u8; 4] = *b"SEAM";

/// Default UDP port. Distinct from Barrier/Synergy's 24800 so seam can run alongside
/// an existing installation during migration.
pub const DEFAULT_PORT: u16 = 24810;

/// A protocol encode/decode error.
///
/// Decoding is a trust boundary. Every variant here is a *rejection*, never a partial
/// or best-guess result: silently accepting a malformed frame would mean injecting a
/// keystroke nobody typed.
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Input ended mid-field.
    Truncated,
    /// Bytes remained after the frame was fully decoded.
    TrailingBytes(usize),
    /// A length-prefixed field exceeded its permitted size.
    TooLong,
    /// A string field was not valid UTF-8.
    InvalidUtf8,
    /// An enum discriminant had no meaning in this protocol version.
    UnknownVariant(&'static str),
    /// The frame kind byte is not recognised.
    UnknownFrameKind(u8),
    /// The peer speaks a different protocol version.
    VersionMismatch { ours: u16, theirs: u16 },
    /// A control stream did not begin with [`MAGIC`].
    BadMagic,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => f.write_str("frame ended unexpectedly"),
            Self::TrailingBytes(n) => write!(f, "{n} unexpected trailing byte(s) after frame"),
            Self::TooLong => f.write_str("length-prefixed field exceeds its limit"),
            Self::InvalidUtf8 => f.write_str("string field is not valid UTF-8"),
            Self::UnknownVariant(ty) => write!(f, "unknown discriminant for {ty}"),
            Self::UnknownFrameKind(k) => write!(f, "unknown frame kind {k:#04x}"),
            Self::VersionMismatch { ours, theirs } => {
                write!(f, "protocol version mismatch: we speak {ours}, peer speaks {theirs}")
            }
            Self::BadMagic => f.write_str("stream did not begin with the seam magic bytes"),
        }
    }
}

impl core::error::Error for Error {}
