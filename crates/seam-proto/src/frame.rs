//! The frame enum and its codec.
//!
//! # Framing
//!
//! Frames travel over QUIC, which supplies two carriers with different guarantees, and
//! each frame kind is assigned to the one that matches its failure mode:
//!
//! - **Datagrams (unreliable, unordered)** carry [`Frame::Motion`] only. Motion is
//!   self-correcting (see [`Motion`]), so loss costs a skipped sample and nothing else.
//!   Crucially, a dropped motion packet must never delay the ones behind it.
//! - **Reliable streams** carry everything else. A lost button-up is a stuck drag; a
//!   lost key-up is a stuck modifier; a lost scroll tick is scrolling that never
//!   happened. None of these can be resynchronised from a later packet, so all of them
//!   need delivery guarantees.
//!
//! Splitting them this way is the point of using QUIC: on a single TCP connection, a
//! retransmitted clipboard chunk head-of-line-blocks pointer motion behind it, which is
//! precisely the stutter users report in Synergy and Barrier.
//!
//! On a stream, each frame is prefixed with a `u32` big-endian length. Datagrams are
//! already message-framed by QUIC and carry the frame bare.

use crate::{
    Error, PROTOCOL_VERSION,
    event::{Button, ButtonEvent, KeyEvent, Motion, Point, Press, ScrollEvent, ScrollUnit},
    keys::{LayoutPolicy, Modifiers},
    state::KeyState,
    wire::{Reader, Writer},
};

/// Largest frame accepted from a peer.
///
/// A bound is required before any allocation is sized from peer-supplied data;
/// otherwise a single bad length prefix is a remote out-of-memory.
pub const MAX_FRAME_LEN: usize = 64 * 1024;

/// A stable, random identifier for a peer, so a machine keeps its identity across
/// hostname changes and DHCP leases.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerId(pub [u8; 16]);

impl PeerId {
    pub const NIL: Self = Self([0; 16]);

    #[must_use]
    pub const fn is_nil(&self) -> bool {
        u128::from_be_bytes(self.0) == 0
    }
}

impl core::fmt::Debug for PeerId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PeerId({:032x})", u128::from_be_bytes(self.0))
    }
}

impl core::fmt::Display for PeerId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Short form for logs; the full id is available via Debug.
        write!(f, "{:08x}", u32::from_be_bytes([self.0[0], self.0[1], self.0[2], self.0[3]]))
    }
}

/// Which edge of a screen the pointer crossed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Edge {
    Left = 0,
    Right = 1,
    Top = 2,
    Bottom = 3,
}

impl Edge {
    const fn from_u8(v: u8) -> Result<Self, Error> {
        match v {
            0 => Ok(Self::Left),
            1 => Ok(Self::Right),
            2 => Ok(Self::Top),
            3 => Ok(Self::Bottom),
            _ => Err(Error::UnknownVariant("Edge")),
        }
    }

    /// The edge the pointer arrives at, given the edge it departed from.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }
}

/// Opening frame, sent by both sides.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hello {
    pub version: u16,
    pub peer: PeerId,
    /// Human-readable screen name, as shown in the layout.
    pub name: String,
    /// Screen size in physical pixels.
    pub width: u32,
    pub height: u32,
    /// Backing scale in 1/256 units (256 = 1.0x, 512 = 2.0x Retina).
    pub scale: u32,
    /// How this peer wants key events reproduced *to it*. The receiver's preference
    /// wins, because it is the machine whose layout is in question.
    pub layout_policy: LayoutPolicy,
}

impl Hello {
    /// Reject a peer speaking a different protocol version.
    pub fn check_version(&self) -> Result<(), Error> {
        if self.version == PROTOCOL_VERSION {
            Ok(())
        } else {
            Err(Error::VersionMismatch { ours: PROTOCOL_VERSION, theirs: self.version })
        }
    }
}

/// Response to [`Hello`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HelloAck {
    pub version: u16,
    pub peer: PeerId,
    pub name: String,
    /// `false` when the peer is known but not paired; the reason is in `message`.
    pub accepted: bool,
    pub message: String,
}

/// Focus entering this screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Enter {
    pub seq: u32,
    /// Where to place the pointer on arrival.
    pub cursor: Point,
    /// The edge it arrives at.
    pub edge: Edge,
    /// The sender's authoritative key state at the moment of handover.
    ///
    /// Included so the receiver starts from a known state rather than inferring one.
    /// Crossing a screen edge mid-chord (holding Shift while dragging, a very common
    /// case) is the single most common way modifiers get stuck.
    pub keys: KeyState,
}

