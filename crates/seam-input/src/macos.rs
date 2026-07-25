//! macOS platform backend.
//!
//! # Safety posture
//!
//! Everything here is FFI into CoreGraphics, so `unsafe` is unavoidable. Two rules apply
//! to every line of it:
//!
//! 1. **Fail open, never closed.** On macOS an active, head-inserted event tap that
//!    suppresses events can — if its Accessibility permission is revoked mid-session —
//!    freeze *all* local input, recoverable only by a hard reboot (`deskflow#9562`). A
//!    leaked tap can also send `WindowServer` into an infinite loop. Every error path here
//!    therefore restores normal input rather than holding on to it. Not forwarding input
//!    is a bug; bricking the user's session is a catastrophe.
//! 2. **Restore OS state on every exit path.** Barrier left this machine with the cursor
//!    disassociated from the mouse after a SIGKILL, stranding the pointer. `CursorGuard`
//!    exists so that state is tied to a value's lifetime rather than to remembering.

#![allow(unsafe_code)]

use core::ffi::c_void;

use crate::Error;
use crate::screen::{Desktop, Display, MM, PixelRect};

type CGDirectDisplayID = u32;
type CGError = i32;
type Boolean = u8;

const K_CG_ERROR_SUCCESS: CGError = 0;
/// Ample: macOS supports far fewer simultaneous displays than this.
const MAX_DISPLAYS: u32 = 32;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

// SAFETY CONTRACT for this block: these are the documented CoreGraphics C signatures.
// Each is a plain C function with no Rust-side invariants; correctness depends only on
// the declarations matching the framework headers, which they do (verified against the
// macOS 26.5 SDK).
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGMainDisplayID() -> CGDirectDisplayID;
    fn CGGetActiveDisplayList(
        max_displays: u32,
        active_displays: *mut CGDirectDisplayID,
        display_count: *mut u32,
    ) -> CGError;
    fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
    fn CGDisplayScreenSize(display: CGDirectDisplayID) -> CGSize;
    fn CGDisplayPixelsWide(display: CGDirectDisplayID) -> usize;
    fn CGDisplayIsMain(display: CGDirectDisplayID) -> Boolean;

    fn CGWarpMouseCursorPosition(new_position: CGPoint) -> CGError;
    fn CGAssociateMouseAndMouseCursorPosition(connected: Boolean) -> CGError;
    fn CGDisplayHideCursor(display: CGDirectDisplayID) -> CGError;
    fn CGDisplayShowCursor(display: CGDirectDisplayID) -> CGError;

    fn CGEventCreate(source: *const c_void) -> *mut c_void;
    fn CGEventGetLocation(event: *mut c_void) -> CGPoint;

    fn CGPreflightListenEventAccess() -> Boolean;
    fn CGRequestListenEventAccess() -> Boolean;
    fn CGPreflightPostEventAccess() -> Boolean;
    fn CGRequestPostEventAccess() -> Boolean;
}

// SAFETY CONTRACT: CFRelease lives in CoreFoundation, not CoreGraphics, and takes
// ownership of one retain count on a valid CF object.
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

/// Enumerate the machine's displays.
pub fn desktop() -> Result<Desktop, Error> {
    let mut ids = [0 as CGDirectDisplayID; MAX_DISPLAYS as usize];
    let mut count: u32 = 0;

    // SAFETY: `ids` has exactly MAX_DISPLAYS elements and that same bound is passed, so
    // CoreGraphics cannot write past it. `count` is a valid, initialised out-parameter.
    let status = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &raw mut count) };
    if status != K_CG_ERROR_SUCCESS {
        return Err(Error::Platform(format!("CGGetActiveDisplayList failed with {status}")));
    }

    let count = (count as usize).min(ids.len());
    let displays = ids[..count].iter().map(|&id| read_display(id)).collect();
    Ok(Desktop::new(displays))
}

