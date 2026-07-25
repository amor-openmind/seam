//! Keyboard identity: physical keys, modifiers, and the logical text a key produced.
//!
//! # Why a key needs two identities
//!
//! Every tool in this category gets keyboard layouts wrong, and they all get it wrong
//! the same way: they put *one* identity on the wire and force the receiver to guess.
//!
//! - Send only the **physical** key and a US-layout sender pressing `;` lands on `ن`
//!   for a Persian receiver. Text entry is destroyed, shortcuts survive.
//! - Send only the **logical** character and `Cmd+C` on a German receiver becomes
//!   "whatever key currently produces `c`" — which may not be the `C` key, so the
//!   shortcut breaks. Text entry survives, shortcuts are destroyed.
//!
//! Neither is correct alone, because the user means different things in the two cases.
//! seam therefore puts **both** on the wire ([`PhysicalKey`] + [`LogicalText`]) and lets
//! the receiver choose per event via [`LayoutPolicy`]. See `docs/PROTOCOL.md`.

use crate::{
    Error,
    wire::{Reader, Writer},
};

/// A physical key, identified by its **USB HID Usage ID** on the
/// Keyboard/Keypad page (`0x07`).
///
/// HID usage codes are the only key identifier that is stable across all four target
/// platforms and independent of the active layout. Every backend converts to/from its
/// native code (macOS `kVK_*`, Windows scancode set 1, Linux evdev `KEY_*`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PhysicalKey(pub u16);

impl PhysicalKey {
    /// A key the sender could not map to a HID usage code.
    pub const UNKNOWN: Self = Self(0);

    // Letters are named by their *US* engraving purely as a mnemonic. The value is the
    // physical position; on an AZERTY keyboard `A` is the key US calls `Q`.
    pub const A: Self = Self(0x04);
    pub const B: Self = Self(0x05);
    pub const C: Self = Self(0x06);
    pub const V: Self = Self(0x19);
    pub const X: Self = Self(0x1B);
    pub const Y: Self = Self(0x1C);
    pub const Z: Self = Self(0x1D);

    pub const ENTER: Self = Self(0x28);
    pub const ESCAPE: Self = Self(0x29);
    pub const BACKSPACE: Self = Self(0x2A);
    pub const TAB: Self = Self(0x2B);
    pub const SPACE: Self = Self(0x2C);
    pub const CAPS_LOCK: Self = Self(0x39);

    pub const RIGHT_ARROW: Self = Self(0x4F);
    pub const LEFT_ARROW: Self = Self(0x50);
    pub const DOWN_ARROW: Self = Self(0x51);
    pub const UP_ARROW: Self = Self(0x52);

    pub const LEFT_CTRL: Self = Self(0xE0);
    pub const LEFT_SHIFT: Self = Self(0xE1);
    pub const LEFT_ALT: Self = Self(0xE2);
    pub const LEFT_GUI: Self = Self(0xE3);
    pub const RIGHT_CTRL: Self = Self(0xE4);
    pub const RIGHT_SHIFT: Self = Self(0xE5);
    pub const RIGHT_ALT: Self = Self(0xE6);
    pub const RIGHT_GUI: Self = Self(0xE7);

    #[must_use]
    pub const fn is_unknown(self) -> bool {
        self.0 == Self::UNKNOWN.0
    }

    /// Whether this key is itself a modifier (HID usages `0xE0..=0xE7`).
    #[must_use]
    pub const fn is_modifier(self) -> bool {
        self.0 >= 0xE0 && self.0 <= 0xE7
    }

    /// The modifier bit this key contributes, if any.
    #[must_use]
    pub const fn modifier_bit(self) -> Option<Modifiers> {
        match self.0 {
            0xE0 => Some(Modifiers::LEFT_CTRL),
            0xE1 => Some(Modifiers::LEFT_SHIFT),
            0xE2 => Some(Modifiers::LEFT_ALT),
            0xE3 => Some(Modifiers::LEFT_GUI),
            0xE4 => Some(Modifiers::RIGHT_CTRL),
            0xE5 => Some(Modifiers::RIGHT_SHIFT),
            0xE6 => Some(Modifiers::RIGHT_ALT),
            0xE7 => Some(Modifiers::RIGHT_GUI),
            _ => None,
        }
    }
}

