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
        macos::set_suppress_local(false);
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

/// Press or release a key by its physical identity, for keys that produce no text.
pub fn inject_key(usage: u16, down: bool) -> Result<(), Error> {
    #[cfg(target_os = "windows")]
    {
        windows::inject_key(usage, down)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (usage, down);
        Err(Error::Unsupported("key injection"))
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

/// This machine's text clipboard.
///
/// # Why `arboard`
///
/// It handles text and images correctly on every target platform. Its documented gaps are
/// **file lists**: it never writes `CFSTR_PREFERREDDROPEFFECT` on Windows, so a cut cannot
/// be represented, and it omits `x-special/gnome-copied-files` on Linux, so files pasted
/// into a file manager do not arrive. Files therefore need their own implementation later;
/// text does not, and reimplementing three pasteboard APIs to prove a point would be
/// worse, not better.
pub mod clipboard {
    use crate::Error;

    /// Read the clipboard's text, if it currently holds any.
    ///
    /// An empty clipboard, or one holding only an image or files, is `None` rather than an
    /// error: it is a completely normal state and not worth a log line every poll.
    pub fn read_text() -> Result<Option<String>, Error> {
        let mut board = arboard::Clipboard::new()
            .map_err(|e| Error::Platform(format!("clipboard unavailable: {e}")))?;
        match board.get_text() {
            Ok(text) if !text.is_empty() => Ok(Some(text)),
            _ => Ok(None),
        }
    }

    /// Read the clipboard's image, if it currently holds one. RGBA8, row-major.
    ///
    /// `None` for an empty clipboard or one holding text/files — the same contract as
    /// [`read_text`]. The bytes are copied out; an image poll is not free, which is why
    /// the caller only tries this when there is no text.
    pub fn read_image() -> Result<Option<(u32, u32, Vec<u8>)>, Error> {
        let mut board = arboard::Clipboard::new()
            .map_err(|e| Error::Platform(format!("clipboard unavailable: {e}")))?;
        match board.get_image() {
            Ok(image) => {
                let (Ok(width), Ok(height)) =
                    (u32::try_from(image.width), u32::try_from(image.height))
                else {
                    return Ok(None);
                };
                Ok(Some((width, height, image.bytes.into_owned())))
            }
            Err(_) => Ok(None),
        }
    }

    /// Replace the clipboard with an image. RGBA8, row-major, dimensions must match.
    pub fn write_image(width: u32, height: u32, rgba: &[u8]) -> Result<(), Error> {
        let mut board = arboard::Clipboard::new()
            .map_err(|e| Error::Platform(format!("clipboard unavailable: {e}")))?;
        board
            .set_image(arboard::ImageData {
                width: width as usize,
                height: height as usize,
                bytes: rgba.into(),
            })
            .map_err(|e| Error::Platform(format!("could not set the clipboard image: {e}")))
    }

    /// The absolute paths of files currently on the clipboard, if any.
    pub fn read_file_list() -> Result<Option<Vec<std::path::PathBuf>>, Error> {
        #[cfg(target_os = "macos")]
        {
            crate::macos::read_file_list()
        }
        #[cfg(target_os = "windows")]
        {
            crate::windows::read_file_list()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Err(Error::Unsupported("file clipboard"))
        }
    }

    /// Put a list of local files on the clipboard, as the native file manager would.
    pub fn write_file_list(paths: &[std::path::PathBuf]) -> Result<(), Error> {
        #[cfg(target_os = "macos")]
        {
            crate::macos::write_file_list(paths)
        }
        #[cfg(target_os = "windows")]
        {
            crate::windows::write_file_list(paths)
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = paths;
            Err(Error::Unsupported("file clipboard"))
        }
    }

    /// Replace the clipboard's text.
    pub fn write_text(text: &str) -> Result<(), Error> {
        let mut board = arboard::Clipboard::new()
            .map_err(|e| Error::Platform(format!("clipboard unavailable: {e}")))?;
        board
            .set_text(text.to_owned())
            .map_err(|e| Error::Platform(format!("could not set the clipboard: {e}")))
    }
}

#[cfg(test)]
mod clipboard_tests {
    use super::clipboard;

    #[cfg(target_os = "macos")]
    #[test]
    fn a_file_list_round_trips_through_the_real_pasteboard() {
        let dir = std::env::temp_dir().join("seam-file-clip-test");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("seam sample — پرونده.txt");
        std::fs::write(&file, b"clipboard file round trip").unwrap();

        crate::macos::write_file_list(std::slice::from_ref(&file)).unwrap();
        let read = crate::macos::read_file_list().unwrap();
        assert_eq!(
            read.as_deref(),
            Some(std::slice::from_ref(&file)),
            "the path must survive URL encoding, spaces and non-Latin letters intact"
        );
    }

    #[test]
    fn an_image_round_trips_through_the_real_clipboard() {
        // Restores nothing: images replace the clipboard, same as a real screenshot.
        let pixels: Vec<u8> = (0..16).flat_map(|i| [i * 16, 255 - i * 16, 128, 255]).collect();
        if clipboard::write_image(4, 4, &pixels).is_err() {
            eprintln!("skipped: clipboard is not writable here");
            return;
        }
        match clipboard::read_image() {
            Ok(Some((w, h, bytes))) => {
                assert_eq!((w, h), (4, 4), "dimensions must survive the pasteboard");
                assert_eq!(bytes.len(), 64, "RGBA byte count must match 4x4");
            }
            other => panic!("wrote an image but read back {other:?}"),
        }
    }

    #[test]
    fn text_round_trips_through_the_real_clipboard() {
        // Restores whatever was there, so running the tests does not eat the user's
        // clipboard.
        let Ok(before) = clipboard::read_text() else {
            eprintln!("skipped: no clipboard on this machine");
            return;
        };

        // Non-Latin and composed characters, because a clipboard that mangles them is
        // useless on this project's own fleet.
        let sample = "سلام — hello — @€ 🙂";
        if clipboard::write_text(sample).is_err() {
            eprintln!("skipped: clipboard is not writable here");
            return;
        }
        assert_eq!(clipboard::read_text().unwrap().as_deref(), Some(sample));

        if let Some(before) = before {
            let _ = clipboard::write_text(&before);
        }
    }
}
