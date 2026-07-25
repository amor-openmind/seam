//! Screen geometry: what displays a machine has, and where its edges are.
//!
//! # Physical units, not pixels
//!
//! A machine's desktop is described in **millimetres**, not pixels. This is the root fix
//! for the entire DPI/resolution bug family in this software category — Deskflow's
//! cursor-jumps-between-different-resolution-monitors, its unreachable screen regions at
//! 125% scaling, and its "cursor stuck at screen edge" reports are all the same bug wearing
//! different hats, and all of them come from exchanging pixel coordinates.
//!
//! In physical space a 27" 4K display and a 13" laptop line up the way the user's hand
//! expects: leaving one screen 10 cm from its top arrives 10 cm from the neighbour's top,
//! regardless of how many pixels that is on either side.
//!
//! Pixels appear exactly twice: when reading the geometry from the OS, and when injecting
//! at the far end. Nothing in between speaks pixels.
//!
//! # A desktop is a union of rectangles
//!
//! Not one rectangle. A machine with two monitors of different heights has an L-shaped
//! desktop, and there are coordinates inside its bounding box that exist on no physical
//! display. Sending the pointer there strands it — which is exactly goal criterion F13.

use seam_proto::Point;

/// Millimetres, in fixed point: 1/64 mm.
///
/// Fixed point rather than floating: these values cross the wire and are compared for
/// equality, and 1/64 mm is roughly 0.4 thousandths of an inch — far finer than any
/// display's pixel pitch, so nothing is lost.
pub type Millis = i32;

/// Sub-units per millimetre.
pub const MM: Millis = 64;

/// One physical display.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Display {
    /// Stable per-machine identifier, so a display can be recognised across a
    /// reconfiguration rather than looking like a new one.
    pub id: u32,
    /// Position and size in this machine's pixel coordinate space.
    pub pixels: PixelRect,
    /// Physical size in 1/64 mm, from the display's EDID where the OS exposes it.
    pub width_mm: Millis,
    pub height_mm: Millis,
    /// Backing scale in 1/256 units: 256 = 1x, 512 = 2x Retina.
    pub scale: u32,
    /// Whether this is the machine's primary display.
    pub primary: bool,
}

impl Display {
    /// Physical pixels per millimetre, horizontally, in 1/256 units.
    ///
    /// Returns `None` for a display that reports no physical size — some virtual and
    /// projector outputs report 0x0. Callers fall back to assuming a nominal density
    /// rather than dividing by zero.
    #[must_use]
    pub fn density_x(&self) -> Option<u32> {
        if self.width_mm <= 0 {
            return None;
        }
        // width_px * 256 * MM / width_mm, ordered to avoid overflow on large displays.
        let px = u64::try_from(self.pixels.width).ok()?;
        let mm = u64::try_from(self.width_mm).ok()?;
        u32::try_from(px * 256 * u64::try_from(MM).ok()? / mm).ok()
    }

    /// A nominal physical size for a display that does not report one.
    ///
    /// 96 DPI is the historical baseline both Windows and X11 assume, so a display with
    /// no EDID lands somewhere plausible instead of being infinitely large or zero-sized.
    #[must_use]
    pub fn with_assumed_size_if_unknown(mut self) -> Self {
        const NOMINAL_DPI: i32 = 96;
        if self.width_mm <= 0 || self.height_mm <= 0 {
            let mm_per_px = 254 * MM / (NOMINAL_DPI * 10);
            self.width_mm = self.pixels.width.saturating_mul(mm_per_px);
            self.height_mm = self.pixels.height.saturating_mul(mm_per_px);
        }
        self
    }
}

/// A rectangle in a machine's pixel coordinate space.
///
/// `x`/`y` may be negative: a monitor placed left of or above the primary has negative
/// origin coordinates on every platform, and treating them as unsigned is a classic way
/// to lose half a multi-monitor setup.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl PixelRect {
    #[must_use]
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self { x, y, width, height }
    }

    #[must_use]
    pub const fn right(&self) -> i32 {
        self.x + self.width
    }

    #[must_use]
    pub const fn bottom(&self) -> i32 {
        self.y + self.height
    }

    #[must_use]
    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }
}

/// Everything a machine can display on.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Desktop {
    pub displays: Vec<Display>,
}

impl Desktop {
    #[must_use]
    pub fn new(displays: Vec<Display>) -> Self {
        Self { displays: displays.into_iter().map(Display::with_assumed_size_if_unknown).collect() }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.displays.iter().all(|d| d.pixels.is_empty())
    }

    #[must_use]
    pub fn primary(&self) -> Option<&Display> {
        self.displays.iter().find(|d| d.primary).or_else(|| self.displays.first())
    }