/// Bitmask of modifier keys held at the moment an event was generated.
///
/// Left and right are tracked separately because `RIGHT_ALT` is `AltGr` on most European
/// layouts and must not be conflated with a plain `Alt`.
#[derive(Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Modifiers(pub u16);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const LEFT_CTRL: Self = Self(1 << 0);
    pub const LEFT_SHIFT: Self = Self(1 << 1);
    pub const LEFT_ALT: Self = Self(1 << 2);
    pub const LEFT_GUI: Self = Self(1 << 3);
    pub const RIGHT_CTRL: Self = Self(1 << 4);
    pub const RIGHT_SHIFT: Self = Self(1 << 5);
    /// `AltGr` on European layouts.
    pub const RIGHT_ALT: Self = Self(1 << 6);
    pub const RIGHT_GUI: Self = Self(1 << 7);
    pub const CAPS_LOCK: Self = Self(1 << 8);
    pub const NUM_LOCK: Self = Self(1 << 9);
    pub const SCROLL_LOCK: Self = Self(1 << 10);

    pub const ANY_CTRL: Self = Self(Self::LEFT_CTRL.0 | Self::RIGHT_CTRL.0);
    pub const ANY_SHIFT: Self = Self(Self::LEFT_SHIFT.0 | Self::RIGHT_SHIFT.0);
    pub const ANY_ALT: Self = Self(Self::LEFT_ALT.0 | Self::RIGHT_ALT.0);
    pub const ANY_GUI: Self = Self(Self::LEFT_GUI.0 | Self::RIGHT_GUI.0);

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether these modifiers make the event a **command chord** rather than text entry.
    ///
    /// Shift and the lock keys are excluded deliberately: `Shift+A` and `CapsLock`
    /// are text entry, whereas `Ctrl`/`Alt`/`Gui` mean "invoke a command".
    ///
    /// `RIGHT_ALT` (`AltGr`) alone is **not** a command chord — on European layouts it is
    /// how you type `@`, `€`, `\` and `{}`. Treating `AltGr` as a command modifier is
    /// real bug in this class of software; it makes `@` untypeable across machines.
    /// `AltGr` is therefore excluded from the command-chord test.
    #[must_use]
    pub const fn is_command_chord(self) -> bool {
        let gui_or_ctrl = self.0 & (Self::ANY_CTRL.0 | Self::ANY_GUI.0) != 0;
        let plain_left_alt = self.0 & Self::LEFT_ALT.0 != 0;
        gui_or_ctrl || plain_left_alt
    }
}

impl core::fmt::Debug for Modifiers {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_empty() {
            return f.write_str("Modifiers(NONE)");
        }
        let names = [
            (Self::LEFT_CTRL, "LCtrl"),
            (Self::LEFT_SHIFT, "LShift"),
            (Self::LEFT_ALT, "LAlt"),
            (Self::LEFT_GUI, "LGui"),
            (Self::RIGHT_CTRL, "RCtrl"),
            (Self::RIGHT_SHIFT, "RShift"),
            (Self::RIGHT_ALT, "AltGr"),
            (Self::RIGHT_GUI, "RGui"),
            (Self::CAPS_LOCK, "Caps"),
            (Self::NUM_LOCK, "Num"),
            (Self::SCROLL_LOCK, "Scroll"),
        ];
        f.write_str("Modifiers(")?;
        let mut first = true;
        for (bit, name) in names {
            if self.contains(bit) {
                if !first {
                    f.write_str("|")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        f.write_str(")")
    }
}

/// The text a key press produced on the **sender's** layout, inline and allocation-free.
///
/// This is not always one `char`: a dead-key sequence composes to `é`, and an IME can
/// commit several characters from one key press. Capacity is 15 UTF-8 bytes, which
/// covers composed Latin, Persian/Arabic, Cyrillic and CJK commits without a heap
/// allocation in the input hot path.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct LogicalText {
    len: u8,
    buf: [u8; Self::CAPACITY],
}

impl LogicalText {
    pub const CAPACITY: usize = 15;

    /// Empty — the key produced no text (a modifier, `F5`, an arrow key).
    pub const NONE: Self = Self { len: 0, buf: [0; Self::CAPACITY] };

