//! Which machine currently owns the mouse and keyboard.
//!
//! This is the piece that turns mirroring into a KVM. Mirroring sends input to every peer
//! at once and the local machine keeps responding; a KVM sends input to **exactly one**
//! machine, and the others — including the local one — do not respond at all.
//!
//! # The model
//!
//! Machines are laid out side by side in one virtual strip. The local machine occupies
//! its own desktop rectangle; each peer occupies a slot beside it. A single virtual
//! pointer moves through that strip, and whichever slot it is inside owns the input.
//!
//! Crossing is decided from **relative movement**, never from the local cursor position.
//! Once focus leaves this machine the cursor is detached from the mouse, so its reported
//! position stops changing — a design that read the cursor position would immediately
//! believe the pointer had stopped and could never come back.
//!
//! # Keyboard follows the pointer
//!
//! There is one focus, and it decides where *all* input goes. Routing the keyboard
//! separately is how you end up typing a password into the wrong machine — see
//! `inputleap#2143`, where the mouse crossed to a Windows lock screen but the keyboard
//! silently stayed behind.

use seam_proto::PeerId;

/// Who currently owns input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Focus {
    /// This machine. Input is not forwarded anywhere.
    Local,
    /// A peer. Input goes only to it, and this machine must not act on it.
    Remote(PeerId),
}

/// A peer's slot in the virtual strip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Slot {
    pub peer: PeerId,
    /// Width of that machine's desktop, in its own pixels.
    pub width: i32,
    pub height: i32,
}

/// The virtual strip, and where the pointer is within it.
#[derive(Debug)]
pub(crate) struct Layout {
    /// This machine's desktop size.
    local_width: i32,
    local_height: i32,
    /// Peers to the right of this machine, in order.
    right: Vec<Slot>,
    /// The virtual pointer's x position across the whole strip. The local machine
    /// occupies `0..local_width`.
    x: i32,
    y: i32,
    focus: Focus,
}

impl Layout {
    pub(crate) fn new(local_width: i32, local_height: i32) -> Self {
        Self {
            local_width: local_width.max(1),
            local_height: local_height.max(1),
            right: Vec::new(),
            x: local_width / 2,
            y: local_height / 2,
            focus: Focus::Local,
        }
    }

    /// Place a peer to the right of everything placed so far.
    pub(crate) fn add_peer_right(&mut self, peer: PeerId, width: i32, height: i32) {
        if self.right.iter().any(|s| s.peer == peer) {
            return;
        }
        self.right.push(Slot { peer, width: width.max(1), height: height.max(1) });
    }

    pub(crate) fn forget_peer(&mut self, peer: PeerId) {
        self.right.retain(|s| s.peer != peer);
        if self.focus == Focus::Remote(peer) {
            // Never leave input pointed at a machine that is gone: that is precisely how
            // a pointer gets stranded (goal R2).
            self.return_home();
        }
    }

    #[must_use]
    pub(crate) const fn focus(&self) -> Focus {
        self.focus
    }

    /// Total width of the strip.
    fn total_width(&self) -> i32 {
        self.local_width + self.right.iter().map(|s| s.width).sum::<i32>()
    }

    /// Snap focus and the virtual pointer back to this machine.
    pub(crate) fn return_home(&mut self) {
        self.focus = Focus::Local;
        // Just inside the right edge, so the pointer appears where it left rather than
        // jumping to the middle of the screen.
        self.x = (self.local_width - 2).max(0);
    }

    /// Apply relative movement and report what changed.
    pub(crate) fn apply_motion(&mut self, dx: i32, dy: i32) -> Update {
        let before = self.focus;
        self.x = (self.x + dx).clamp(0, self.total_width() - 1);
        self.y = self.y.saturating_add(dy);

        // Which slot is the pointer in now?
        let mut edge = self.local_width;
        let mut now = Focus::Local;
        if self.x >= edge {
            for slot in &self.right {
                if self.x < edge + slot.width {
                    now = Focus::Remote(slot.peer);
                    break;
                }
                edge += slot.width;
            }
            // Past every slot: stay on the last one rather than falling off the end.
            if now == Focus::Local
                && let Some(last) = self.right.last()
            {
                now = Focus::Remote(last.peer);
            }
        }

        // Clamp vertically to whichever machine now owns the pointer.
        let height = match now {
            Focus::Local => self.local_height,
            Focus::Remote(p) => {
                self.right.iter().find(|s| s.peer == p).map_or(self.local_height, |s| s.height)
            }
        };
        self.y = self.y.clamp(0, height - 1);

        self.focus = now;
        Update {
            focus: now,
            changed: now != before,
            // Position within the owning machine's own coordinate space.
            local_x: self.x - if now == Focus::Local { 0 } else { self.offset_of(now) },
            local_y: self.y,
        }
    }

    fn offset_of(&self, focus: Focus) -> i32 {
        let Focus::Remote(peer) = focus else { return 0 };
        let mut edge = self.local_width;
        for slot in &self.right {
            if slot.peer == peer {
                return edge;
            }
            edge += slot.width;
        }
        edge
    }
}

