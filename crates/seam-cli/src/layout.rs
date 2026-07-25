//! Detecting this machine's active keyboard layout.
//!
//! Goal Z2 requires that anything detectable is detected rather than asked. The active
//! layout is detectable on every target platform, and it matters more here than in most
//! software: seam's whole reason for carrying two key identities is that the sending and
//! receiving machines may compose the same character with different modifiers.
//!
//! The concrete case on this project's own fleet: a German Apple keyboard on macOS types
//! `@` as `Option+L`, while a German layout on Windows types it as `AltGr+Q`. Knowing
//! both ends' layouts is what lets seam explain a mismatch instead of silently producing
//! the wrong glyph.
//!
//! # Status
//!
//! macOS is implemented. Windows and Linux report [`Layout::Unknown`] until their
//! platform backends exist — reported honestly rather than guessed, because a *wrong*
//! layout name is worse than no layout name.

use std::process::Command;

/// The active keyboard layout, as the OS names it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Layout {
    /// The OS's identifier, e.g. `com.apple.keylayout.German-DIN-2137`.
    Known { id: String, display: String },
    /// Not detectable on this platform yet, with the reason.
    Unknown { why: String },
}

impl Layout {
    #[must_use]
    pub(crate) fn display(&self) -> &str {
        match self {
            Self::Known { display, .. } => display,
            Self::Unknown { why } => why,
        }
    }

    /// A rough guess at whether this layout composes characters with `AltGr` (Windows and
    /// Linux) or with `Option` (macOS).
    ///
    /// Advisory only, for diagnostics. seam never decides how to replay a key from this —
    /// that decision uses the text the sender actually produced, which is always correct
    /// regardless of what we believe about the layout.
    #[must_use]
    pub(crate) const fn composes_with_option() -> bool {
        cfg!(target_os = "macos")
    }
}

/// Detect the current layout.
#[must_use]
pub(crate) fn detect() -> Layout {
    #[cfg(target_os = "macos")]
    {
        detect_macos()
    }
    #[cfg(target_os = "windows")]
    {
        Layout::Unknown {
            why: "not detected yet — the Windows backend is not built (GetKeyboardLayout)".into(),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Layout::Unknown {
            why: "not detected yet — the Linux backend is not built (xkb / localectl)".into(),
        }
    }
}

/// Read the selected layout from the `HIToolbox` preference domain.
///
/// **Interim implementation.** The correct call is `TISCopyCurrentKeyboardLayoutInputSource`
/// plus `kTISPropertyLocalizedName`, which reflects a layout change immediately and needs
/// no subprocess. That belongs in the macOS platform backend; this reads the same value
/// via `defaults` so `seam doctor` can report something real today. It is called at
/// startup and in `doctor`, never on the input path.
#[cfg(target_os = "macos")]
fn detect_macos() -> Layout {
    let id = read_default("AppleCurrentKeyboardLayoutInputSourceID");
    let Some(id) = id else {
        return Layout::Unknown { why: "macOS did not report a selected keyboard layout".into() };
    };
    // `com.apple.keylayout.German-DIN-2137` → `German-DIN-2137`
    let display = id.rsplit('.').next().unwrap_or(&id).to_owned();
    Layout::Known { id, display }
}

#[cfg(target_os = "macos")]
fn read_default(key: &str) -> Option<String> {
    let output =
        Command::new("defaults").args(["read", "com.apple.HIToolbox", key]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
fn read_default(_key: &str) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_never_panics_and_always_says_something() {
        let layout = detect();
        assert!(!layout.display().is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reports_a_real_layout() {
        // On a Mac this must succeed: the layout is always detectable, so falling back to
        // Unknown here would mean seam is asking a question it did not need to ask.
        match detect() {
            Layout::Known { id, display } => {
                assert!(id.starts_with("com.apple.keylayout."), "unexpected id {id}");
                assert!(!display.is_empty());
            }
            Layout::Unknown { why } => panic!("macOS layout should be detectable: {why}"),
        }
    }
}