    /// Returns `None` if `s` exceeds [`Self::CAPACITY`] bytes.
    #[must_use]
    pub fn new(s: &str) -> Option<Self> {
        let bytes = s.as_bytes();
        if bytes.len() > Self::CAPACITY {
            return None;
        }
        let mut buf = [0u8; Self::CAPACITY];
        buf[..bytes.len()].copy_from_slice(bytes);
        // Guarded above: `bytes.len() <= CAPACITY == 15`, so this conversion is exact.
        let len = u8::try_from(bytes.len()).ok()?;
        Some(Self { len, buf })
    }

    #[must_use]
    pub fn from_char(c: char) -> Self {
        let mut tmp = [0u8; 4];
        // A `char` is at most 4 UTF-8 bytes, well under CAPACITY.
        Self::new(c.encode_utf8(&mut tmp)).unwrap_or(Self::NONE)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        // Invariant: `buf[..len]` is only ever written from a `&str`, so it is valid
        // UTF-8. Decoding validates before constructing, so this holds for wire data too.
        core::str::from_utf8(&self.buf[..self.len as usize]).unwrap_or("")
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn encode(&self, w: &mut Writer<'_>) {
        w.u8(self.len);
        w.bytes(&self.buf[..self.len as usize]);
    }

    pub(crate) fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        let len = r.u8()?;
        if len as usize > Self::CAPACITY {
            return Err(Error::TooLong);
        }
        let raw = r.bytes(len as usize)?;
        // Validate here so `as_str` can never see invalid UTF-8 from a hostile peer.
        let s = core::str::from_utf8(raw).map_err(|_| Error::InvalidUtf8)?;
        Self::new(s).ok_or(Error::TooLong)
    }
}

impl core::fmt::Debug for LogicalText {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "LogicalText({:?})", self.as_str())
    }
}

/// How a receiver should reproduce a key event whose sender had a different layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum LayoutPolicy {
    /// Per-event choice — the default, and the reason seam exists.
    ///
    /// Command chords (see [`Modifiers::is_command_chord`]) replay the **physical**
    /// key, so `Cmd+C` is still `Cmd+C` on a Persian or German keyboard. Everything
    /// else replays the **logical** text, so the glyph you saw is the glyph you get.
    #[default]
    Auto = 0,
    /// Always replay the physical key. The receiver's own layout decides the glyph.
    /// Correct when you think of the remote machine as having its own keyboard, and
    /// required for games, terminals and remapping tools that read scancodes.
    Physical = 1,
    /// Always replay the logical text. Guarantees identical characters, but shortcuts
    /// depend on the receiver's layout placing that character on a sane key.
    Logical = 2,
}

impl LayoutPolicy {
    pub(crate) const fn from_u8(v: u8) -> Result<Self, Error> {
        match v {
            0 => Ok(Self::Auto),
            1 => Ok(Self::Physical),
            2 => Ok(Self::Logical),
            _ => Err(Error::UnknownVariant("LayoutPolicy")),
        }
    }
}

/// The concrete injection strategy a receiver derived for one key event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Replay {
    /// Inject the physical key plus its modifiers.
    Physical(PhysicalKey),
    /// Inject this text directly (macOS `CGEventKeyboardSetUnicodeString`,
    /// Windows `KEYEVENTF_UNICODE`, Linux transient keymap remap).
    Text(LogicalText),
}

