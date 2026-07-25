//! Which machine owns the mouse and keyboard, and how the pointer travels between them.
//!
//! # A graph, not a strip
//!
//! Screens are not a row. On this project's own fleet the iMac sits left of the Mac-mini
//! and the laptop sits below the iMac, so leaving the Mac-mini's *left* edge arrives at
//! the iMac, and leaving the iMac's *bottom* edge arrives at the laptop. Modelling that
//! as a left-to-right list cannot express it.
//!
//! Each machine is a node with up to four edges, and each edge names the machine you
//! arrive at. Crossing moves the pointer to the *opposite* edge of the neighbour —
//! leaving through a left edge arrives at the neighbour's right side — which is what
//! makes the movement feel continuous rather than like a teleport.
//!
//! # Movement, not position
//!
//! Crossing is decided from relative movement. Once the pointer leaves this machine its
//! cursor is detached from the mouse and its reported position stops changing, so
//! anything reading cursor position would conclude the pointer had stopped and could
//! never bring it back.
//!
//! # One focus governs everything
//!
//! The keyboard is not routed separately from the pointer. Doing that is how you type a
//! password into the wrong machine — `inputleap#2143`, where the mouse crossed to a
//! Windows lock screen and the keyboard silently stayed behind.

use seam_proto::PeerId;

/// Which side of a screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    /// The side you arrive at, having left through this one.
    const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Top => 2,
            Self::Bottom => 3,
        }
    }

    /// Parse a side from the words people write. Used by the layout override, and kept
    /// here so the vocabulary lives with the type it names.
    #[cfg_attr(not(test), expect(dead_code, reason = "wired to --place in the next change"))]
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "top" | "up" | "above" => Some(Self::Top),
            "bottom" | "down" | "below" => Some(Self::Bottom),
            _ => None,
        }
    }
}

/// Who owns input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Focus {
    /// This machine. Nothing is forwarded.
    Local,
    /// A peer. Input goes only there, and this machine must not act on it.
    Remote(PeerId),
}

#[derive(Clone, Debug)]
struct Node {
    /// `None` is this machine.
    peer: Option<PeerId>,
    width: i32,
    height: i32,
    /// Neighbour reached through each edge, indexed by [`Edge::index`].
    links: [Option<usize>; 4],
}

/// The machines, how they join up, and where the pointer is.
#[derive(Debug)]
pub(crate) struct Graph {
    nodes: Vec<Node>,
    current: usize,
    x: i32,
    y: i32,
    /// Events to ignore the OS cursor for, after returning to this machine.
    ///
    /// Returning warps the real cursor to where the layout says the pointer is, but events
    /// already in flight were stamped with the *old* location — the boundary the pointer
    /// just crossed. Adopting one of those puts the pointer straight back on the edge and
    /// it re-crosses on the very next event, 20 ms later, which is exactly what a real
    /// session showed: every return followed immediately by another crossing.
    ///
    /// So the OS cursor is ignored briefly after a return, until the warp has certainly
    /// taken effect.
    settling: u8,
    /// Motion events left before another crossing is allowed.
    lockout: u8,
}

impl Graph {
    /// Start with only this machine, pointer in the middle of it.
    pub(crate) fn new(width: i32, height: i32) -> Self {
        Self {
            nodes: vec![Node {
                peer: None,
                width: width.max(1),
                height: height.max(1),
                links: [None; 4],
            }],
            current: 0,
            x: width / 2,
            y: height / 2,
            settling: 0,
            lockout: 0,
        }
    }

    fn index_of(&self, peer: PeerId) -> Option<usize> {
        self.nodes.iter().position(|n| n.peer == Some(peer))
    }

    fn ensure(&mut self, peer: PeerId, width: i32, height: i32) -> usize {
        if let Some(i) = self.index_of(peer) {
            return i;
        }
        self.nodes.push(Node {
            peer: Some(peer),
            width: width.max(1),
            height: height.max(1),
            links: [None; 4],
        });
        self.nodes.len() - 1
    }

    /// Attach `peer` on the given side of `anchor` (`None` meaning this machine).
    ///
    /// The reverse link is created automatically: a screen you can reach must be one you
    /// can come back from, and a one-way edge is how a pointer gets stranded.
    pub(crate) fn place(
        &mut self,
        peer: PeerId,
        edge: Edge,
        anchor: Option<PeerId>,
        width: i32,
        height: i32,
    ) {
        let anchor_index = match anchor {
            None => 0,
            Some(a) => match self.index_of(a) {
                Some(i) => i,
                None => return,
            },
        };
        let peer_index = self.ensure(peer, width, height);
        if peer_index == anchor_index {
            return;
        }
        self.nodes[anchor_index].links[edge.index()] = Some(peer_index);
        self.nodes[peer_index].links[edge.opposite().index()] = Some(anchor_index);
    }