/// A protocol message.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Frame {
    Hello(Hello),
    HelloAck(HelloAck),

    /// Pointer motion. **Datagram carrier.**
    Motion(Motion),
    Button(ButtonEvent),
    Scroll(ScrollEvent),
    Key(KeyEvent),

    /// Focus enters the receiving screen.
    Enter(Enter),
    /// Focus leaves the receiving screen. The receiver releases every held key.
    Leave {
        seq: u32,
    },
    /// Unconditional "release everything". Sent on disconnect, sleep, and by the user's
    /// panic hotkey. Idempotent, so it is safe to send liberally.
    ReleaseAll {
        seq: u32,
    },

    /// Heartbeat carrying a fingerprint of the sender's held keys.
    KeyStateDigest {
        seq: u32,
        digest: u64,
        count: u16,
    },
    /// Receiver asks for the full state after a digest mismatch.
    KeyStateQuery {
        seq: u32,
    },
    /// Authoritative full key state, used to reconcile.
    KeyStateFull {
        seq: u32,
        state: KeyState,
    },

    /// Liveness probe. `t_send_us` is the sender's clock in microseconds.
    Ping {
        nonce: u64,
        t_send_us: u64,
    },
    /// Probe reply. Carries both timestamps so the initiator can derive round-trip time
    /// *and* clock offset from a single exchange, NTP-style.
    Pong {
        nonce: u64,
        t_send_us: u64,
        t_echo_us: u64,
    },
}

// Frame kind bytes. Stable across versions; gaps are reserved, never reused.
mod kind {
    pub(super) const HELLO: u8 = 0x01;
    pub(super) const HELLO_ACK: u8 = 0x02;

    pub(super) const MOTION: u8 = 0x10;
    pub(super) const BUTTON: u8 = 0x11;
    pub(super) const SCROLL: u8 = 0x12;
    pub(super) const KEY: u8 = 0x13;

    pub(super) const ENTER: u8 = 0x20;
    pub(super) const LEAVE: u8 = 0x21;
    pub(super) const RELEASE_ALL: u8 = 0x22;

    pub(super) const KEY_STATE_DIGEST: u8 = 0x30;
    pub(super) const KEY_STATE_QUERY: u8 = 0x31;
    pub(super) const KEY_STATE_FULL: u8 = 0x32;

    pub(super) const PING: u8 = 0x40;
    pub(super) const PONG: u8 = 0x41;

    // Reserved, assigned but not yet implemented:
    //   0x50..=0x5F  clipboard offer / request / data
    //   0x60..=0x6F  file transfer
}

impl Frame {
    /// Whether this frame may travel on an unreliable datagram.
    ///
    /// Only motion qualifies, because only motion can reconstruct itself from the next
    /// packet. Everything else would be silently lost.
    #[must_use]
    pub const fn is_datagram_safe(&self) -> bool {
        matches!(self, Self::Motion(_))
    }