/// Decide how to reproduce a key event. Pure, platform-free, and unit-testable.
///
/// Falls back to [`Replay::Physical`] whenever there is no usable text, and to
/// [`Replay::Text`] when the physical key is unknown to the sender.
#[must_use]
pub fn resolve_replay(
    policy: LayoutPolicy,
    physical: PhysicalKey,
    logical: LogicalText,
    modifiers: Modifiers,
) -> Replay {
    // When only one identity is usable there is no policy decision to make. Honouring
    // the policy here anyway would mean injecting nothing at all for that key.
    match (physical.is_unknown(), logical.is_empty()) {
        // No text to reproduce: F-keys, arrows and modifiers are position-only.
        (_, true) => return Replay::Physical(physical),
        // The sender could not identify the position, so the glyph is all we have.
        (true, false) => return Replay::Text(logical),
        (false, false) => {}
    }

    // Both identities are usable, so this is a genuine policy decision: does the user
    // mean the key's *position* or the *glyph* it produced?
    let prefer_position = match policy {
        LayoutPolicy::Physical => true,
        LayoutPolicy::Logical => false,
        // A command chord means "invoke this shortcut", which is addressed by position.
        LayoutPolicy::Auto => modifiers.is_command_chord(),
    };

    if prefer_position { Replay::Physical(physical) } else { Replay::Text(logical) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_text_holds_multibyte_scripts() {
        for s in ["a", "é", "ی", "щ", "の", "🙂"] {
            let t = LogicalText::new(s).expect("fits");
            assert_eq!(t.as_str(), s, "roundtrip {s}");
        }
    }

    #[test]
    fn logical_text_rejects_oversized_input() {
        assert!(LogicalText::new("0123456789abcdef").is_none()); // 16 bytes > 15
        assert!(LogicalText::new("0123456789abcde").is_some()); // 15 bytes
    }

    #[test]
    fn altgr_is_not_a_command_chord() {
        // Typing `@` on a German layout is AltGr+Q. If we classified that as a
        // shortcut we would replay the physical key and the user would get `q`.
        assert!(!Modifiers::RIGHT_ALT.is_command_chord());
        assert!(!Modifiers::ANY_SHIFT.is_command_chord());
        assert!(!Modifiers::CAPS_LOCK.is_command_chord());

        assert!(Modifiers::LEFT_CTRL.is_command_chord());
        assert!(Modifiers::LEFT_GUI.is_command_chord());
        assert!(Modifiers::LEFT_ALT.is_command_chord());
    }

    #[test]
    fn auto_policy_keeps_shortcuts_addressed_by_position() {
        // US sender presses Cmd+C. Receiver has a Persian layout where that physical
        // key types `ب`. The user means "copy", so replay the position.
        let r = resolve_replay(
            LayoutPolicy::Auto,
            PhysicalKey::C,
            LogicalText::from_char('c'),
            Modifiers::LEFT_GUI,
        );
        assert_eq!(r, Replay::Physical(PhysicalKey::C));
    }

    #[test]
    fn auto_policy_keeps_text_addressed_by_glyph() {
        // Persian sender types `ی`. Receiver has a US layout. The user means the
        // glyph, so replay the text — the physical key would produce `d`.
        let text = LogicalText::new("ی").unwrap();
        let r = resolve_replay(LayoutPolicy::Auto, PhysicalKey(0x07), text, Modifiers::NONE);
        assert_eq!(r, Replay::Text(text));
    }

    #[test]
    fn auto_policy_types_at_sign_through_altgr() {
        // German sender: AltGr+Q produces `@`. Must arrive as `@`, not `q`.
        let text = LogicalText::from_char('@');
        let r = resolve_replay(
            LayoutPolicy::Auto,
            PhysicalKey(0x14), // physical Q
            text,
            Modifiers::RIGHT_ALT,
        );
        assert_eq!(r, Replay::Text(text));
    }

    #[test]
    fn auto_policy_falls_back_to_position_for_textless_keys() {
        for key in [PhysicalKey::LEFT_ARROW, PhysicalKey::ENTER, PhysicalKey::LEFT_SHIFT] {
            let r = resolve_replay(LayoutPolicy::Auto, key, LogicalText::NONE, Modifiers::NONE);
            assert_eq!(r, Replay::Physical(key));
        }
    }

    #[test]
    fn auto_policy_falls_back_to_text_for_unknown_position() {
        let text = LogicalText::from_char('%');
        let r = resolve_replay(LayoutPolicy::Auto, PhysicalKey::UNKNOWN, text, Modifiers::NONE);
        assert_eq!(r, Replay::Text(text));
    }

    #[test]
    fn modifier_keys_map_to_their_bits() {
        assert_eq!(PhysicalKey::LEFT_CTRL.modifier_bit(), Some(Modifiers::LEFT_CTRL));
        assert_eq!(PhysicalKey::RIGHT_ALT.modifier_bit(), Some(Modifiers::RIGHT_ALT));
        assert_eq!(PhysicalKey::A.modifier_bit(), None);
        assert!(PhysicalKey::LEFT_SHIFT.is_modifier());
        assert!(!PhysicalKey::SPACE.is_modifier());
    }

    #[test]
    fn modifiers_debug_is_readable() {
        let m = Modifiers::LEFT_CTRL.union(Modifiers::RIGHT_ALT);
        assert_eq!(format!("{m:?}"), "Modifiers(LCtrl|AltGr)");
        assert_eq!(format!("{:?}", Modifiers::NONE), "Modifiers(NONE)");
    }
}