    /// Remove a peer, returning the pointer here if it held it.
    pub(crate) fn forget(&mut self, peer: PeerId) {
        let Some(index) = self.index_of(peer) else { return };
        for node in &mut self.nodes {
            for link in &mut node.links {
                if *link == Some(index) {
                    *link = None;
                }
            }
        }
        if self.current == index {
            // Never leave input aimed at a machine that is gone (goal R2).
            self.return_home();
        }
    }

    pub(crate) fn return_home(&mut self) {
        self.settling = SETTLING_EVENTS;
        self.current = 0;
        self.x = self.nodes[0].width / 2;
        self.y = self.nodes[0].height / 2;
    }

    #[must_use]
    pub(crate) fn focus(&self) -> Focus {
        self.nodes[self.current].peer.map_or(Focus::Local, Focus::Remote)
    }

    #[must_use]
    pub(crate) fn is_placed(&self, peer: PeerId) -> bool {
        self.index_of(peer).is_some()
    }

    /// Adopt the operating system's own cursor position while input is local.
    ///
    /// This is the difference between a boundary at the screen edge and a boundary
    /// somewhere in the middle of it. While the pointer is on this machine the OS cursor
    /// is the truth: the user can move it with a trackpad, another app can warp it, and
    /// it starts wherever it was left. Accumulating deltas from an assumed starting point
    /// drifts away from the real cursor, and the screen edge is then reached — and
    /// crossed — while the visible pointer is still mid-screen.
    ///
    /// Once focus is remote the OS cursor is detached and stops moving, so it stops being
    /// a useful signal and movement is accumulated instead. This is the same split
    /// Barrier and Synergy use.
    pub(crate) fn sync_local_cursor(&mut self, x: i32, y: i32) {
        if self.current != 0 {
            return;
        }
        if self.settling > 0 {
            // Still waiting for the warp to be reflected in the events we receive.
            self.settling -= 1;
            return;
        }
        let node = &self.nodes[0];
        self.x = x.clamp(0, node.width - 1);
        self.y = y.clamp(0, node.height - 1);
    }

    /// Send the pointer home unconditionally — the panic path, and the UI's release
    /// button. Same settling/lockout treatment as a crossing, so the return is clean.
    pub(crate) fn force_home(&mut self) -> Update {
        let before = self.current;
        self.current = 0;
        let node = &self.nodes[0];
        self.x = self.x.clamp(0, node.width - 1);
        self.y = self.y.clamp(0, node.height - 1);
        self.settling = SETTLING_EVENTS;
        self.lockout = CROSSING_LOCKOUT;
        Update { focus: self.focus(), changed: self.current != before, x: self.x, y: self.y }
    }

