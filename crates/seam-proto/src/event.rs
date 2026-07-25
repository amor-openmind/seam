//! Input events, and the drift-free motion model.

use crate::{
    Error,
    keys::{LogicalText, Modifiers, PhysicalKey},
    wire::{Reader, Writer},
};

/// Fractional bits in a [`Point`] coordinate. Coordinates are fixed-point 1/256 px.
///
/// A 1000 Hz mouse moving slowly emits sub-pixel motion every poll. Rounding each
/// packet to whole pixels quantises that away and the pointer feels notchy; carrying
/// 8 fractional bits preserves it without the reproducibility problems of floats.
pub const SUBPIXEL_BITS: u32 = 8;

/// One subpixel unit.
pub const SUBPIXEL_ONE: i32 = 1 << SUBPIXEL_BITS;

/// A position in fixed-point 1/256 px, in the target screen's coordinate space.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    #[must_use]
    pub const fn from_px(x: i32, y: i32) -> Self {
        Self { x: x << SUBPIXEL_BITS, y: y << SUBPIXEL_BITS }
    }

    #[must_use]
    pub const fn to_px(self) -> (i32, i32) {
        (self.x >> SUBPIXEL_BITS, self.y >> SUBPIXEL_BITS)
    }

    pub(crate) fn encode(self, w: &mut Writer<'_>) {
        w.i32(self.x);
        w.i32(self.y);
    }

    pub(crate) fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self { x: r.i32()?, y: r.i32()? })
    }
}

/// Pointer motion.
///
/// # Why this carries cumulative values instead of deltas
///
/// Motion rides unreliable QUIC datagrams, because a dropped motion packet must never
/// delay the ones behind it. That is only safe if losing a packet cannot corrupt state.
///
/// A *delta* encoding fails this: one lost packet silently offsets the pointer by that
/// delta **forever**. Existing tools paper over this with periodic resyncs, which is
/// why their pointers visibly jump.
///
/// Both fields here are therefore cumulative and self-correcting:
/// - `cursor` is an absolute position, so the newest packet is always authoritative;
/// - `travel` is a running total of raw device counters, so a receiver recovers the
///   true movement with one subtraction no matter how many packets it missed.
///
/// Loss costs a skipped intermediate sample. It never costs accuracy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Motion {
    /// Wrapping sequence number, used to discard reordered datagrams.
    pub seq: u32,
    /// Absolute pointer position on the target screen.
    pub cursor: Point,
    /// Cumulative raw horizontal device counter since session start. Wraps.
    pub travel_x: i32,
    /// Cumulative raw vertical device counter since session start. Wraps.
    pub travel_y: i32,
}

/// Motion after a receiver has reconciled it against what it saw last.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AppliedMotion {
    /// Where to place the pointer in absolute (pointer-warp) mode.
    pub cursor: Point,
    /// Raw movement since the last accepted packet, for relative (pointer-locked) mode.
    /// Already accounts for any packets that were lost in between.
    pub delta_x: i32,
    pub delta_y: i32,
    /// How many packets were lost immediately before this one. Diagnostics only —
    /// correctness does not depend on it.
    pub lost: u32,
}

/// Receiver-side motion state. Turns a stream of lossy, possibly reordered [`Motion`]
/// datagrams into correct pointer updates.
#[derive(Clone, Copy, Debug, Default)]
pub struct MotionTracker {
    last: Option<Motion>,
}

impl MotionTracker {
    #[must_use]
    pub const fn new() -> Self {
        Self { last: None }
    }

