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

    /// Apply movement, crossing edges as needed.
    pub(crate) fn apply_motion(&mut self, dx: i32, dy: i32) -> Update {
        let before = self.current;
        self.x = self.x.saturating_add(dx);
        self.y = self.y.saturating_add(dy);

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

            // Enter at the matching edge, carrying the *overshoot* across rather than
            // snapping to the border. Discarding it would make a fast flick stop at the
            // first screen it reaches, and would quietly throw away real hand movement.
            // The position along the edge is scaled, so screens of different sizes line
            // up proportionally instead of by raw pixel row.
            let target = &self.nodes[next];
            match edge {
                Edge::Left => {
                    self.y = scale(self.y, h, target.height);
                    self.x += target.width;
                }
                Edge::Right => {
                    self.y = scale(self.y, h, target.height);
                    self.x -= w;
                }
                Edge::Top => {
                    self.x = scale(self.x, w, target.width);
                    self.y += target.height;
                }
                Edge::Bottom => {
                    self.x = scale(self.x, w, target.width);
                    self.y -= h;
                }
            }
            self.current = next;
        }

        let node = &self.nodes[self.current];
        self.x = self.x.clamp(0, node.width - 1);
        self.y = self.y.clamp(0, node.height - 1);

        Update { focus: self.focus(), changed: self.current != before, x: self.x, y: self.y }
    }
}

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
        g.apply_motion(0, 541);
        assert_eq!(g.focus(), Focus::Remote(laptop()));

        // The pointer entered the laptop at y=1, so -2 puts it just past the top.
        let update = g.apply_motion(0, -2);
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
    fn one_fast_movement_can_cross_two_screens() {
        let mut g = Graph::new(1000, 1000);
        g.place(imac(), Edge::Left, None, 1000, 1000);
        g.place(laptop(), Edge::Left, Some(imac()), 1000, 1000);

        let update = g.apply_motion(-2500, 0);
        assert_eq!(update.focus, Focus::Remote(laptop()), "a flick must not stop halfway");
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