    /// Apply movement, crossing edges as needed.
    pub(crate) fn apply_motion(&mut self, dx: i32, dy: i32) -> Update {
        let before = self.current;
        self.x = self.x.saturating_add(dx);
        self.y = self.y.saturating_add(dy);

        // A crossing that just happened locks out the next few, and this is the whole
        // reason focus stopped oscillating.
        //
        // Landing ENTRY_MARGIN inside the new screen is not enough on its own: a fast
        // mouse reports 30-60 px in a single event, far more than the margin, so the very
        // next event carried the pointer straight back out of the edge it had just come
        // in through. The log showed focus alternating between two machines every 20 ms —
        // the event rate — and while it read "back on this machine" suppression was off,
        // so half of every keystroke and every motion landed on the local machine as well
        // as the remote one. From a chair that is "it types on both".
        //
        // The margin was always a distance answering a velocity question. This is the
        // directional lockout Barrier uses instead.
        if self.lockout > 0 {
            self.lockout -= 1;
            let node = &self.nodes[self.current];
            self.x = self.x.clamp(0, node.width - 1);
            self.y = self.y.clamp(0, node.height - 1);
            return Update {
                focus: self.focus(),
                changed: self.current != before,
                x: self.x,
                y: self.y,
            };
        }

        // Loop, because one fast flick can cross more than one screen.
        for _ in 0..8 {
            let node = &self.nodes[self.current];
            let (w, h) = (node.width, node.height);

            let crossing = if self.x < 0 {
                Some(Edge::Left)
            } else if self.x >= w {
                Some(Edge::Right)
            } else if self.y < 0 {
                Some(Edge::Top)
            } else if self.y >= h {
                Some(Edge::Bottom)
            } else {
                None
            };

            let Some(edge) = crossing else { break };
            let Some(next) = node.links[edge.index()] else {
                // No neighbour that way: stop at the edge rather than letting the pointer
                // wander somewhere no screen exists.
                self.x = self.x.clamp(0, w - 1);
                self.y = self.y.clamp(0, h - 1);
                break;
            };

            // Enter *at* the matching edge, a small margin in — the overshoot is
            // deliberately discarded.
            //
            // Carrying it across seemed more faithful, and is much worse to use: pushing
            // hard off an edge lands the pointer as deep inside the neighbour as the
            // shove was long, so returning means retracing that whole distance. Reported
            // as "it takes many movements to get back; once doesn't work".
            //
            // Landing at the edge means one small movement always brings the pointer
            // home, which is how Barrier and Synergy behave and what the hand expects.
            // The cost is that a single fast flick crosses one screen rather than two.
            //
            // The position along the edge is scaled, so screens of different sizes line
            // up proportionally instead of by raw pixel row.
            let target = &self.nodes[next];
            let (tw, th) = (target.width, target.height);
            match edge {
                Edge::Left => {
                    self.y = scale(self.y, h, th);
                    self.x = tw - 1 - ENTRY_MARGIN.min(tw / 4);
                }
                Edge::Right => {
                    self.y = scale(self.y, h, th);
                    self.x = ENTRY_MARGIN.min(tw / 4);
                }
                Edge::Top => {
                    self.x = scale(self.x, w, tw);
                    self.y = th - 1 - ENTRY_MARGIN.min(th / 4);
                }
                Edge::Bottom => {
                    self.x = scale(self.x, w, tw);
                    self.y = ENTRY_MARGIN.min(th / 4);
                }
            }
            // Having just arrived, ignore the OS cursor for a few events: anything already
            // in flight still reports the position on the far side of the boundary.
            self.settling = SETTLING_EVENTS;
            self.lockout = CROSSING_LOCKOUT;
            self.current = next;
        }

        let node = &self.nodes[self.current];
        self.x = self.x.clamp(0, node.width - 1);
        self.y = self.y.clamp(0, node.height - 1);

        Update { focus: self.focus(), changed: self.current != before, x: self.x, y: self.y }
    }
}

/// How far inside a screen the pointer is placed on arrival.
///
/// Landing exactly on the boundary means a single pixel of movement the other way crosses
/// straight back, and the pointer ping-pongs between machines on consecutive events —
/// observed in a real session as a return immediately followed by another crossing 20 ms
/// later, which reads as "it never came back".
///
/// A margin makes the boundary hysteretic: having crossed, the pointer must be moved
/// deliberately back through the margin to cross again. Every KVM needs some form of this;
/// Barrier calls it a switch delay.
const ENTRY_MARGIN: i32 = 80;

// Why 80 and not 12.
//
// 12 px was chosen as "just off the boundary", which is the right idea measured against
// the wrong thing. A mouse moving at speed reports 30-60 px in a single event, so a
// pointer landing 12 px inside was carried straight back out by the very next one. The
// crossing lockout then bounded how fast that could repeat, and the log showed exactly
// that: a bounce every 180 ms - the lockout period - instead of every 20 ms. Rate-limiting
// an oscillation is not stopping it.
//
// 80 px is more than one fast event's travel, so no single event can re-cross, while
// staying well under one deliberate movement of the hand. Coming back is still one small
// push, which matters: an earlier attempt to make the boundary sticky by direction meant
// the pointer had to be walked away from the edge before it could leave, and that felt
// like the machine refusing to give the pointer back.

/// How many events to ignore the OS cursor for after a crossing.
///
/// Events arrive roughly every 20 ms, so this is a settling window of about 100 ms — long
/// enough for a warp to be reflected in what we receive, short enough to be invisible.
const SETTLING_EVENTS: u8 = 12;

