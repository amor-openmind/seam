//! # seam-input
//!
//! Platform input capture, injection and display geometry.
//!
//! This is the only crate in seam that contains `unsafe`. Everything else is
//! `#![forbid(unsafe_code)]`, and the boundary is deliberate: FFI into the OS input stack
//! is where a mistake stops being a wrong pixel and starts being a wedged machine.
//!
//! ## The governing rule: fail open
//!
//! On macOS, an active event tap whose permission is revoked mid-session can freeze all
//! local input until a hard reboot. On Windows, `SendInput` blocked by UIPI fails
//! **silently** — neither the return value nor `GetLastError` reports it. On Linux,
//! `EVIOCGRAB` on the user's only keyboard locks them out if the process dies holding it.
//!
//! In every one of those cases the safe behaviour is the same: **give input back to the
//! local machine**. Not forwarding a keystroke is a bug. Leaving someone unable to use
//! their computer is not a bug, it is damage.

#![cfg_attr(not(any(target_os = "macos", target_os = "windows")), forbid(unsafe_code))]

pub mod screen;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

pub use screen::{Desktop, Display, MM, Millis, PixelRect};

/// A platform backend error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("the operating system refused an input operation: {0}")]
    Platform(String),

    #[error(
        "seam needs permission to {what} on this machine. Grant it in {where_to}, then \
         restart seam — the permission does not apply to an already-running program."
    )]
    PermissionDenied { what: String, where_to: String },

    #[error("input {0} is not implemented on this platform yet")]
    Unsupported(&'static str),
}

/// Read this machine's display layout.
///
/// Detected, never configured (goal Z2), and callers are expected to re-read it on
/// display reconfiguration rather than caching it at startup (goal F10).
pub fn desktop() -> Result<Desktop, Error> {
    #[cfg(target_os = "macos")]
    {
        macos::desktop()
    }
    #[cfg(target_os = "windows")]
    {
        windows::desktop()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err(Error::Unsupported("display enumeration"))
    }
}

/// Where the pointer is, in this machine's pixel coordinates.
pub fn cursor_position() -> Result<(i32, i32), Error> {
    #[cfg(target_os = "macos")]
    {
        macos::cursor_position()
    }
    #[cfg(target_os = "windows")]
    {
        windows::cursor_position()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err(Error::Unsupported("cursor position"))
    }
}

/// Move the pointer without generating input events.
pub fn warp_cursor(x: i32, y: i32) -> Result<(), Error> {
    #[cfg(target_os = "macos")]
    {
        macos::warp_cursor(x, y)
    }
    #[cfg(target_os = "windows")]
    {
        windows::warp_cursor(x, y)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (x, y);
        Err(Error::Unsupported("cursor warping"))
    }
}

/// Return input to the local machine unconditionally.
///
/// Safe to call at any time, including when seam never took control. This is the recovery
/// path for a previous process that died holding OS input state — the exact failure that
/// stranded the pointer on this machine when another KVM was killed mid-session.
pub fn release_input() {
    #[cfg(target_os = "macos")]
    {
        macos::force_restore_cursor();
    }
}

/// Move the pointer by injecting a real input event, so applications see genuine movement
/// rather than a teleport.
///
/// This is what a receiving machine calls for every incoming motion frame.
pub fn inject_motion(x: i32, y: i32) -> Result<(), Error> {
    #[cfg(target_os = "windows")]
    {
        windows::inject_motion_verified(x, y)
    }
    #[cfg(target_os = "macos")]
    {
        // macOS has no silent-failure equivalent of UIPI, and warping is both lower
        // latency and unambiguous, so injection is the same operation there.
        macos::warp_cursor(x, y)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (x, y);
        Err(Error::Unsupported("motion injection"))
    }
}

/// Press or release a mouse button on this machine.
pub fn inject_button(button: u8, down: bool) -> Result<(), Error> {
    #[cfg(target_os = "windows")]
    {
        windows::inject_button(button, down)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (button, down);
        Err(Error::Unsupported("button injection"))
    }
}

/// Scroll on this machine.
pub fn inject_scroll(dx: i32, dy: i32) -> Result<(), Error> {
    #[cfg(target_os = "windows")]
    {
        windows::inject_scroll(dx, dy)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (dx, dy);
        Err(Error::Unsupported("scroll injection"))
    }
}

/// Type text on this machine, reproducing the glyph the sender's layout produced.
pub fn inject_text(text: &str, down: bool) -> Result<(), Error> {
    #[cfg(target_os = "windows")]
    {
        windows::inject_text(text, down)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (text, down);
        Err(Error::Unsupported("text injection"))
    }
}

/// What this machine will let seam do, as a human-readable report.
///
/// Used by `seam doctor`. Returns `None` where permissions are not a concept.
#[must_use]
pub fn permission_report() -> Option<Vec<(&'static str, bool, &'static str)>> {
    #[cfg(target_os = "macos")]
    {
        let p = macos::Permissions::check();
        Some(vec![
            (
                "capture input",
                p.can_listen,
                "System Settings > Privacy & Security > Input Monitoring",
            ),
            ("inject input", p.can_post, "System Settings > Privacy & Security > Accessibility"),
        ])
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releasing_input_is_safe_even_when_nothing_was_captured() {
        // The recovery path must never require a prior capture, because it exists
        // precisely for the case where the process that captured is already gone.
        release_input();
        release_input();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reports_its_permission_state() {
        let report = permission_report().expect("macOS has permissions");
        assert_eq!(report.len(), 2);
        for (what, _granted, where_to) in report {
            assert!(!what.is_empty());
            assert!(where_to.contains("System Settings"), "must tell the user where to go");
        }
    }
}
