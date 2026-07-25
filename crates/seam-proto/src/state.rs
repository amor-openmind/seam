//! Authoritative key state and reconciliation — the "no stuck modifiers" guarantee.
//!
//! # The problem
//!
//! Every tool in this category leaves modifier keys stuck. The cause is always the
//! same: key state lives implicitly in the receiver's OS, built up from a stream of
//! down/up events. Lose one `up` — to a dropped packet, a screen switch mid-chord, a
//! crash, a sleep — and the receiving OS believes `Ctrl` is held down forever. The user
//! discovers this when every keystroke becomes a shortcut.
//!
//! Patching it with "release everything on focus leave" is best-effort, because it only
//! runs on the paths someone remembered to cover.
//!
//! # The fix
//!
//! Make the pressed-key set an **explicit, comparable value**. The sender owns the
//! authoritative [`KeyState`]; the receiver keeps its own copy of what it has injected.
//! A heartbeat carries a [`KeyState::digest`], and any divergence — from any cause,
//! including ones nobody anticipated — is detected within one heartbeat and repaired by
//! [`KeyState::reconcile`].
//!
//! Correctness stops depending on every code path being right, and depends instead on
//! one comparison that runs continuously.

use crate::keys::{Modifiers, PhysicalKey};

/// Number of HID usage codes tracked. The Keyboard/Keypad page (`0x07`) defines usages
/// up to `0xE7`, so a 256-bit set covers every standard key with no allocation.
/// Backends map anything outside this range to [`PhysicalKey::UNKNOWN`].
const TRACKED_USAGES: usize = 256;
const WORDS: usize = TRACKED_USAGES / 64;

/// The set of physical keys currently held down, plus the modifier mask.
///
/// Fixed-size and `Copy`: comparing, diffing and hashing key state never allocates, so
/// reconciliation is safe to run on every heartbeat.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct KeyState {
    pressed: [u64; WORDS],
    modifiers: Modifiers,
}

impl KeyState {
    #[must_use]
    pub const fn new() -> Self {
        Self { pressed: [0; WORDS], modifiers: Modifiers::NONE }
    }

    /// `None` for usages outside the tracked range.
    const fn slot(key: PhysicalKey) -> Option<(usize, u64)> {
        if key.0 as usize >= TRACKED_USAGES {
            return None;
        }
        Some(((key.0 >> 6) as usize, 1u64 << (key.0 & 63)))
    }

    /// Record a key as held. Returns `true` if this changed the state.
    pub fn press(&mut self, key: PhysicalKey) -> bool {
        let Some((word, bit)) = Self::slot(key) else { return false };
        let changed = self.pressed[word] & bit == 0;
        self.pressed[word] |= bit;
        if let Some(m) = key.modifier_bit() {
            self.modifiers = self.modifiers.union(m);
        }
        changed
    }

    /// Record a key as released. Returns `true` if this changed the state.
    pub fn release(&mut self, key: PhysicalKey) -> bool {
        let Some((word, bit)) = Self::slot(key) else { return false };
        let changed = self.pressed[word] & bit != 0;
        self.pressed[word] &= !bit;
        if let Some(m) = key.modifier_bit() {
            self.modifiers = self.modifiers.without(m);
        }
        changed
    }

    #[must_use]
    pub const fn is_pressed(&self, key: PhysicalKey) -> bool {
        match Self::slot(key) {
            Some((word, bit)) => self.pressed[word] & bit != 0,
            None => false,
        }
    }

    #[must_use]
    pub const fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// Latching modifiers (Caps/Num/Scroll Lock) are state, not a held key, so they are
    /// set directly rather than inferred from press/release.
    pub fn set_locks(&mut self, locks: Modifiers) {
        const LOCKS: u16 =
            Modifiers::CAPS_LOCK.0 | Modifiers::NUM_LOCK.0 | Modifiers::SCROLL_LOCK.0;
        self.modifiers = Modifiers((self.modifiers.0 & !LOCKS) | (locks.0 & LOCKS));
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pressed.iter().all(|w| *w == 0)
    }

    #[must_use]
    pub fn count(&self) -> u32 {
        self.pressed.iter().map(|w| w.count_ones()).sum()
    }