// SETTLING_EVENTS must stay LARGER than CROSSING_LOCKOUT.
//
// If adoption of the OS cursor resumes while the lockout is still running, the pointer
// adopts a position that is stale by construction: the local cursor has not moved since
// the pointer left, so it is still sitting on the boundary. That drags the pointer back
// onto the edge, and it re-crosses the instant the lockout expires - a bounce at exactly
// the lockout period. The live log showed 180 ms gaps against an 8-event lockout, which
// is that arithmetic.

/// Motion events that must pass after a crossing before another is allowed.
///
/// At roughly 50 Hz this is about 160 ms: longer than any in-flight event, far shorter
/// than a deliberate move back. It bounds how fast focus can change no matter how fast the
/// mouse moves, which a distance margin cannot do.
const CROSSING_LOCKOUT: u8 = 8;

// Enforced at compile time, because getting this ordering wrong is silent: everything
// still works, focus just bounces at the lockout period and input lands on two machines.
const _: () = assert!(
    SETTLING_EVENTS > CROSSING_LOCKOUT,
    "settling must outlast the lockout, or a stale cursor position is adopted mid-lockout"
);


/// Map a position along an edge onto a screen of a different size, so a 1080-tall screen
/// and a 2160-tall one line up proportionally rather than by raw pixel row.
fn scale(value: i32, from: i32, to: i32) -> i32 {
    if from <= 1 {
        return 0;
    }
    let scaled = i64::from(value) * i64::from(to - 1) / i64::from(from - 1);
    i32::try_from(scaled).unwrap_or(0).clamp(0, (to - 1).max(0))
}