    /// Encode into `buf`, appending. Reuse one `Vec` to keep the hot path allocation-free.
    pub fn encode(&self, buf: &mut Vec<u8>) -> Result<(), Error> {
        let mut w = Writer::new(buf);
        match self {
            Self::Hello(h) => {
                w.u8(kind::HELLO);
                w.u16(h.version);
                w.bytes(&h.peer.0);
                w.string(&h.name)?;
                w.u32(h.width);
                w.u32(h.height);
                w.u32(h.scale);
                w.u8(h.layout_policy as u8);
            }
            Self::HelloAck(h) => {
                w.u8(kind::HELLO_ACK);
                w.u16(h.version);
                w.bytes(&h.peer.0);
                w.string(&h.name)?;
                w.u8(u8::from(h.accepted));
                w.string(&h.message)?;
            }
            Self::Motion(m) => {
                w.u8(kind::MOTION);
                w.u32(m.seq);
                m.cursor.encode(&mut w);
                w.i32(m.travel_x);
                w.i32(m.travel_y);
            }
            Self::Button(b) => {
                w.u8(kind::BUTTON);
                w.u32(b.seq);
                w.u8(b.button.to_u8());
                w.u8(b.press as u8);
                w.u16(b.modifiers.0);
            }
            Self::Scroll(s) => {
                w.u8(kind::SCROLL);
                w.u32(s.seq);
                w.i32(s.dx);
                w.i32(s.dy);
                w.u8(s.unit as u8);
                w.u8(u8::from(s.end_of_gesture));
            }
            Self::Key(k) => {
                w.u8(kind::KEY);
                k.encode(&mut w);
            }
            Self::Enter(e) => {
                w.u8(kind::ENTER);
                w.u32(e.seq);
                e.cursor.encode(&mut w);
                w.u8(e.edge as u8);
                encode_key_state(&mut w, &e.keys);
            }
            Self::Leave { seq } => {
                w.u8(kind::LEAVE);
                w.u32(*seq);
            }
            Self::ReleaseAll { seq } => {
                w.u8(kind::RELEASE_ALL);
                w.u32(*seq);
            }
            Self::KeyStateDigest { seq, digest, count } => {
                w.u8(kind::KEY_STATE_DIGEST);
                w.u32(*seq);
                w.u64(*digest);
                w.u16(*count);
            }
            Self::KeyStateQuery { seq } => {
                w.u8(kind::KEY_STATE_QUERY);
                w.u32(*seq);
            }
            Self::KeyStateFull { seq, state } => {
                w.u8(kind::KEY_STATE_FULL);
                w.u32(*seq);
                encode_key_state(&mut w, state);
            }
            Self::Ping { nonce, t_send_us } => {
                w.u8(kind::PING);
                w.u64(*nonce);
                w.u64(*t_send_us);
            }
            Self::Pong { nonce, t_send_us, t_echo_us } => {
                w.u8(kind::PONG);
                w.u64(*nonce);
                w.u64(*t_send_us);
                w.u64(*t_echo_us);
            }
        }
        Ok(())
    }

    /// Decode exactly one frame from `buf`, rejecting trailing bytes.
    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf);
        let frame = Self::decode_body(&mut r)?;
        r.finish()?;
        Ok(frame)
    }

    fn decode_body(r: &mut Reader<'_>) -> Result<Self, Error> {
        let k = r.u8()?;
        Ok(match k {
            kind::HELLO => Self::Hello(Hello {
                version: r.u16()?,
                peer: PeerId(read_peer_id(r)?),
                name: r.string()?.to_owned(),
                width: r.u32()?,
                height: r.u32()?,
                scale: r.u32()?,
                layout_policy: LayoutPolicy::from_u8(r.u8()?)?,
            }),
            kind::HELLO_ACK => Self::HelloAck(HelloAck {
                version: r.u16()?,
                peer: PeerId(read_peer_id(r)?),
                name: r.string()?.to_owned(),
                accepted: r.u8()? != 0,
                message: r.string()?.to_owned(),
            }),
            kind::MOTION => Self::Motion(Motion {
                seq: r.u32()?,
                cursor: Point::decode(r)?,
                travel_x: r.i32()?,
                travel_y: r.i32()?,
            }),
            kind::BUTTON => Self::Button(ButtonEvent {
                seq: r.u32()?,
                button: Button::from_u8(r.u8()?)?,
                press: Press::from_u8(r.u8()?)?,
                modifiers: Modifiers(r.u16()?),
            }),
            kind::SCROLL => Self::Scroll(ScrollEvent {
                seq: r.u32()?,
                dx: r.i32()?,
                dy: r.i32()?,
                unit: ScrollUnit::from_u8(r.u8()?)?,
                end_of_gesture: r.u8()? != 0,
            }),
            kind::KEY => Self::Key(KeyEvent::decode(r)?),
            kind::ENTER => Self::Enter(Enter {
                seq: r.u32()?,
                cursor: Point::decode(r)?,
                edge: Edge::from_u8(r.u8()?)?,
                keys: decode_key_state(r)?,
            }),
            kind::LEAVE => Self::Leave { seq: r.u32()? },
            kind::RELEASE_ALL => Self::ReleaseAll { seq: r.u32()? },
            kind::KEY_STATE_DIGEST => {
                Self::KeyStateDigest { seq: r.u32()?, digest: r.u64()?, count: r.u16()? }
            }
            kind::KEY_STATE_QUERY => Self::KeyStateQuery { seq: r.u32()? },
            kind::KEY_STATE_FULL => {
                Self::KeyStateFull { seq: r.u32()?, state: decode_key_state(r)? }
            }
            kind::PING => Self::Ping { nonce: r.u64()?, t_send_us: r.u64()? },
            kind::PONG => Self::Pong { nonce: r.u64()?, t_send_us: r.u64()?, t_echo_us: r.u64()? },
            other => return Err(Error::UnknownFrameKind(other)),
        })
    }

    /// Encode with a `u32` big-endian length prefix, for a reliable stream.
    pub fn encode_framed(&self, buf: &mut Vec<u8>) -> Result<(), Error> {
        let start = buf.len();
        buf.extend_from_slice(&[0; 4]); // placeholder
        self.encode(buf)?;
        let len = buf.len() - start - 4;
        if len > MAX_FRAME_LEN {
            buf.truncate(start);
            return Err(Error::TooLong);
        }
        // Guarded above: `len <= MAX_FRAME_LEN`, so this conversion is exact.
        let len = u32::try_from(len).map_err(|_| Error::TooLong)?;
        buf[start..start + 4].copy_from_slice(&len.to_be_bytes());
        Ok(())
    }

    /// Decode one length-prefixed frame, returning it and the bytes consumed.
    ///
    /// Returns `Ok(None)` when `buf` does not yet hold a complete frame, so a stream
    /// reader can simply wait for more bytes.
    pub fn decode_framed(buf: &[u8]) -> Result<Option<(Self, usize)>, Error> {
        let Some(header) = buf.get(..4) else { return Ok(None) };
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        // Checked before any allocation is sized from it.
        if len > MAX_FRAME_LEN {
            return Err(Error::TooLong);
        }
        let Some(body) = buf.get(4..4 + len) else { return Ok(None) };
        Ok(Some((Self::decode(body)?, 4 + len)))
    }
}