    pub fn clear(&mut self) {
        self.pressed = [0; WORDS];
        self.modifiers = Modifiers::NONE;
    }

    /// Iterate the held keys in ascending usage order.
    pub fn iter(&self) -> impl Iterator<Item = PhysicalKey> + '_ {
        self.pressed.iter().enumerate().flat_map(|(word, bits)| {
            let mut bits = *bits;
            // `word < WORDS == 4` and `bit < 64`, so the usage is at most 255 and both
            // conversions below are exact.
            let base = u16::try_from(word).unwrap_or(0) << 6;
            core::iter::from_fn(move || {
                if bits == 0 {
                    return None;
                }
                let bit = u16::try_from(bits.trailing_zeros()).unwrap_or(0);
                bits &= bits - 1;
                Some(PhysicalKey(base | bit))
            })
        })
    }

    /// A compact fingerprint of this state, for the heartbeat.
    ///
    /// Sending 8 bytes per heartbeat instead of the full key set keeps the check cheap
    /// enough to run continuously, which is what makes divergence short-lived.
    #[must_use]
    pub fn digest(&self) -> u64 {
        // Not cryptographic and does not need to be: peers are already authenticated by
        // TLS, so this only has to catch accidental divergence, not a forged match.
        let mut acc = u64::from(self.modifiers.0).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        for (i, word) in self.pressed.iter().enumerate() {
            acc ^= splitmix64(word ^ (i as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9));
            acc = acc.rotate_left(17);
        }
        splitmix64(acc)
    }

    /// Compute the injections that bring `self` (what the receiver has injected) into
    /// agreement with `authoritative` (what the sender says is really held).
    #[must_use]
    pub fn reconcile(&self, authoritative: &Self) -> Reconciliation {
        let mut to_release = Self::new();
        let mut to_press = Self::new();
        for i in 0..WORDS {
            to_release.pressed[i] = self.pressed[i] & !authoritative.pressed[i];
            to_press.pressed[i] = authoritative.pressed[i] & !self.pressed[i];
        }
        Reconciliation { to_release, to_press, modifiers: authoritative.modifiers }
    }

    pub(crate) const fn words(&self) -> &[u64; WORDS] {
        &self.pressed
    }

    pub(crate) const fn from_parts(pressed: [u64; WORDS], modifiers: Modifiers) -> Self {
        Self { pressed, modifiers }
    }
}

/// The repair actions produced by [`KeyState::reconcile`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reconciliation {
    /// Keys the receiver is holding that the sender is not. **Release these first** —
    /// a stuck modifier corrupts every subsequent keystroke, so it is the urgent half.
    pub to_release: KeyState,
    /// Keys the sender holds that the receiver is not.
    pub to_press: KeyState,
    /// The authoritative modifier mask, including lock state.
    pub modifiers: Modifiers,
}

impl Reconciliation {
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.to_release.is_empty() && self.to_press.is_empty()
    }
}

const fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_and_release_track_membership() {
        let mut s = KeyState::new();
        assert!(s.press(PhysicalKey::A));
        assert!(!s.press(PhysicalKey::A), "repeat press is not a change");
        assert!(s.is_pressed(PhysicalKey::A));
        assert_eq!(s.count(), 1);
        assert!(s.release(PhysicalKey::A));
        assert!(!s.release(PhysicalKey::A), "repeat release is not a change");
        assert!(s.is_empty());
    }

    #[test]
    fn modifier_mask_follows_modifier_keys() {
        let mut s = KeyState::new();
        s.press(PhysicalKey::LEFT_CTRL);
        s.press(PhysicalKey::RIGHT_ALT);
        assert!(s.modifiers().contains(Modifiers::LEFT_CTRL));
        assert!(s.modifiers().contains(Modifiers::RIGHT_ALT));
        s.release(PhysicalKey::LEFT_CTRL);
        assert!(!s.modifiers().contains(Modifiers::LEFT_CTRL));
        assert!(s.modifiers().contains(Modifiers::RIGHT_ALT));
    }

    #[test]
    fn iteration_yields_exactly_the_pressed_keys() {
        let mut s = KeyState::new();
        let keys = [PhysicalKey::A, PhysicalKey::SPACE, PhysicalKey::LEFT_GUI, PhysicalKey(255)];
        for k in keys {
            s.press(k);
        }
        let mut got: Vec<_> = s.iter().collect();
        got.sort_unstable();
        let mut want = keys.to_vec();
        want.sort_unstable();
        assert_eq!(got, want);
    }

    #[test]
    fn digest_is_order_independent() {
        let mut a = KeyState::new();
        let mut b = KeyState::new();
        for k in [PhysicalKey::LEFT_CTRL, PhysicalKey::A, PhysicalKey::SPACE] {
            a.press(k);
        }
        for k in [PhysicalKey::SPACE, PhysicalKey::LEFT_CTRL, PhysicalKey::A] {
            b.press(k);
        }
        assert_eq!(a.digest(), b.digest());
    }

    #[test]
    fn digest_detects_a_single_stuck_key() {
        let mut sender = KeyState::new();
        sender.press(PhysicalKey::A);
        let mut receiver = sender;
        receiver.press(PhysicalKey::LEFT_CTRL); // the classic stuck modifier
        assert_ne!(sender.digest(), receiver.digest());
    }

    #[test]
    fn reconcile_releases_a_stuck_modifier() {
        // Receiver missed the Ctrl-up: this is the exact real-world failure.
        let mut sender = KeyState::new();
        sender.press(PhysicalKey::A);

        let mut receiver = KeyState::new();
        receiver.press(PhysicalKey::A);
        receiver.press(PhysicalKey::LEFT_CTRL);

        let fix = receiver.reconcile(&sender);
        assert!(!fix.is_noop());
        assert_eq!(fix.to_release.iter().collect::<Vec<_>>(), vec![PhysicalKey::LEFT_CTRL]);
        assert_eq!(fix.to_press.count(), 0);

        // Applying the repair converges, and a second pass is a no-op.
        for k in fix.to_release.iter() {
            receiver.release(k);
        }
        assert_eq!(receiver.digest(), sender.digest());
        assert!(receiver.reconcile(&sender).is_noop());
    }

    #[test]
    fn reconcile_presses_a_key_the_receiver_missed() {
        let mut sender = KeyState::new();
        sender.press(PhysicalKey::LEFT_SHIFT);
        let receiver = KeyState::new();

        let fix = receiver.reconcile(&sender);
        assert_eq!(fix.to_press.iter().collect::<Vec<_>>(), vec![PhysicalKey::LEFT_SHIFT]);
        assert_eq!(fix.to_release.count(), 0);
        assert!(fix.modifiers.contains(Modifiers::LEFT_SHIFT));
    }

    #[test]
    fn identical_states_reconcile_to_nothing() {
        let mut s = KeyState::new();
        s.press(PhysicalKey::LEFT_GUI);
        s.press(PhysicalKey::TAB);
        assert!(s.reconcile(&s.clone()).is_noop());
    }

    #[test]
    fn clear_releases_everything() {
        let mut s = KeyState::new();
        for k in [PhysicalKey::LEFT_CTRL, PhysicalKey::LEFT_ALT, PhysicalKey::A] {
            s.press(k);
        }
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.modifiers(), Modifiers::NONE);
        assert_eq!(s.digest(), KeyState::new().digest());
    }

    #[test]
    fn locks_are_set_not_inferred() {
        let mut s = KeyState::new();
        s.set_locks(Modifiers::CAPS_LOCK.union(Modifiers::NUM_LOCK));
        assert!(s.modifiers().contains(Modifiers::CAPS_LOCK));
        assert!(s.modifiers().contains(Modifiers::NUM_LOCK));
        s.set_locks(Modifiers::NUM_LOCK);
        assert!(!s.modifiers().contains(Modifiers::CAPS_LOCK));
        assert!(s.modifiers().contains(Modifiers::NUM_LOCK));
    }

    #[test]
    fn untracked_usages_are_ignored_rather_than_panicking() {
        let mut s = KeyState::new();
        assert!(!s.press(PhysicalKey(0x1000)));
        assert!(!s.is_pressed(PhysicalKey(0x1000)));
        assert!(s.is_empty());
    }
}