/// The result of applying movement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Update {
    pub focus: Focus,
    /// True when ownership just moved between machines.
    pub changed: bool,
    /// Pointer position in the owning machine's own pixels.
    pub x: i32,
    pub y: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Let the post-crossing lockout drain, as real mouse movement does.
    ///
    /// A crossing locks out the next few so a fast mouse cannot bounce straight back
    /// (`oscillation::a_fast_mouse_cannot_bounce_straight_back_across_an_edge`). A person
    /// moving the mouse produces a stream of events and never notices; a test that calls
    /// `apply_motion` twice does, so it has to spend the lockout explicitly.
    fn drain_lockout(g: &mut Graph) {
        for _ in 0..CROSSING_LOCKOUT {
            g.apply_motion(0, 0);
        }
    }


    fn imac() -> PeerId {
        PeerId([1; 16])
    }
    fn laptop() -> PeerId {
        PeerId([2; 16])
    }

    /// The development fleet: iMac left of the Mac-mini, laptop below the iMac.
    fn fleet() -> Graph {
        let mut g = Graph::new(2560, 1080);
        g.place(imac(), Edge::Left, None, 1920, 1080);
        g.place(laptop(), Edge::Bottom, Some(imac()), 1920, 1080);
        g
    }

    #[test]
    fn the_local_cursor_position_is_adopted_while_input_is_here() {
        // Otherwise the boundary drifts away from the real cursor and the pointer crosses
        // while it is still visibly mid-screen.
        let mut g = fleet();
        g.sync_local_cursor(2559, 500);
        // One step left from the far right edge must NOT cross.
        let update = g.apply_motion(-1, 0);
        assert_eq!(update.focus, Focus::Local);
        assert_eq!(update.x, 2558);

        // From the left edge, one step left must cross.
        g.sync_local_cursor(0, 500);
        assert_eq!(g.apply_motion(-1, 0).focus, Focus::Remote(imac()));
    }

    #[test]
    fn the_local_cursor_is_ignored_once_the_pointer_has_left() {
        // The OS cursor is detached and frozen while a peer owns the pointer, so adopting
        // it would drag focus back to a stale position.
        let mut g = fleet();
        g.sync_local_cursor(0, 500);
        g.apply_motion(-1, 0);
        assert_eq!(g.focus(), Focus::Remote(imac()));
        drain_lockout(&mut g);

        g.sync_local_cursor(1280, 540);
        assert_eq!(g.focus(), Focus::Remote(imac()), "must stay on the peer");
    }

    #[test]
    fn input_starts_here() {
        assert_eq!(fleet().focus(), Focus::Local);
    }

    #[test]
    fn leaving_the_left_edge_reaches_the_machine_on_the_left() {
        let mut g = fleet();
        let update = g.apply_motion(-5000, 0);
        assert!(update.changed);
        assert_eq!(update.focus, Focus::Remote(imac()));
    }

    #[test]
    fn leaving_that_machines_right_edge_comes_back_here() {
        let mut g = fleet();
        g.apply_motion(-5000, 0);
        assert_eq!(g.focus(), Focus::Remote(imac()));

        drain_lockout(&mut g);
        let update = g.apply_motion(5000, 0);
        assert!(update.changed);
        assert_eq!(update.focus, Focus::Local);
    }

    #[test]
    fn arriving_from_the_left_edge_lands_on_the_neighbours_right_side() {
        // Continuity: pushing left off this screen must appear at the *right* of the next
        // one, not teleport to its left.
        let mut g = fleet();
        // Just past the edge: the overshoot is small, so it lands near the far side.
        let update = g.apply_motion(-1281, 0);
        assert_eq!(update.focus, Focus::Remote(imac()));
        assert!(update.x > 1800, "should enter near the right edge, got {}", update.x);
    }

    #[test]
    fn leaving_the_bottom_of_the_left_machine_reaches_the_laptop() {
        let mut g = fleet();
        g.apply_motion(-1281, 0);
        assert_eq!(g.focus(), Focus::Remote(imac()));
        drain_lockout(&mut g);

        // Pointer sits at y=540 on a 1080-tall screen; 541 puts it just past the bottom.
        let update = g.apply_motion(0, 541);
        assert!(update.changed);
        assert_eq!(update.focus, Focus::Remote(laptop()));
        assert!(update.y < 100, "should enter near the top, got {}", update.y);
    }

    #[test]
    fn leaving_the_top_of_the_laptop_returns_to_the_left_machine() {
        let mut g = fleet();
        g.apply_motion(-1281, 0);
        drain_lockout(&mut g);
        g.apply_motion(0, 541);
        assert_eq!(g.focus(), Focus::Remote(laptop()));
        drain_lockout(&mut g);

        // The pointer enters the laptop a margin below its top edge, so it must be moved
        // deliberately back through that margin to leave again.
        let update = g.apply_motion(0, -(ENTRY_MARGIN + 2));
        assert_eq!(update.focus, Focus::Remote(imac()));
        assert!(update.y > 900, "should enter near the bottom, got {}", update.y);
    }

    #[test]
    fn there_is_no_direct_edge_between_this_machine_and_the_laptop() {
        // The laptop is only reachable through the iMac, matching the physical layout.
        let mut g = fleet();
        let update = g.apply_motion(0, 5000);
        assert_eq!(update.focus, Focus::Local, "the bottom edge here leads nowhere");
        assert_eq!(update.y, 1079, "so the pointer stops at the edge");
    }

    #[test]
    fn an_edge_with_no_neighbour_stops_the_pointer_rather_than_losing_it() {
        let mut g = fleet();
        let update = g.apply_motion(5000, 0);
        assert_eq!(update.focus, Focus::Local);
        assert_eq!(update.x, 2559);
    }

    #[test]
    fn a_fast_flick_crosses_one_screen_and_stops_there() {
        // Deliberate: the overshoot is discarded so the pointer lands at the edge it
        // entered by, which is what makes one small movement enough to come back.
        let mut g = Graph::new(1000, 1000);
        g.place(imac(), Edge::Left, None, 1000, 1000);
        g.place(laptop(), Edge::Left, Some(imac()), 1000, 1000);

        let update = g.apply_motion(-2500, 0);
        assert_eq!(update.focus, Focus::Remote(imac()), "should stop at the first screen");
    }

    #[test]
    fn a_hard_shove_still_returns_with_one_small_movement() {
        // The reported problem: pushing hard onto a peer used to land the pointer as deep
        // inside as the shove was long, so getting back meant retracing all of it.
        let mut g = fleet();
        g.sync_local_cursor(0, 540);
        g.apply_motion(-3000, 0);
        assert_eq!(g.focus(), Focus::Remote(imac()));

        drain_lockout(&mut g);
        let update = g.apply_motion(ENTRY_MARGIN + 1, 0);
        assert_eq!(update.focus, Focus::Local, "one small movement must bring it home");
    }

    #[test]
    fn position_along_the_edge_is_kept_proportional() {
        // A 1080-tall screen meeting a 2160-tall one must line up by proportion, not by
        // raw pixel row, or the pointer jumps vertically on every crossing.
        let mut g = Graph::new(2560, 1080);
        g.place(imac(), Edge::Left, None, 1920, 2160);

        g.apply_motion(0, -1080); // to the very top
        let update = g.apply_motion(-5000, 0);
        assert_eq!(update.focus, Focus::Remote(imac()));
        assert!(update.y < 100, "top of one screen must arrive at the top of the other");
    }

    #[test]
    fn losing_the_focused_machine_returns_input_here() {
        let mut g = fleet();
        g.apply_motion(-5000, 0);
        assert_eq!(g.focus(), Focus::Remote(imac()));

        g.forget(imac());
        assert_eq!(g.focus(), Focus::Local, "a vanished machine must not hold the pointer");
    }

    #[test]
    fn losing_an_unfocused_machine_does_not_disturb_focus() {
        let mut g = fleet();
        g.apply_motion(-5000, 0);
        g.forget(laptop());
        assert_eq!(g.focus(), Focus::Remote(imac()));
    }

    #[test]
    fn a_removed_machine_is_no_longer_reachable() {
        let mut g = fleet();
        g.forget(imac());
        let update = g.apply_motion(-5000, 0);
        assert_eq!(update.focus, Focus::Local, "its edge must be gone too");
    }

    #[test]
    fn placing_the_same_machine_twice_does_not_duplicate_it() {
        let mut g = fleet();
        let before = g.nodes.len();
        g.place(imac(), Edge::Left, None, 1920, 1080);
        assert_eq!(g.nodes.len(), before, "a reconnect must not add a second screen");
    }

    #[test]
    fn edges_parse_the_words_people_actually_use() {
        assert_eq!(Edge::parse("left"), Some(Edge::Left));
        assert_eq!(Edge::parse(" Right "), Some(Edge::Right));
        assert_eq!(Edge::parse("above"), Some(Edge::Top));
        assert_eq!(Edge::parse("below"), Some(Edge::Bottom));
        assert_eq!(Edge::parse("sideways"), None);
    }
}