    /// Forget history, e.g. after re-entering this screen or reconnecting.
    ///
    /// The next packet then reports a zero delta rather than a huge bogus one relative
    /// to a stale counter from a previous session.
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// Accept a datagram, or return `None` if it is stale (arrived out of order).
    pub fn accept(&mut self, m: Motion) -> Option<AppliedMotion> {
        let applied = match self.last {
            // First packet of a session: absolute position is known, relative movement
            // is not yet meaningful.
            None => AppliedMotion { cursor: m.cursor, delta_x: 0, delta_y: 0, lost: 0 },
            Some(prev) => {
                // Wrapping-aware ordering: treat the gap as signed so the comparison
                // stays correct across the u32 boundary.
                let gap = m.seq.wrapping_sub(prev.seq).cast_signed();
                if gap <= 0 {
                    // Reordered or duplicated. Applying it would move the pointer
                    // backwards, which is exactly the jitter we are eliminating.
                    return None;
                }
                AppliedMotion {
                    cursor: m.cursor,
                    // Wrapping subtraction of cumulative counters: correct regardless
                    // of how many packets were lost, and across counter wraparound.
                    delta_x: m.travel_x.wrapping_sub(prev.travel_x),
                    delta_y: m.travel_y.wrapping_sub(prev.travel_y),
                    lost: gap.cast_unsigned() - 1,
                }
            }
        };
        self.last = Some(m);
        Some(applied)
    }
}

/// A pointer button. Values follow the Linux evdev ordering, which every backend maps to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Button {
    Left = 1,
    Right = 2,
    Middle = 3,
    /// "Back" / side button 1.
    Back = 4,
    /// "Forward" / side button 2.
    Forward = 5,
    /// Any further button, carried by index.
    Extra(u8),
}

impl Button {
    /// Map a platform backend's button index onto the protocol's numbering.
    ///
    /// Rejects 0, which every backend uses to mean "no button" — silently turning that
    /// into a real click would be worse than dropping the event.
    pub const fn try_from_u8(v: u8) -> Result<Self, Error> {
        Self::from_u8(v)
    }

    /// The protocol's numbering, for a platform backend to map onto its own.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::Left => 1,
            Self::Right => 2,
            Self::Middle => 3,
            Self::Back => 4,
            Self::Forward => 5,
            Self::Extra(n) => n,
        }
    }

    pub(crate) const fn from_u8(v: u8) -> Result<Self, Error> {
        match v {
            1 => Ok(Self::Left),
            2 => Ok(Self::Right),
            3 => Ok(Self::Middle),
            4 => Ok(Self::Back),
            5 => Ok(Self::Forward),
            0 => Err(Error::UnknownVariant("Button")),
            n => Ok(Self::Extra(n)),
        }
    }
}

/// Whether a key or button went down or up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Press {
    Up = 0,
    Down = 1,
    /// An auto-repeat from holding the key. Distinguished from `Down` so a receiver can
    /// suppress it and let its own OS generate repeats at the local repeat rate.
    Repeat = 2,
}

impl Press {
    pub(crate) const fn from_u8(v: u8) -> Result<Self, Error> {
        match v {
            0 => Ok(Self::Up),
            1 => Ok(Self::Down),
            2 => Ok(Self::Repeat),
            _ => Err(Error::UnknownVariant("Press")),
        }
    }

    #[must_use]
    pub const fn is_down(self) -> bool {
        matches!(self, Self::Down | Self::Repeat)
    }
}

/// A pointer button event. Carried reliably — a lost button-up leaves a stuck drag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ButtonEvent {
    pub seq: u32,
    pub button: Button,
    pub press: Press,
    pub modifiers: Modifiers,
}

/// Unit of a scroll event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ScrollUnit {
    /// Traditional notched wheel, in 1/120 of a detent (matching Windows `WHEEL_DELTA`
    /// and the Linux high-resolution scroll axis).
    Detent = 0,
    /// Pixel-precise scrolling from a trackpad or free-spinning wheel.
    Pixel = 1,
}

impl ScrollUnit {
    pub(crate) const fn from_u8(v: u8) -> Result<Self, Error> {
        match v {
            0 => Ok(Self::Detent),
            1 => Ok(Self::Pixel),
            _ => Err(Error::UnknownVariant("ScrollUnit")),
        }
    }
}