fn read_peer_id(r: &mut Reader<'_>) -> Result<[u8; 16], Error> {
    let b = r.bytes(16)?;
    let mut out = [0u8; 16];
    out.copy_from_slice(b);
    Ok(out)
}

fn encode_key_state(w: &mut Writer<'_>, s: &KeyState) {
    for word in s.words() {
        w.u64(*word);
    }
    w.u16(s.modifiers().0);
}

fn decode_key_state(r: &mut Reader<'_>) -> Result<KeyState, Error> {
    let mut words = [0u64; 4];
    for word in &mut words {
        *word = r.u64()?;
    }
    Ok(KeyState::from_parts(words, Modifiers(r.u16()?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{LogicalText, PhysicalKey};

    /// One of every frame kind, for exhaustive round-trip coverage.
    fn sample_frames() -> Vec<Frame> {
        let mut keys = KeyState::new();
        keys.press(PhysicalKey::LEFT_SHIFT);
        keys.press(PhysicalKey::A);

        vec![
            Frame::Hello(Hello {
                version: PROTOCOL_VERSION,
                peer: PeerId([7; 16]),
                name: "Mac-mini".into(),
                width: 3840,
                height: 2160,
                scale: 512,
                layout_policy: LayoutPolicy::Auto,
            }),
            Frame::HelloAck(HelloAck {
                version: PROTOCOL_VERSION,
                peer: PeerId([9; 16]),
                name: "میز کار".into(),
                accepted: false,
                message: "not paired".into(),
            }),
            Frame::Motion(Motion {
                seq: u32::MAX,
                cursor: Point::from_px(-1920, 1080),
                travel_x: i32::MIN,
                travel_y: i32::MAX,
            }),
            Frame::Button(ButtonEvent {
                seq: 3,
                button: Button::Extra(9),
                press: Press::Down,
                modifiers: Modifiers::LEFT_CTRL,
            }),
            Frame::Scroll(ScrollEvent {
                seq: 4,
                dx: -120,
                dy: 360,
                unit: ScrollUnit::Pixel,
                end_of_gesture: true,
            }),
            Frame::Key(KeyEvent {
                seq: 5,
                physical: PhysicalKey::A,
                logical: LogicalText::new("ی").unwrap(),
                press: Press::Repeat,
                modifiers: Modifiers::RIGHT_ALT,
            }),
            Frame::Enter(Enter { seq: 6, cursor: Point::from_px(0, 540), edge: Edge::Left, keys }),
            Frame::Leave { seq: 7 },
            Frame::ReleaseAll { seq: 8 },
            Frame::KeyStateDigest { seq: 9, digest: 0xDEAD_BEEF_CAFE_F00D, count: 2 },
            Frame::KeyStateQuery { seq: 10 },
            Frame::KeyStateFull { seq: 11, state: keys },
            Frame::Ping { nonce: 0x1234, t_send_us: 1_700_000_000_000_000 },
            Frame::Pong { nonce: 0x1234, t_send_us: 1, t_echo_us: 2 },
        ]
    }

    #[test]
    fn every_frame_kind_roundtrips() {
        let mut buf = Vec::new();
        for frame in sample_frames() {
            buf.clear();
            frame.encode(&mut buf).expect("encode");
            let decoded = Frame::decode(&buf).expect("decode");
            assert_eq!(decoded, frame, "roundtrip failed for {frame:?}");
        }
    }

    #[test]
    fn length_framing_roundtrips_a_concatenated_stream() {
        let frames = sample_frames();
        let mut buf = Vec::new();
        for f in &frames {
            f.encode_framed(&mut buf).expect("encode_framed");
        }
        let mut rest = &buf[..];
        for expected in &frames {
            let (got, used) = Frame::decode_framed(rest).expect("decode").expect("complete");
            assert_eq!(&got, expected);
            rest = &rest[used..];
        }
        assert!(rest.is_empty());
    }

    #[test]
    fn partial_stream_reads_are_incomplete_not_errors() {
        let mut buf = Vec::new();
        Frame::Leave { seq: 1 }.encode_framed(&mut buf).unwrap();
        // Every strict prefix must report "need more data", never an error.
        for n in 0..buf.len() {
            assert_eq!(
                Frame::decode_framed(&buf[..n]),
                Ok(None),
                "prefix of length {n} should be incomplete"
            );
        }
        assert!(Frame::decode_framed(&buf).unwrap().is_some());
    }

    #[test]
    fn only_motion_is_datagram_safe() {
        for f in sample_frames() {
            let expected = matches!(f, Frame::Motion(_));
            assert_eq!(f.is_datagram_safe(), expected, "{f:?}");
        }
    }

    #[test]
    fn unknown_frame_kind_is_rejected() {
        assert_eq!(Frame::decode(&[0xFE]), Err(Error::UnknownFrameKind(0xFE)));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut buf = Vec::new();
        Frame::Leave { seq: 1 }.encode(&mut buf).unwrap();
        buf.push(0);
        assert_eq!(Frame::decode(&buf), Err(Error::TrailingBytes(1)));
    }

    #[test]
    fn oversized_length_prefix_is_rejected_before_allocating() {
        let mut buf = u32::MAX.to_be_bytes().to_vec();
        buf.extend_from_slice(&[0; 8]);
        assert_eq!(Frame::decode_framed(&buf), Err(Error::TooLong));
    }

    #[test]
    fn version_mismatch_is_refused() {
        let mut h = Hello {
            version: PROTOCOL_VERSION,
            peer: PeerId::NIL,
            name: "x".into(),
            width: 1,
            height: 1,
            scale: 256,
            layout_policy: LayoutPolicy::Auto,
        };
        assert!(h.check_version().is_ok());
        h.version = PROTOCOL_VERSION + 1;
        assert!(matches!(h.check_version(), Err(Error::VersionMismatch { .. })));
    }

    #[test]
    fn edges_are_opposite_in_pairs() {
        for e in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            assert_eq!(e.opposite().opposite(), e);
            assert_ne!(e.opposite(), e);
        }
    }

    /// Decoding is a trust boundary: arbitrary bytes must produce a `Result`, never a
    /// panic. This is the cheap always-on companion to the `cargo-fuzz` target.
    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut buf = Vec::with_capacity(64);
        for _ in 0..200_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let len = (state % 48) as usize;
            buf.clear();
            for i in 0..len {
                let byte = (state >> (i % 8 * 8)) ^ u64::try_from(i).unwrap_or(0);
                buf.push(byte.to_le_bytes()[0]);
            }
            let _ = Frame::decode(&buf);
            let _ = Frame::decode_framed(&buf);
        }
    }

    /// Corrupting any single byte of a valid frame must be rejected or decode to
    /// something well-formed — never panic.
    #[test]
    fn bit_flips_in_valid_frames_never_panic() {
        for frame in sample_frames() {
            let mut buf = Vec::new();
            frame.encode(&mut buf).unwrap();
            for i in 0..buf.len() {
                for bit in 0..8 {
                    let mut corrupt = buf.clone();
                    corrupt[i] ^= 1 << bit;
                    let _ = Frame::decode(&corrupt);
                }
            }
        }
    }
}