#[cfg(test)]
mod round_trip {
    //! Simulation of the daemon's actual event loop.
    //!
    //! These exist because a series of regressions all shared one shape: each was correct
    //! in isolation and wrong in sequence. Testing `apply_motion` alone cannot catch them,
    //! because the daemon does two things per event — adopt the OS cursor while local, then
    //! apply movement — and the bugs lived in the interaction.
    //!
    //! The simulated OS cursor behaves like the real one: it tracks movement while input is
    //! local, and **freezes** once the pointer leaves, because the daemon detaches it.

    use super::*;

    fn imac() -> PeerId {
        PeerId([1; 16])
    }

    /// Stands in for the daemon loop and the OS cursor together.
    struct Machine {
        graph: Graph,
        /// Where macOS thinks the cursor is.
        cursor: (i32, i32),
        width: i32,
        height: i32,
    }

    impl Machine {
        fn new() -> Self {
            let (width, height) = (2560, 1080);
            let mut graph = Graph::new(width, height);
            graph.place(imac(), Edge::Left, None, 1920, 1080);
            Self { graph, cursor: (width / 2, height / 2), width, height }
        }

        /// One mouse movement, processed exactly as the daemon processes it.
        fn r#move(&mut self, dx: i32, dy: i32) -> Update {
            let local_before = self.graph.focus() == Focus::Local;
            if local_before {
                // The OS moves its own cursor, clamped to the screen.
                self.cursor.0 = (self.cursor.0 + dx).clamp(0, self.width - 1);
                self.cursor.1 = (self.cursor.1 + dy).clamp(0, self.height - 1);
            }
            // The daemon adopts the OS position while local, then applies movement.
            self.graph.sync_local_cursor(self.cursor.0, self.cursor.1);
            let update = self.graph.apply_motion(dx, dy);