/// The result of applying movement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Update {
    pub focus: Focus,
    /// True when ownership just moved between machines.
    pub changed: bool,
    /// Pointer position in the owning machine's own pixels.
    pub local_x: i32,
    pub local_y: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(n: u8) -> PeerId {
        PeerId([n; 16])
    }

    fn two_machines() -> Layout {
        let mut l = Layout::new(2560, 1080);
        l.add_peer_right(peer(1), 1920, 1080);
        l
    }

    #[test]
    fn input_starts_on_the_local_machine() {
        assert_eq!(two_machines().focus(), Focus::Local);
    }

    #[test]
    fn crossing_the_right_edge_hands_over_to_the_peer() {
        let mut l = two_machines();
        // Well inside the local screen: still local.
        assert!(!l.apply_motion(100, 0).changed);
        assert_eq!(l.focus(), Focus::Local);

        // Push past the right edge.
        let update = l.apply_motion(5000, 0);
        assert!(update.changed, "crossing the edge must change ownership");
        assert_eq!(update.focus, Focus::Remote(peer(1)));
    }

    #[test]
    fn coming_back_returns_ownership_to_the_local_machine() {
        let mut l = two_machines();
        l.apply_motion(5000, 0);
        assert_eq!(l.focus(), Focus::Remote(peer(1)));

        let update = l.apply_motion(-5000, 0);
        assert!(update.changed);
        assert_eq!(update.focus, Focus::Local);
        // And it must be *inside* the local screen, not clamped to the far side.
        assert!(update.local_x < 2560);
    }

    #[test]
    fn the_pointer_lands_in_the_peers_own_coordinate_space() {
        let mut l = two_machines();
        // The pointer starts centred, so move to the far edge first and then 300 in.
        l.return_home();
        let update = l.apply_motion(2 + 300, 0);
        assert_eq!(update.focus, Focus::Remote(peer(1)));
        assert_eq!(update.local_x, 300, "must be relative to the peer's own screen");
    }

    #[test]
    fn the_pointer_cannot_run_off_the_end_of_the_strip() {
        let mut l = two_machines();
        for _ in 0..50 {
            l.apply_motion(10_000, 0);
        }
        assert_eq!(l.focus(), Focus::Remote(peer(1)), "must stop on the last machine");
        let update = l.apply_motion(10_000, 0);
        assert!(update.local_x < 1920, "must stay inside the peer's screen");
    }

    #[test]
    fn the_pointer_cannot_run_off_the_left_either() {
        let mut l = two_machines();
        for _ in 0..50 {
            l.apply_motion(-10_000, 0);
        }
        assert_eq!(l.focus(), Focus::Local);
        let update = l.apply_motion(-10_000, 0);
        assert_eq!(update.local_x, 0);
    }

    #[test]
    fn vertical_movement_is_clamped_to_the_owning_screen() {
        let mut l = Layout::new(2560, 1080);
        l.add_peer_right(peer(1), 1920, 2160); // a taller peer
        let update = l.apply_motion(0, 99_999);
        assert!(update.local_y < 1080, "clamped to the local screen while local");

        l.apply_motion(5000, 0);
        let update = l.apply_motion(0, 99_999);
        assert!(update.local_y < 2160, "clamped to the peer's own height once there");
        assert!(update.local_y >= 1080, "and may use the peer's extra height");
    }

    #[test]
    fn three_machines_hand_over_in_order() {
        let mut l = Layout::new(1000, 1000);
        l.add_peer_right(peer(1), 1000, 1000);
        l.add_peer_right(peer(2), 1000, 1000);

        // Starts centred at 500 within a 1000-wide local screen.
        assert_eq!(l.apply_motion(700, 0).focus, Focus::Remote(peer(1)));
        assert_eq!(l.apply_motion(1000, 0).focus, Focus::Remote(peer(2)));
        assert_eq!(l.apply_motion(-1000, 0).focus, Focus::Remote(peer(1)));
        assert_eq!(l.apply_motion(-1000, 0).focus, Focus::Local);
    }

    #[test]
    fn losing_the_focused_peer_returns_input_to_this_machine() {
        // Goal R2: a peer that disappears must never keep the pointer.
        let mut l = two_machines();
        l.apply_motion(5000, 0);
        assert_eq!(l.focus(), Focus::Remote(peer(1)));

        l.forget_peer(peer(1));
        assert_eq!(l.focus(), Focus::Local, "a vanished peer must not hold the pointer");
    }

    #[test]
    fn losing_an_unfocused_peer_does_not_disturb_focus() {
        let mut l = Layout::new(1000, 1000);
        l.add_peer_right(peer(1), 1000, 1000);
        l.add_peer_right(peer(2), 1000, 1000);
        l.apply_motion(700, 0);
        assert_eq!(l.focus(), Focus::Remote(peer(1)));

        l.forget_peer(peer(2));
        assert_eq!(l.focus(), Focus::Remote(peer(1)));
    }

    #[test]
    fn adding_the_same_peer_twice_does_not_duplicate_its_slot() {
        let mut l = two_machines();
        let width_before = l.total_width();
        l.add_peer_right(peer(1), 1920, 1080);
        assert_eq!(l.total_width(), width_before, "a reconnect must not widen the strip");
    }

    #[test]
    fn returning_home_puts_the_pointer_just_inside_the_edge() {
        let mut l = two_machines();
        l.apply_motion(5000, 0);
        l.return_home();
        assert_eq!(l.focus(), Focus::Local);
        let update = l.apply_motion(0, 0);
        assert!(update.local_x > 2500, "should reappear near the edge it left from");
        assert!(update.local_x < 2560);
    }
}