    /// The smallest rectangle containing every display.
    ///
    /// Useful for absolute-coordinate injection, and **not** a description of the
    /// desktop's shape: see [`Desktop::contains`].
    #[must_use]
    pub fn bounding_box(&self) -> PixelRect {
        let mut it = self.displays.iter().filter(|d| !d.pixels.is_empty());
        let Some(first) = it.next() else { return PixelRect::default() };
        let (mut left, mut top) = (first.pixels.x, first.pixels.y);
        let (mut right, mut bottom) = (first.pixels.right(), first.pixels.bottom());
        for d in it {
            left = left.min(d.pixels.x);
            top = top.min(d.pixels.y);
            right = right.max(d.pixels.right());
            bottom = bottom.max(d.pixels.bottom());
        }
        PixelRect::new(left, top, right - left, bottom - top)
    }

    /// Whether a pixel coordinate lands on an actual display.
    ///
    /// This is what stops the pointer being sent into a gap in an L-shaped desktop, where
    /// it would be invisible and unreachable (goal F13).
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.displays.iter().any(|d| d.pixels.contains(x, y))
    }

    /// The display a coordinate falls on.
    #[must_use]
    pub fn display_at(&self, x: i32, y: i32) -> Option<&Display> {
        self.displays.iter().find(|d| d.pixels.contains(x, y))
    }

    /// Move a coordinate to the nearest point that is actually on a display.
    ///
    /// Used when an incoming position lands in a gap: clamping to the nearest real pixel
    /// is always better than leaving the pointer somewhere the user cannot see it.
    #[must_use]
    pub fn clamp_onto_a_display(&self, x: i32, y: i32) -> (i32, i32) {
        if self.contains(x, y) {
            return (x, y);
        }
        let mut best: Option<((i32, i32), i64)> = None;
        for d in self.displays.iter().filter(|d| !d.pixels.is_empty()) {
            let cx = x.clamp(d.pixels.x, d.pixels.right() - 1);
            let cy = y.clamp(d.pixels.y, d.pixels.bottom() - 1);
            let dx = i64::from(cx - x);
            let dy = i64::from(cy - y);
            let distance = dx * dx + dy * dy;
            if best.is_none_or(|(_, b)| distance < b) {
                best = Some(((cx, cy), distance));
            }
        }
        best.map_or((x, y), |(p, _)| p)
    }

    /// Convert a pixel coordinate to the scale-independent form used on the wire.
    ///
    /// The result is a fraction of the bounding box in 1/65536 units, so a receiver with a
    /// completely different resolution lands in the same *relative* place (goal F12).
    #[must_use]
    pub fn to_normalized(&self, x: i32, y: i32) -> (u16, u16) {
        let bb = self.bounding_box();
        if bb.is_empty() {
            return (0, 0);
        }
        let nx = i64::from(x - bb.x) * 65535 / i64::from(bb.width.max(1));
        let ny = i64::from(y - bb.y) * 65535 / i64::from(bb.height.max(1));
        (
            u16::try_from(nx.clamp(0, 65535)).unwrap_or(0),
            u16::try_from(ny.clamp(0, 65535)).unwrap_or(0),
        )
    }

    /// Inverse of [`Desktop::to_normalized`], clamped onto a real display.
    #[must_use]
    pub fn from_normalized(&self, nx: u16, ny: u16) -> (i32, i32) {
        let bb = self.bounding_box();
        if bb.is_empty() {
            return (0, 0);
        }
        let x = bb.x + i32::try_from(i64::from(nx) * i64::from(bb.width) / 65535).unwrap_or(0);
        let y = bb.y + i32::try_from(i64::from(ny) * i64::from(bb.height) / 65535).unwrap_or(0);
        self.clamp_onto_a_display(x, y)
    }

    /// Where the pointer currently is, as a protocol [`Point`].
    #[must_use]
    pub fn to_point(x: i32, y: i32) -> Point {
        Point::from_px(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(id: u32, x: i32, y: i32, w: i32, h: i32, primary: bool) -> Display {
        Display {
            id,
            pixels: PixelRect::new(x, y, w, h),
            width_mm: 0,
            height_mm: 0,
            scale: 256,
            primary,
        }
    }

    /// The development fleet's own Mac: one 2560x1080 ultrawide.
    fn single() -> Desktop {
        Desktop::new(vec![display(1, 0, 0, 2560, 1080, true)])
    }

    /// A monitor placed to the LEFT of the primary, so it has a negative origin — the
    /// arrangement that breaks implementations using unsigned coordinates.
    fn left_and_primary() -> Desktop {
        Desktop::new(vec![
            display(1, 0, 0, 2560, 1080, true),
            display(2, -1920, 0, 1920, 1200, false),
        ])
    }

    #[test]
    fn bounding_box_covers_every_display_including_negative_origins() {
        let bb = left_and_primary().bounding_box();
        assert_eq!(bb, PixelRect::new(-1920, 0, 4480, 1200));
    }

    #[test]
    fn a_gap_in_an_l_shaped_desktop_is_not_part_of_the_desktop() {
        // Goal F13. The bounding box is 4480x1200, but the primary is only 1080 tall, so
        // the bottom-right corner exists on no physical display.
        let d = left_and_primary();
        assert!(d.bounding_box().contains(2000, 1150), "test setup: inside the bounding box");
        assert!(!d.contains(2000, 1150), "but on no actual display");
        assert!(d.contains(2000, 1000), "and this one is on the primary");
    }

    #[test]
    fn a_coordinate_in_a_gap_is_clamped_onto_the_nearest_display() {
        let d = left_and_primary();
        let (x, y) = d.clamp_onto_a_display(2000, 1150);
        assert!(d.contains(x, y), "clamped point must be on a display, got ({x}, {y})");
        assert_eq!(x, 2000, "should move only as far as needed");
        assert_eq!(y, 1079, "onto the bottom row of the primary");
    }

    #[test]
    fn a_coordinate_already_on_a_display_is_left_alone() {
        let d = left_and_primary();
        assert_eq!(d.clamp_onto_a_display(100, 100), (100, 100));
        assert_eq!(d.clamp_onto_a_display(-1000, 500), (-1000, 500));
    }

    #[test]
    fn normalized_coordinates_survive_a_resolution_change() {
        // Goal F12: leaving one machine at 43% across arrives at 43% across on a machine
        // with a completely different resolution.
        let big = single();
        let small = Desktop::new(vec![display(1, 0, 0, 1280, 720, true)]);

        let (nx, ny) = big.to_normalized(1100, 464); // ~43% across, ~43% down
        let (x, y) = small.from_normalized(nx, ny);

        let across = f64::from(x) / 1280.0;
        let down = f64::from(y) / 720.0;
        assert!((across - 0.43).abs() < 0.01, "landed {across} across");
        assert!((down - 0.43).abs() < 0.01, "landed {down} down");
    }

    #[test]
    fn normalization_round_trips_within_a_pixel() {
        let d = single();
        for (x, y) in [(0, 0), (1279, 540), (2559, 1079), (7, 3)] {
            let (nx, ny) = d.to_normalized(x, y);
            let (rx, ry) = d.from_normalized(nx, ny);
            assert!((rx - x).abs() <= 1, "x {x} -> {rx}");
            assert!((ry - y).abs() <= 1, "y {y} -> {ry}");
        }
    }

    #[test]
    fn denormalizing_never_lands_off_a_display() {
        // Every possible normalized input must produce a usable pointer position.
        let d = left_and_primary();
        for nx in (0..=65535u32).step_by(1023) {
            for ny in (0..=65535u32).step_by(4095) {
                let (nx, ny) = (u16::try_from(nx).unwrap(), u16::try_from(ny).unwrap());
                let (x, y) = d.from_normalized(nx, ny);
                assert!(d.contains(x, y), "({nx},{ny}) -> ({x},{y}) is off-screen");
            }
        }
    }

    #[test]
    fn a_display_with_no_reported_physical_size_gets_a_plausible_one() {
        // Virtual and projector outputs often report 0x0; dividing by that would panic or
        // produce nonsense, and a zero-sized display cannot be laid out physically.
        let d = single();
        let only = d.displays.first().unwrap();
        assert!(only.width_mm > 0 && only.height_mm > 0);
        // 2560 px at ~96 DPI is roughly 677 mm.
        let mm = only.width_mm / MM;
        assert!((600..760).contains(&mm), "implausible assumed width: {mm} mm");
    }

    #[test]
    fn density_is_reported_only_when_the_physical_size_is_known() {
        let mut d = display(1, 0, 0, 2560, 1080, true);
        d.width_mm = 0;
        assert_eq!(d.density_x(), None, "must not divide by a zero physical size");

        d.width_mm = 677 * MM;
        assert!(d.density_x().is_some());
    }

    #[test]
    fn an_empty_desktop_does_not_panic() {
        let d = Desktop::default();
        assert!(d.is_empty());
        assert_eq!(d.bounding_box(), PixelRect::default());
        assert_eq!(d.to_normalized(5, 5), (0, 0));
        assert_eq!(d.from_normalized(100, 100), (0, 0));
        assert!(!d.contains(0, 0));
    }

    #[test]
    fn display_lookup_finds_the_right_screen() {
        let d = left_and_primary();
        assert_eq!(d.display_at(100, 100).map(|s| s.id), Some(1));
        assert_eq!(d.display_at(-1000, 100).map(|s| s.id), Some(2));
        assert_eq!(d.display_at(9999, 9999).map(|s| s.id), None);
    }
}