            if update.changed && update.focus == Focus::Local {
                // Returning focus warps the real cursor to match the layout.
                self.cursor = (update.x, update.y);
            }
            update
        }
    }

    #[test]
    fn the_pointer_leaves_and_comes_back() {
        // The whole round trip, which is what kept breaking.
        let mut m = Machine::new();

        // Walk left to the edge, then across.
        let mut crossed = false;
        for _ in 0..100 {
            if m.r#move(-40, 0).focus != Focus::Local {
                crossed = true;
                break;
            }
        }
        assert!(crossed, "the pointer never left this machine");

        // Walk back right.
        let mut returned = false;
        for _ in 0..100 {
            if m.r#move(40, 0).focus == Focus::Local {
                returned = true;
                break;
            }
        }
        assert!(returned, "the pointer never came back to this machine");
    }

    #[test]
    fn it_survives_many_round_trips() {
        // A one-way bug can pass a single crossing and fail on the second.
        let mut m = Machine::new();
        for trip in 0..5 {
            let mut left = false;
            for _ in 0..200 {
                if m.r#move(-40, 0).focus != Focus::Local {
                    left = true;
                    break;
                }
            }
            assert!(left, "failed to leave on trip {trip}");

            let mut back = false;
            for _ in 0..200 {
                if m.r#move(40, 0).focus == Focus::Local {
                    back = true;
                    break;
                }
            }
            assert!(back, "failed to return on trip {trip}");
        }
    }

    #[test]
    fn the_pointer_does_not_oscillate_at_the_boundary() {
        // Coming back, small movements must not flip focus every event — the narrow band
        // along the shared edge where the pointer appeared to be on both machines.
        let mut m = Machine::new();
        while m.graph.focus() == Focus::Local {
            m.r#move(-40, 0);
        }
        while m.graph.focus() != Focus::Local {
            m.r#move(40, 0);
        }

        let mut flips = 0;
        for _ in 0..40 {
            if m.r#move(2, 0).changed {
                flips += 1;
            }
        }
        assert_eq!(flips, 0, "focus flipped {flips} times while moving away from the edge");
    }

    #[test]
    fn moving_within_this_machine_never_hands_over() {
        let mut m = Machine::new();
        for _ in 0..30 {
            let update = m.r#move(10, 5);
            assert_eq!(update.focus, Focus::Local);
            assert!(!update.changed);
        }
    }

    #[test]
    fn the_returning_pointer_lands_on_this_machine() {
        let mut m = Machine::new();
        while m.graph.focus() == Focus::Local {
            m.r#move(-40, 0);
        }
        let mut update = m.r#move(40, 0);
        while update.focus != Focus::Local {
            update = m.r#move(40, 0);
        }
        assert!(update.x >= 0 && update.x < 2560, "landed off-screen at x={}", update.x);
        assert!(update.y >= 0 && update.y < 1080, "landed off-screen at y={}", update.y);
    }
}

#[cfg(test)]
mod entry_position {
    //! Where the pointer lands on arrival, and whether it can get back out.
    //!
    //! Crossing carries the overshoot, so arriving with a large one can place the pointer
    //! deep inside the neighbour — or, with the wrong sign, right back at the edge it came
    //! from. Either makes returning feel broken in a way the graph tests above do not
    //! reveal, because they only assert which machine holds focus.

    use super::*;

    /// Let the post-crossing lockout drain, as real mouse movement does.
    ///
    /// A crossing locks out the next few so a fast mouse cannot bounce straight back
    /// (`oscillation::a_fast_mouse_cannot_bounce_straight_back_across_an_edge`). A person
    /// moving the mouse produces a stream of events and never notices; a test that calls
    /// `apply_motion` twice does, so it has to spend the lockout explicitly.
    fn drain_lockout(g: &mut Graph) {
        for _ in 0..CROSSING_LOCKOUT {
            g.apply_motion(0, 0);
        }
    }


    fn imac() -> PeerId {
        PeerId([1; 16])
    }

    fn pair() -> Graph {
        let mut g = Graph::new(2560, 1080);
        g.place(imac(), Edge::Left, None, 1920, 1080);
        g
    }

    #[test]
    fn a_gentle_crossing_lands_just_inside_the_neighbour() {
        let mut g = pair();
        g.sync_local_cursor(0, 540);
        let update = g.apply_motion(-1, 0);
        assert_eq!(update.focus, Focus::Remote(imac()));
        assert_eq!(
            update.x,
            1919 - ENTRY_MARGIN,
            "arrives a margin inside the far edge, not on it"
        );
    }

    #[test]
    fn a_fast_crossing_still_lands_at_the_edge() {
        // However hard the shove, the pointer arrives at the edge it entered by. This is
        // what stops a hard push from burying it deep inside the neighbour.
        let mut g = pair();
        g.sync_local_cursor(0, 540);
        let update = g.apply_motion(-800, 0);
        assert_eq!(update.focus, Focus::Remote(imac()));
        assert_eq!(
            update.x,
            1919 - ENTRY_MARGIN,
            "a margin inside the far edge, regardless of overshoot"
        );
    }