fn read_display(id: CGDirectDisplayID) -> Display {
    // SAFETY: `id` came from CGGetActiveDisplayList, so it is a live display id. All four
    // calls are pure reads that return by value and cannot fail for a valid id.
    let (bounds, size_mm, pixels_wide, is_main) = unsafe {
        (
            CGDisplayBounds(id),
            CGDisplayScreenSize(id),
            CGDisplayPixelsWide(id),
            CGDisplayIsMain(id) != 0,
        )
    };

    // CGDisplayBounds is in *points*, while CGDisplayPixelsWide is in backing pixels, so
    // their ratio is the scale factor. Reading it this way avoids depending on the display
    // mode API, which reports differently for mirrored and scaled modes.
    let points_wide = bounds.size.width;
    let scale = if points_wide > 0.0 {
        #[expect(clippy::cast_possible_truncation, reason = "scale is 1.0-3.0 in practice")]
        #[expect(clippy::cast_sign_loss, reason = "both operands are positive")]
        #[expect(clippy::cast_precision_loss, reason = "pixel widths are far under 2^52")]
        {
            ((pixels_wide as f64 / points_wide) * 256.0).round() as u32
        }
    } else {
        256
    };

    Display {
        id,
        pixels: rect_to_pixels(bounds),
        width_mm: mm_to_fixed(size_mm.width),
        height_mm: mm_to_fixed(size_mm.height),
        scale: scale.max(1),
        primary: is_main,
    }
}

#[expect(clippy::cast_possible_truncation, reason = "display coordinates fit in i32")]
fn rect_to_pixels(r: CGRect) -> PixelRect {
    PixelRect::new(
        r.origin.x.round() as i32,
        r.origin.y.round() as i32,
        r.size.width.round() as i32,
        r.size.height.round() as i32,
    )
}

#[expect(clippy::cast_possible_truncation, reason = "physical sizes are small")]
fn mm_to_fixed(mm: f64) -> i32 {
    if mm.is_finite() && mm > 0.0 { (mm * f64::from(MM)).round() as i32 } else { 0 }
}

/// Where the pointer is right now, in global pixel coordinates.
pub fn cursor_position() -> Result<(i32, i32), Error> {
    // SAFETY: CGEventCreate(NULL) is the documented way to make an event from the current
    // state; it returns NULL only on allocation failure, which is checked. The event is
    // released on every path, including the error path.
    unsafe {
        let event = CGEventCreate(core::ptr::null());
        if event.is_null() {
            return Err(Error::Platform("CGEventCreate returned null".into()));
        }
        let location = CGEventGetLocation(event);
        CFRelease(event.cast_const());
        #[expect(clippy::cast_possible_truncation, reason = "screen coordinates fit in i32")]
        Ok((location.x.round() as i32, location.y.round() as i32))
    }
}

/// Move the pointer without generating events.
pub fn warp_cursor(x: i32, y: i32) -> Result<(), Error> {
    let point = CGPoint { x: f64::from(x), y: f64::from(y) };
    // SAFETY: takes a plain by-value struct; out-of-bounds points are clamped by the OS.
    let status = unsafe { CGWarpMouseCursorPosition(point) };
    if status == K_CG_ERROR_SUCCESS {
        Ok(())
    } else {
        Err(Error::Platform(format!("CGWarpMouseCursorPosition failed with {status}")))
    }
}

/// Holds the cursor decoupled from the mouse, and **guarantees reattachment**.
///
/// `CGAssociateMouseAndMouseCursorPosition(false)` is the primitive that freezes the
/// on-screen cursor while the physical mouse still produces deltas — exactly what a KVM
/// needs while the pointer is on another machine. It is also what stranded the pointer on
/// this machine when Barrier was killed: it never reattached.
///
/// Tying it to a guard means every early return, `?`, panic and normal exit reattaches.
/// It does **not** survive `SIGKILL`, which no in-process mechanism can — that is why the
/// goal calls for a supervisor process (R2.1).
#[derive(Debug)]
pub struct CursorGuard {
    hidden: bool,
}

impl CursorGuard {
    /// Decouple the cursor from the mouse, optionally hiding it.
    pub fn detach(hide: bool) -> Result<Self, Error> {
        // SAFETY: both take a plain boolean and affect only process-visible cursor state.
        let status = unsafe { CGAssociateMouseAndMouseCursorPosition(0) };
        if status != K_CG_ERROR_SUCCESS {
            return Err(Error::Platform(format!(
                "could not decouple the cursor from the mouse (error {status})"
            )));
        }
        if hide {
            // SAFETY: the display argument is documented as ignored.
            unsafe { CGDisplayHideCursor(CGMainDisplayID()) };
        }
        Ok(Self { hidden: hide })
    }