/// A scroll event.
///
/// Reliable, not datagram: scroll is *incremental* and has no absolute reference to
/// resynchronise against, so a dropped packet is permanently lost scrolling.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScrollEvent {
    pub seq: u32,
    pub dx: i32,
    pub dy: i32,
    pub unit: ScrollUnit,
    /// Set on the final event of a trackpad fling, so the receiver can end momentum.
    pub end_of_gesture: bool,
}

/// A keyboard event carrying both identities of the key. See [`crate::keys`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyEvent {
    pub seq: u32,
    /// Layout-independent physical key (USB HID usage).
    pub physical: PhysicalKey,
    /// Text the sender's layout produced. Empty for non-text keys.
    pub logical: LogicalText,
    pub press: Press,
    pub modifiers: Modifiers,
}

impl KeyEvent {
    pub(crate) fn encode(&self, w: &mut Writer<'_>) {
        w.u32(self.seq);
        w.u16(self.physical.0);
        self.logical.encode(w);
        w.u8(self.press as u8);
        w.u16(self.modifiers.0);
    }

    pub(crate) fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self {
            seq: r.u32()?,
            physical: PhysicalKey(r.u16()?),
            logical: LogicalText::decode(r)?,
            press: Press::from_u8(r.u8()?)?,
            modifiers: Modifiers(r.u16()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motion(seq: u32, tx: i32, ty: i32) -> Motion {
        Motion { seq, cursor: Point::from_px(tx, ty), travel_x: tx, travel_y: ty }
    }

    #[test]
    fn first_packet_has_no_relative_movement() {
        let mut t = MotionTracker::new();
        let a = t.accept(motion(7, 100, 50)).unwrap();
        assert_eq!((a.delta_x, a.delta_y), (0, 0));
        assert_eq!(a.cursor, Point::from_px(100, 50));
    }

    #[test]
    fn lost_packets_do_not_cause_drift() {
        let mut t = MotionTracker::new();
        t.accept(motion(1, 0, 0)).unwrap();
        // Packets 2..=9 are dropped by the network; only 10 arrives.
        let a = t.accept(motion(10, 500, -300)).unwrap();
        assert_eq!((a.delta_x, a.delta_y), (500, -300), "delta must cover the whole gap");
        assert_eq!(a.lost, 8);
    }

    #[test]
    fn reordered_packets_are_discarded() {
        let mut t = MotionTracker::new();
        t.accept(motion(1, 0, 0)).unwrap();
        t.accept(motion(5, 100, 100)).unwrap();
        assert!(t.accept(motion(3, 40, 40)).is_none(), "late packet must not rewind");
        assert!(t.accept(motion(5, 100, 100)).is_none(), "duplicate must be dropped");
    }

    #[test]
    fn sequence_and_travel_survive_wraparound() {
        let mut t = MotionTracker::new();
        t.accept(Motion {
            seq: u32::MAX - 1,
            cursor: Point::default(),
            travel_x: i32::MAX - 5,
            travel_y: 0,
        })
        .unwrap();
        let a = t
            .accept(Motion {
                seq: 1, // wrapped past u32::MAX
                cursor: Point::default(),
                travel_x: i32::MAX.wrapping_add(5), // wrapped past i32::MAX
                travel_y: 0,
            })
            .unwrap();
        assert_eq!(a.delta_x, 10, "wrapping arithmetic must yield the true movement");
    }

    #[test]
    fn reset_prevents_a_bogus_jump_after_reconnect() {
        let mut t = MotionTracker::new();
        t.accept(motion(1, 10_000, 10_000)).unwrap();
        t.reset();
        let a = t.accept(motion(2, 0, 0)).unwrap();
        assert_eq!((a.delta_x, a.delta_y), (0, 0));
    }

    #[test]
    fn subpixel_conversion_roundtrips() {
        assert_eq!(Point::from_px(-3, 7).to_px(), (-3, 7));
        assert_eq!(Point::from_px(1, 1).x, SUBPIXEL_ONE);
    }
}