    #[test]
    fn the_distance_back_does_not_depend_on_how_hard_it_was_pushed() {
        // Returning must cost the same whether the pointer arrived gently or was flung.
        let mut gentle = pair();
        gentle.sync_local_cursor(0, 540);
        gentle.apply_motion(-1, 0);
        drain_lockout(&mut gentle);

        let mut flung = pair();
        flung.sync_local_cursor(0, 540);
        flung.apply_motion(-5000, 0);
        drain_lockout(&mut flung);

        let step = ENTRY_MARGIN + 1;
        assert_eq!(gentle.apply_motion(step, 0).focus, Focus::Local);
        assert_eq!(flung.apply_motion(step, 0).focus, Focus::Local);
    }

    #[test]
    fn a_crossing_cannot_immediately_bounce_back() {
        // This is the bug seen in a real session: every return was followed by another
        // crossing 20 ms later, so the pointer never appeared to come back at all.
        let mut g = pair();
        g.sync_local_cursor(0, 540);
        g.apply_motion(-1, 0);
        assert_eq!(g.focus(), Focus::Remote(imac()));

        for step in 0..ENTRY_MARGIN {
            assert_eq!(
                g.apply_motion(1, 0).focus,
                Focus::Remote(imac()),
                "bounced back after only {step} px"
            );
        }
        // Deliberate movement through the margin still crosses.
        assert_eq!(g.apply_motion(1, 0).focus, Focus::Local);
    }

    #[test]
    fn the_margin_protects_both_directions() {
        // Returning must be as stable as leaving, or the pointer ping-pongs on the way
        // home instead of on the way out.
        let mut g = pair();
        g.sync_local_cursor(0, 540);
        g.apply_motion(-1, 0);
        while g.focus() != Focus::Local {
            g.apply_motion(1, 0);
        }
        for step in 0..ENTRY_MARGIN {
            assert_eq!(
                g.apply_motion(-1, 0).focus,
                Focus::Local,
                "bounced away again after only {step} px"
            );
        }
    }

    #[test]
    fn a_peer_reporting_an_enormous_screen_is_still_escapable() {
        // If geometry were ever wrong or stale, the pointer must still be able to get out
        // rather than being trapped behind a boundary that is thousands of pixels away.
        let mut g = Graph::new(2560, 1080);
        g.place(imac(), Edge::Left, None, 30_000, 1080);
        g.sync_local_cursor(0, 540);
        g.apply_motion(-1, 0);
        assert_eq!(g.focus(), Focus::Remote(imac()));
        drain_lockout(&mut g);
        // Past the entry margin, it is still escapable — wrong or stale geometry must
        // never trap the pointer behind a boundary thousands of pixels away.
        assert_eq!(
            g.apply_motion(ENTRY_MARGIN + 1, 0).focus,
            Focus::Local,
            "a short deliberate movement must still return it"
        );
    }
}

#[cfg(test)]
mod oscillation {
    use super::*;

    fn fleet() -> Graph {
        let mac = seam_proto::PeerId([1; 16]);
        let imac = seam_proto::PeerId([2; 16]);
        let mut g = Graph::new(1920, 1080);
        g.place(imac, Edge::Left, None, 2048, 1152);
        let _ = mac;
        g
    }

    #[test]
    fn a_fast_mouse_cannot_bounce_straight_back_across_an_edge() {
        // The reported bug, as arithmetic: cross left, then immediately move right by more
        // than the entry margin. Before the lockout this crossed back on the very next
        // event, and focus alternated at the event rate — which is why input landed on
        // both machines at once.
        let mut g = fleet();
        g.apply_motion(-2000, 0);
        let crossed = g.current;
        assert_ne!(crossed, 0, "should have left the local screen");

        // A single fast event, far larger than ENTRY_MARGIN.
        let update = g.apply_motion(60, 0);
        assert_eq!(g.current, crossed, "must not bounce back on the next event");
        assert!(!update.changed, "focus must not change during the lockout");
    }

    #[test]
    fn the_pointer_can_still_come_back_deliberately() {
        // The lockout must not strand the pointer: after it expires, moving back works.
        let mut g = fleet();
        g.apply_motion(-2000, 0);
        for _ in 0..CROSSING_LOCKOUT {
            g.apply_motion(1, 0);
        }
        g.apply_motion(5000, 0);
        assert_eq!(g.current, 0, "the pointer must be able to return home");
    }

    #[test]
    fn the_lockout_expires_rather_than_latching() {
        let mut g = fleet();
        g.apply_motion(-2000, 0);
        for _ in 0..20 {
            g.apply_motion(0, 1);
        }
        assert_eq!(g.lockout, 0, "the lockout must drain, never latch");
    }
}