    /// Reattach explicitly. Idempotent, and also run by `Drop`.
    pub fn release(self) {
        drop(self);
    }
}

impl Drop for CursorGuard {
    fn drop(&mut self) {
        // SAFETY: reattaching is always valid, and showing the cursor more times than it
        // was hidden is harmless — the counter saturates at visible. Deliberately
        // over-showing: CGDisplayShowCursor is refcounted, and an unbalanced hide leaves
        // the user with no cursor at all, which is far worse than a redundant call.
        unsafe {
            if self.hidden {
                for _ in 0..4 {
                    CGDisplayShowCursor(CGMainDisplayID());
                }
            }
            CGAssociateMouseAndMouseCursorPosition(1);
        }
    }
}

/// Force the cursor back to a sane state regardless of what any previous process did.
///
/// This is the recovery path for the failure actually observed on this machine: another
/// KVM was killed while the cursor was detached, and the pointer was stranded until the
/// association was restored by hand.
pub fn force_restore_cursor() {
    // SAFETY: all three are idempotent and valid at any time. Deliberately ignores errors:
    // this runs on recovery paths where the only wrong move is to give up partway.
    unsafe {
        CGAssociateMouseAndMouseCursorPosition(1);
        for _ in 0..8 {
            CGDisplayShowCursor(CGMainDisplayID());
        }
    }
}

/// What macOS will and will not let this process do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Permissions {
    /// Input Monitoring — required to *observe* input.
    pub can_listen: bool,
    /// The post-events privilege, shown under Accessibility — required to *inject* input.
    pub can_post: bool,
}

impl Permissions {
    #[must_use]
    pub fn check() -> Self {
        // SAFETY: both are documented, argument-free preflight calls with no side effects.
        unsafe {
            Self {
                can_listen: CGPreflightListenEventAccess() != 0,
                can_post: CGPreflightPostEventAccess() != 0,
            }
        }
    }

    /// Ask macOS to prompt for whatever is missing.
    #[must_use]
    ///
    /// The prompt appears once per app identity; afterwards the user must grant it in
    /// System Settings and **restart the app**, because a TCC grant does not apply to an
    /// already-running process.
    pub fn request_missing(self) -> Self {
        // SAFETY: documented request calls; they prompt and return the resulting state.
        unsafe {
            if !self.can_listen {
                CGRequestListenEventAccess();
            }
            if !self.can_post {
                CGRequestPostEventAccess();
            }
        }
        Self::check()
    }

    #[must_use]
    pub const fn all_granted(self) -> bool {
        self.can_listen && self.can_post
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_mac_reports_at_least_one_display() {
        let desktop = desktop().expect("CGGetActiveDisplayList should succeed");
        assert!(!desktop.displays.is_empty(), "a Mac always has a display");
        assert!(desktop.primary().is_some());

        let primary = desktop.primary().unwrap();
        assert!(primary.pixels.width > 0 && primary.pixels.height > 0);
        assert!(primary.scale >= 256, "scale must be at least 1x, got {}", primary.scale);
    }

    #[test]
    fn every_display_reports_a_usable_physical_size() {
        // Desktop::new fills in a nominal size for displays that report none, so nothing
        // downstream can divide by zero.
        for d in desktop().unwrap().displays {
            assert!(d.width_mm > 0, "display {} reported no width", d.id);
            assert!(d.height_mm > 0, "display {} reported no height", d.id);
            assert!(d.density_x().is_some());
        }
    }

    #[test]
    fn the_cursor_is_somewhere_on_the_desktop() {
        let (x, y) = cursor_position().expect("cursor position should be readable");
        let desktop = desktop().unwrap();
        assert!(
            desktop.contains(x, y),
            "cursor at ({x}, {y}) is not on any display: {:?}",
            desktop.bounding_box()
        );
    }

    #[test]
    fn permissions_can_be_checked_without_prompting() {
        // Must never hang or prompt in a test run; it only reports state.
        let p = Permissions::check();
        let _ = p.all_granted();
    }

    #[test]
    fn forcing_a_cursor_restore_is_always_safe() {
        // This is the recovery path, so it must be callable at any time, including when
        // nothing was ever detached.
        force_restore_cursor();
        force_restore_cursor();
        let (x, y) = cursor_position().unwrap();
        assert!(desktop().unwrap().contains(x, y), "cursor lost after restore");
    }
}
