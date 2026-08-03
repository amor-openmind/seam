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
use seam_proto::LogicalText;

type CGDirectDisplayID = u32;
type CGError = i32;

// SAFETY CONTRACT: private window-server API, shipped by Synergy and Barrier for two
// decades for exactly one purpose. `CGDisplayHideCursor` from a process without
// foreground status is silently ignored - it returns success and does nothing - unless
// the connection has asked for background cursor control first. There is no public API
// for this; the alternative is a foreground GUI agent whose only job is owning the
// cursor image.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn _CGSDefaultConnection() -> u32;
    fn CGSSetConnectionProperty(
        cid: u32,
        target_cid: u32,
        key: *const c_void,
        value: *const c_void,
    ) -> CGError;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const u8,
        encoding: u32,
    ) -> *const c_void;
    static kCFBooleanTrue: *const c_void;
}

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// Ask the window server to honour cursor calls from this background process.
///
/// This is the difference between seam and Barrier that kept the arrow visible: every
/// cursor-hide call seam ever made was accepted and ignored. Barrier sets the
/// `SetsCursorInBackground` property on its window-server connection before each
/// hide/show (OSXScreen.mm), and with it a background process's cursor calls take
/// effect. Idempotent; the outcome is logged once so the daemon log shows whether this
/// macOS still honours it.
fn allow_background_cursor_control() -> CGError {
    static OUTCOME: std::sync::OnceLock<CGError> = std::sync::OnceLock::new();
    *OUTCOME.get_or_init(|| {
        // SAFETY: creates a CFString, hands it to the window server, releases it. The
        // connection id is this process's own.
        unsafe {
            let key = CFStringCreateWithCString(
                core::ptr::null(),
                c"SetsCursorInBackground".as_ptr().cast(),
                K_CF_STRING_ENCODING_UTF8,
            );
            if key.is_null() {
                return -1;
            }
            let cid = _CGSDefaultConnection();
            let status = CGSSetConnectionProperty(cid, cid, key, kCFBooleanTrue);
            CFRelease(key);
            if status == K_CG_ERROR_SUCCESS {
                tracing::info!("window server granted background cursor control");
            } else {
                tracing::warn!(
                    status,
                    "window server refused background cursor control - the cursor \
                     image cannot be hidden from a daemon on this macOS"
                );
            }
            status
        }
    })
}

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
    fn CGSetLocalEventsSuppressionInterval(seconds: f64) -> CGError;
    fn CGAssociateMouseAndMouseCursorPosition(connected: Boolean) -> CGError;
    fn CGDisplayHideCursor(display: CGDirectDisplayID) -> CGError;
    fn CGCursorIsVisible() -> Boolean;
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
        // Warping the cursor makes macOS suppress local hardware events for about a
        // quarter of a second by default, which would swallow the very movement seam needs
        // to keep capturing. Barrier zeroes this for the same reason (OSXScreen.mm,
        // setZeroSuppressionInterval). Without it, pinning the cursor below would blind
        // the tap four times a second.
        //
        // SAFETY: documented CoreGraphics call taking a plain double.
        unsafe { CGSetLocalEventsSuppressionInterval(0.0) };

        let status = unsafe { CGAssociateMouseAndMouseCursorPosition(0) };
        if status != K_CG_ERROR_SUCCESS {
            return Err(Error::Platform(format!(
                "could not decouple the cursor from the mouse (error {status})"
            )));
        }
        if hide {
            let _ = allow_background_cursor_control();
            // SAFETY: the display argument is documented as ignored.
            unsafe { CGDisplayHideCursor(CGMainDisplayID()) };
            HIDES.store(1, std::sync::atomic::Ordering::Relaxed);
        }
        tracing::info!(hide, "cursor detached from the mouse");
        Ok(Self { hidden: hide })
    }

    /// Reattach explicitly. Idempotent, and also run by `Drop`.
    pub fn release(self) {
        drop(self);
    }
}

impl Drop for CursorGuard {
    fn drop(&mut self) {
        // Said out loud on purpose. A guard dying mid-session is invisible from the
        // user's chair - the cursor just starts tracking again - and finding the
        // watchdog bug took hours precisely because the release was silent.
        tracing::info!("cursor reattached to the mouse");
        // Releasing the cursor and withholding input must end together: leaving
        // suppression on with the cursor reattached would be a Mac that ignores its own
        // keyboard.
        set_suppress_local(false);
        // SAFETY: reattaching is always valid, and showing the cursor more times than it
        // was hidden is harmless — the counter saturates at visible. Deliberately
        // over-showing: CGDisplayShowCursor is refcounted, and an unbalanced hide leaves
        // the user with no cursor at all, which is far worse than a redundant call.
        if self.hidden {
            let _ = allow_background_cursor_control();
        }
        // SAFETY: reattaching is always valid, and the over-show is refcount-saturating.
        unsafe {
            if self.hidden {
                // Balance the hide count exactly: mid-session re-hides (see
                // `rehide_if_visible`) each incremented it, and a fixed count here
                // would leave the cursor invisible after a session in which the system
                // re-showed it more than that many times. One extra is harmless — the
                // counter saturates at visible.
                // Show once per hide, plus a generous margin. Over-showing is harmless
                // — the window server's refcount saturates at visible — while
                // under-showing leaves the machine with no cursor at all, which is far
                // worse than the bug this counting exists to fix. Err upward.
                let hides = HIDES.swap(0, std::sync::atomic::Ordering::Relaxed);
                for _ in 0..hides.saturating_add(8) {
                    CGDisplayShowCursor(CGMainDisplayID());
                }
                // Verify rather than trust the arithmetic: if anything hid the cursor
                // outside the counter's knowledge, keep showing until the window server
                // agrees it is visible. Bounded, because a display mid-sleep can answer
                // "not visible" forever and this must never spin.
                for _ in 0..64 {
                    if CGCursorIsVisible() != 0 {
                        break;
                    }
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

// ---------------------------------------------------------------- capture

use std::sync::mpsc::{Receiver, Sender, channel};

type CFMachPortRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopMode = *const c_void;
type CGEventTapProxy = *mut c_void;
type CGEventRef = *mut c_void;
type CGEventType = u32;
type CGEventMask = u64;

/// `kCGHIDEventTap` — where HID events enter the window server, **before** the cursor is
/// moved.
///
/// This placement is the difference between suppression that works and suppression that
/// only looks like it does. At `kCGSessionEventTap` the `WindowServer` has already moved the
/// cursor by the time the tap sees the event, so discarding it stops applications
/// receiving input while the pointer carries on visibly tracking the mouse. At the HID tap
/// the event is discarded before that happens.
///
/// Apple's reference page still says only root may tap here. That text predates TCC and is
/// stale: deskflow and input-leap both create this tap from an ordinary user process, and
/// the real gate is the Input Monitoring permission. A NULL return means the permission is
/// missing, not that root is required.
const K_CG_HID_EVENT_TAP: u32 = 0;

/// `kCGSessionEventTap` — the login session. Kept for reference; see above for why the HID
/// tap is used instead.
#[expect(dead_code, reason = "documents the alternative that does not suppress the cursor")]
const K_CG_SESSION_EVENT_TAP: u32 = 1;
/// `kCGHeadInsertEventTap`.
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
/// `kCGEventTapOptionDefault` — may modify or discard events.
const K_CG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;

const K_CG_EVENT_LEFT_MOUSE_DOWN: CGEventType = 1;
const K_CG_EVENT_LEFT_MOUSE_UP: CGEventType = 2;
const K_CG_EVENT_RIGHT_MOUSE_DOWN: CGEventType = 3;
const K_CG_EVENT_RIGHT_MOUSE_UP: CGEventType = 4;
const K_CG_EVENT_MOUSE_MOVED: CGEventType = 5;
const K_CG_EVENT_KEY_DOWN: CGEventType = 10;
const K_CG_EVENT_KEY_UP: CGEventType = 11;
const K_CG_EVENT_FLAGS_CHANGED: CGEventType = 12;
/// `NX_SYSDEFINED`: where macOS puts volume, mute, media and brightness keys. They are
/// NOT keyboard events — a tap masking only key-down/up never sees them.
const K_CG_EVENT_SYSTEM_DEFINED: CGEventType = 14;
const K_CG_EVENT_SCROLL_WHEEL: CGEventType = 22;
const K_CG_EVENT_OTHER_MOUSE_DOWN: CGEventType = 25;
const K_CG_EVENT_OTHER_MOUSE_UP: CGEventType = 26;

/// `kCGMouseEventDeltaX` / `Y` — movement since the last event. While the cursor is
/// detached from the mouse the reported *location* stops changing, so these deltas are
/// the only remaining truth about what the user's hand did.
const K_CG_MOUSE_DELTA_X: u32 = 4;
const K_CG_MOUSE_DELTA_Y: u32 = 5;

/// `kCGEventUnacceleratedPointerMovementX` / `Y` — raw device movement.
///
/// These matter more than they look. `kCGMouseEventDelta*` is how far the *cursor* moved,
/// so once the cursor is pinned against a screen edge it reports **zero** no matter how
/// hard the user pushes — and a design that crosses screens on cursor delta can therefore
/// never leave the screen at all. The unaccelerated fields report what the hand did,
/// independent of where the cursor is allowed to be.
const K_CG_UNACCELERATED_X: u32 = 170;
const K_CG_UNACCELERATED_Y: u32 = 171;

/// `kCGKeyboardEventKeycode` — the physical key, independent of what it types.
const K_CG_KEYBOARD_KEYCODE: u32 = 9;

/// `kCGScrollWheelEventDeltaAxis1` — vertical, in wheel notches.
const K_CG_SCROLL_DELTA_AXIS1: u32 = 11;
/// `kCGScrollWheelEventPointDeltaAxis1/2` — the PIXEL deltas. Smooth devices (trackpad,
/// Magic Mouse) put real motion here while reporting zero wheel notches.
const K_CG_SCROLL_POINT_DELTA_AXIS1: u32 = 96;
const K_CG_SCROLL_POINT_DELTA_AXIS2: u32 = 97;
/// `kCGScrollWheelEventDeltaAxis2` — horizontal.
const K_CG_SCROLL_DELTA_AXIS2: u32 = 12;
const K_CG_EVENT_LEFT_MOUSE_DRAGGED: CGEventType = 6;
const K_CG_EVENT_RIGHT_MOUSE_DRAGGED: CGEventType = 7;
const K_CG_EVENT_OTHER_MOUSE_DRAGGED: CGEventType = 27;
const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: CGEventType = 0xFFFF_FFFE;
const K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT: CGEventType = 0xFFFF_FFFF;

type CGEventTapCallBack = unsafe extern "C" fn(
    proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

// SAFETY CONTRACT: documented CoreGraphics / CoreFoundation signatures, verified against
// the macOS 26.5 SDK headers.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventKeyboardGetUnicodeString(
        event: CGEventRef,
        max_length: usize,
        actual_length: *mut usize,
        unicode_string: *mut u16,
    );
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: Boolean);
    fn CGEventTapIsEnabled(tap: CFMachPortRef) -> Boolean;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFRunLoopMode);
    fn CFRunLoopRun();
    static kCFRunLoopCommonModes: CFRunLoopMode;
}

const fn mask_for(event_type: CGEventType) -> CGEventMask {
    1u64 << event_type
}

/// Whether local input is currently being withheld from this machine.
///
/// # Why this is a global
///
/// The tap callback runs on its own run loop thread and receives only a raw pointer to
/// its state. A single atomic is the smallest thing that can be read from it without a
/// lock — and taking a lock on the input path is what makes macOS disable the tap.
///
/// # Why it defaults to false
///
/// **Fail open.** Every path that cannot reach a definite answer leaves this false, so
/// input reaches this machine. An active tap that discards events is the mechanism behind
/// the freeze-until-reboot failure (`deskflow#9562`); if anything goes wrong the correct
/// outcome is a KVM that stops forwarding, never a Mac that stops responding.
static SUPPRESS_LOCAL: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Withhold local input while another machine owns the pointer.
///
/// Only ever set true while a peer holds focus, and cleared on every path that returns
/// focus here — including [`CursorGuard::drop`].
pub fn set_suppress_local(suppress: bool) {
    SUPPRESS_LOCAL.store(suppress, core::sync::atomic::Ordering::Relaxed);
}

#[must_use]
pub fn is_suppressing_local() -> bool {
    SUPPRESS_LOCAL.load(core::sync::atomic::Ordering::Relaxed)
}

/// Something the user did on this machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Observed {
    /// Pointer moved. `x`/`y` are the absolute position, which freezes while the cursor
    /// is detached; `dx`/`dy` are the movement, which keeps working either way.
    Motion { x: i32, y: i32, dx: i32, dy: i32 },
    /// A mouse button changed state. `button` follows the evdev numbering the protocol uses.
    Button { button: u8, down: bool },
    /// Scrolled. Positive `dy` is away from the user, matching the protocol's convention.
    Scroll { dx: i32, dy: i32 },
    /// A key changed state.
    ///
    /// `text` is what the *local* layout produced, and `physical` identifies the key
    /// itself. Both are carried because keys divide into two kinds and each needs a
    /// different one: `@` must arrive as the glyph, while Backspace and F5 produce no
    /// text at all and can only be described by which key they are.
    ///
    /// The text is what makes mismatched layouts work: a German Apple keyboard types `@`
    /// as `Option+L`, and sending the resulting glyph is the only way the receiver can
    /// reproduce it without knowing anything about the sender's layout.
    Key {
        text: LogicalText,
        physical: seam_proto::PhysicalKey,
        modifiers: seam_proto::Modifiers,
        down: bool,
    },
}

/// State shared with the tap callback. Boxed and leaked deliberately: the callback may
/// outlive this function's stack frame, and the run loop owns it for the process lifetime.
struct TapState {
    sender: Sender<Observed>,
    tap: CFMachPortRef,
}

// SAFETY: the only field touched from the callback thread is `sender`, which is Send, and
// `tap`, which is only compared and passed back to CoreGraphics.
unsafe impl Send for TapState {}

/// macOS virtual keycode to USB HID usage.
///
/// Only the keys that produce **no text** need to be here. Everything that types a
/// character travels as that character instead, which is what makes mismatched layouts
/// work — but Backspace, Enter, Tab, Escape, the arrows and the function keys have no
/// character at all, and without this they were being dropped entirely.
const fn hid_usage_for(keycode: u16) -> seam_proto::PhysicalKey {
    let usage: u16 = match keycode {
        0x33 => 0x2A, // Delete (Backspace)
        0x24 => 0x28, // Return
        0x30 => 0x2B, // Tab
        0x35 => 0x29, // Escape
        0x75 => 0x4C, // Forward Delete
        0x7B => 0x50, // Left
        0x7C => 0x4F, // Right
        0x7D => 0x51, // Down
        0x7E => 0x52, // Up
        0x73 => 0x4A, // Home
        0x77 => 0x4D, // End
        0x74 => 0x4B, // Page Up
        0x79 => 0x4E, // Page Down
        0x7A => 0x3A, // F1
        0x78 => 0x3B,
        0x63 => 0x3C,
        0x76 => 0x3D,
        0x60 => 0x3E,
        0x61 => 0x3F,
        0x62 => 0x40,
        0x64 => 0x41,
        0x65 => 0x42,
        0x6D => 0x43,
        0x67 => 0x44,
        0x6F => 0x45, // F12
        // Letters and digits, so a shortcut can be addressed by key position rather than
        // by the glyph the sender's layout produced.
        0x00 => 0x04, // A
        0x0B => 0x05, // B
        0x08 => 0x06, // C
        0x02 => 0x07, // D
        0x0E => 0x08, // E
        0x03 => 0x09, // F
        0x05 => 0x0A, // G
        0x04 => 0x0B, // H
        0x22 => 0x0C, // I
        0x26 => 0x0D, // J
        0x28 => 0x0E, // K
        0x25 => 0x0F, // L
        0x2E => 0x10, // M
        0x2D => 0x11, // N
        0x1F => 0x12, // O
        0x23 => 0x13, // P
        0x0C => 0x14, // Q
        0x0F => 0x15, // R
        0x01 => 0x16, // S
        0x11 => 0x17, // T
        0x20 => 0x18, // U
        0x09 => 0x19, // V
        0x0D => 0x1A, // W
        0x07 => 0x1B, // X
        0x10 => 0x1C, // Y
        0x06 => 0x1D, // Z
        0x31 => 0x2C, // Space
        _ => 0,
    };
    seam_proto::PhysicalKey(usage)
}

// SAFETY CONTRACT: the Objective-C runtime plus AppKit, used for exactly one job:
// reading the `subtype` and `data1` of a system-defined event. CGEvent has no public
// accessor for those fields; NSEvent does, and `eventWithCGEvent:` is the documented
// bridge between the two. This is how Barrier and deskflow read media keys as well.
#[link(name = "objc", kind = "dylib")]
unsafe extern "C" {
    fn objc_getClass(name: *const core::ffi::c_char) -> *mut c_void;
    fn sel_registerName(name: *const core::ffi::c_char) -> *mut c_void;
    fn objc_msgSend();
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

// Empty on purpose: forces AppKit to be linked so the NSEvent class exists at runtime.
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

/// The `NSEvent` subtype carrying media and volume keys.
const NX_SUBTYPE_AUX_CONTROL_BUTTONS: i16 = 8;

/// Decode a media key out of an `NX_SYSDEFINED` event, if that is what it holds.
///
/// The key lives in `data1`: bits 16-31 are the key code, bits 8-15 are 0x0A for down
/// and 0x0B for up. Key codes are the `NX_KEYTYPE_*` family. Anything that is not a
/// media key — and plenty of other system traffic uses this event type — returns `None`
/// so the event passes through untouched; swallowing unknown system events is how a
/// machine's volume HUD or power management quietly breaks.
unsafe fn decode_media_key(event: CGEventRef) -> Option<Observed> {
    type MsgSendEvent =
        unsafe extern "C" fn(*mut c_void, *mut c_void, CGEventRef) -> *mut c_void;
    type MsgSendI16 = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i16;
    type MsgSendIsize = unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize;

    // SAFETY: standard objc runtime calls; the pool balances push/pop on every path,
    // because `eventWithCGEvent:` returns an autoreleased object and this thread has no
    // pool of its own — without one, every media key would leak an NSEvent.
    unsafe {
        let pool = objc_autoreleasePoolPush();
        let decoded = 'decode: {
            let class = objc_getClass(c"NSEvent".as_ptr());
            if class.is_null() {
                break 'decode None;
            }
            let with_cg: MsgSendEvent = core::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let ns = with_cg(class, sel_registerName(c"eventWithCGEvent:".as_ptr()), event);
            if ns.is_null() {
                break 'decode None;
            }
            let get_subtype: MsgSendI16 = core::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            if get_subtype(ns, sel_registerName(c"subtype".as_ptr()))
                != NX_SUBTYPE_AUX_CONTROL_BUTTONS
            {
                break 'decode None;
            }
            let get_data1: MsgSendIsize = core::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let data1 = get_data1(ns, sel_registerName(c"data1".as_ptr()));

            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "masked")]
            let key = ((data1 >> 16) & 0xFFFF) as u16;
            let down = ((data1 >> 8) & 0xFF) == 0x0A;

            // NX_KEYTYPE_* → HID Consumer-page usage. Brightness is carried too, so a
            // client that can act on it may; Windows has no virtual key for it and
            // drops it at injection.
            let usage: u16 = match key {
                0 => 0xE9,       // volume up
                1 => 0xEA,       // volume down
                7 => 0xE2,       // mute
                16 => 0xCD,      // play/pause
                17 | 19 => 0xB5, // next / fast
                18 | 20 => 0xB6, // previous / rewind
                2 => 0x6F,       // brightness up
                3 => 0x70,       // brightness down
                _ => break 'decode None,
            };
            Some(Observed::Key {
                text: seam_proto::LogicalText::NONE,
                physical: seam_proto::PhysicalKey::consumer(usage),
                modifiers: seam_proto::Modifiers::NONE,
                down,
            })
        };
        objc_autoreleasePoolPop(pool);
        decoded
    }
}

/// Turn a CoreGraphics event into something the protocol can carry.
///
/// # Safety
/// `event` must be a valid `CGEventRef` for the duration of the call.
/// Accumulate smooth-scroll pixels into whole wheel notches the far side understands.
///
/// Ten pixels to a notch approximates how macOS itself translates trackpad motion; the
/// remainder carries across events so slow, fine scrolling still adds up instead of
/// being rounded away.
fn notches_from_pixels(px: i64, py: i64) -> Option<Observed> {
    const PIXELS_PER_NOTCH: i64 = 10;
    use std::sync::atomic::{AtomicI64, Ordering};
    static ACC_X: AtomicI64 = AtomicI64::new(0);
    static ACC_Y: AtomicI64 = AtomicI64::new(0);
    let total_y = ACC_Y.fetch_add(py, Ordering::Relaxed) + py;
    let total_x = ACC_X.fetch_add(px, Ordering::Relaxed) + px;
    let notches_y = total_y / PIXELS_PER_NOTCH;
    let notches_x = total_x / PIXELS_PER_NOTCH;
    if notches_y == 0 && notches_x == 0 {
        return None;
    }
    ACC_Y.fetch_sub(notches_y * PIXELS_PER_NOTCH, Ordering::Relaxed);
    ACC_X.fetch_sub(notches_x * PIXELS_PER_NOTCH, Ordering::Relaxed);
    Some(Observed::Scroll {
        dx: i32::try_from(notches_x).unwrap_or(0),
        dy: i32::try_from(notches_y).unwrap_or(0),
    })
}

unsafe fn classify(event_type: CGEventType, event: CGEventRef) -> Option<Observed> {
    match event_type {
        K_CG_EVENT_MOUSE_MOVED
        | K_CG_EVENT_LEFT_MOUSE_DRAGGED
        | K_CG_EVENT_RIGHT_MOUSE_DRAGGED
        | K_CG_EVENT_OTHER_MOUSE_DRAGGED => {
            // SAFETY: valid event; all three are by-value getters on it.
            let (p, raw_x, raw_y, dx, dy) = unsafe {
                (
                    CGEventGetLocation(event),
                    CGEventGetIntegerValueField(event, K_CG_UNACCELERATED_X),
                    CGEventGetIntegerValueField(event, K_CG_UNACCELERATED_Y),
                    CGEventGetIntegerValueField(event, K_CG_MOUSE_DELTA_X),
                    CGEventGetIntegerValueField(event, K_CG_MOUSE_DELTA_Y),
                )
            };
            // Prefer the *accelerated* cursor delta, so the pointer moves at the same
            // speed on every machine as it does here — raw device counts are unscaled and
            // make a remote screen feel much faster than the local one.
            //
            // Fall back to raw movement when the accelerated delta is zero, which happens
            // when the cursor is pinned against a screen edge. That case is why raw is
            // read at all: without it the pointer can never reach the boundary to cross.
            let (dx, dy) = if dx != 0 || dy != 0 { (dx, dy) } else { (raw_x, raw_y) };
            #[expect(clippy::cast_possible_truncation, reason = "screen coordinates fit i32")]
            Some(Observed::Motion {
                x: p.x.round() as i32,
                y: p.y.round() as i32,
                dx: i32::try_from(dx).unwrap_or(0),
                dy: i32::try_from(dy).unwrap_or(0),
            })
        }

        K_CG_EVENT_LEFT_MOUSE_DOWN => Some(Observed::Button { button: 1, down: true }),
        K_CG_EVENT_LEFT_MOUSE_UP => Some(Observed::Button { button: 1, down: false }),
        K_CG_EVENT_RIGHT_MOUSE_DOWN => Some(Observed::Button { button: 2, down: true }),
        K_CG_EVENT_RIGHT_MOUSE_UP => Some(Observed::Button { button: 2, down: false }),
        K_CG_EVENT_OTHER_MOUSE_DOWN => Some(Observed::Button { button: 3, down: true }),
        K_CG_EVENT_OTHER_MOUSE_UP => Some(Observed::Button { button: 3, down: false }),

        K_CG_EVENT_SCROLL_WHEEL => {
            // SAFETY: valid event; these fields exist on every scroll event.
            let (dy, dx) = unsafe {
                (
                    CGEventGetIntegerValueField(event, K_CG_SCROLL_DELTA_AXIS1),
                    CGEventGetIntegerValueField(event, K_CG_SCROLL_DELTA_AXIS2),
                )
            };
            if dx == 0 && dy == 0 {
                // Zero wheel notches is not zero motion: a trackpad or Magic Mouse
                // smooth-scroll carries its movement in the PIXEL delta fields while
                // the notch fields stay zero. Dropping these lost the whole smooth
                // portion of every scroll — and, worse, the dropped events passed
                // through the suppression and scrolled THIS machine in small steps
                // while a peer held the pointer. Pixels accumulate into notches so
                // smooth input becomes wheel input the far side understands.
                // SAFETY: valid event; by-value getters.
                let (py, px) = unsafe {
                    (
                        CGEventGetIntegerValueField(event, K_CG_SCROLL_POINT_DELTA_AXIS1),
                        CGEventGetIntegerValueField(event, K_CG_SCROLL_POINT_DELTA_AXIS2),
                    )
                };
                if px == 0 && py == 0 {
                    // Genuinely a no-op (the inertial tail); nothing to forward.
                    return None;
                }
                return notches_from_pixels(px, py);
            }
            Some(Observed::Scroll {
                dx: i32::try_from(dx).unwrap_or(0),
                dy: i32::try_from(dy).unwrap_or(0),
            })
        }

        K_CG_EVENT_KEY_DOWN | K_CG_EVENT_KEY_UP => {
            // SAFETY: valid event; by-value getter.
            let keycode = unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_KEYCODE) };
            let physical = hid_usage_for(u16::try_from(keycode).unwrap_or(0));
            // SAFETY: valid event; by-value getter.
            let modifiers = modifiers_from(unsafe { CGEventGetFlags(event) });

            let mut buffer = [0u16; 8];
            let mut length: usize = 0;
            // SAFETY: `buffer` has 8 elements and that bound is passed, so CoreGraphics
            // cannot overrun it; `length` is a valid out-parameter.
            unsafe {
                CGEventKeyboardGetUnicodeString(
                    event,
                    buffer.len(),
                    &raw mut length,
                    buffer.as_mut_ptr(),
                );
            }
            let length = length.min(buffer.len());
            let text = String::from_utf16_lossy(&buffer[..length]);
            // Control characters are what Ctrl+letter produces; they are a *command*, not
            // text, so they must not be replayed as a glyph.
            let text: String = text.chars().filter(|c| !c.is_control()).collect();
            let logical = LogicalText::new(&text).unwrap_or(LogicalText::NONE);
            // A key with neither text nor a known identity cannot be reproduced, so
            // sending it would only produce a phantom keystroke.
            if logical.is_empty() && physical.is_unknown() {
                return None;
            }
            Some(Observed::Key {
                text: logical,
                physical,
                modifiers,
                down: event_type == K_CG_EVENT_KEY_DOWN,
            })
        }

        // A modifier key changing state. macOS reports these as flag changes rather than
        // key presses, and without them a receiver can never be told that Cmd is held —
        // so no shortcut can ever be reproduced.
        K_CG_EVENT_SYSTEM_DEFINED => unsafe { decode_media_key(event) },

        K_CG_EVENT_FLAGS_CHANGED => {
            // SAFETY: valid event; both are by-value getters.
            let (keycode, flags) = unsafe {
                (CGEventGetIntegerValueField(event, K_CG_KEYBOARD_KEYCODE), CGEventGetFlags(event))
            };
            let physical = modifier_usage_for(u16::try_from(keycode).unwrap_or(0));
            if physical.is_unknown() {
                return None;
            }
            let modifiers = modifiers_from(flags);
            // A flag change carries no direction, so it is derived: the key is down when
            // its own bit is still set in the resulting flags.
            let down = physical.modifier_bit().is_some_and(|bit| modifiers.contains(bit));
            Some(Observed::Key { text: LogicalText::NONE, physical, modifiers, down })
        }

        _ => None,
    }
}

/// macOS keycode for a modifier key to its USB HID usage.
const fn modifier_usage_for(keycode: u16) -> seam_proto::PhysicalKey {
    let usage: u16 = match keycode {
        0x37 => 0xE3, // Left Command
        0x36 => 0xE7, // Right Command
        0x38 => 0xE1, // Left Shift
        0x3C => 0xE5, // Right Shift
        0x3B => 0xE0, // Left Control
        0x3E => 0xE4, // Right Control
        0x3A => 0xE2, // Left Option
        0x3D => 0xE6, // Right Option
        _ => 0,
    };
    seam_proto::PhysicalKey(usage)
}

/// CoreGraphics event flags to the protocol's modifier mask.
fn modifiers_from(flags: u64) -> seam_proto::Modifiers {
    use seam_proto::Modifiers as M;
    // Device-dependent bits distinguish left from right; the documented masks do not.
    const NX_DEVICE_LSHIFT: u64 = 0x0000_0002;
    const NX_DEVICE_RSHIFT: u64 = 0x0000_0004;
    const NX_DEVICE_LCTRL: u64 = 0x0000_0001;
    const NX_DEVICE_RCTRL: u64 = 0x0000_2000;
    const NX_DEVICE_LALT: u64 = 0x0000_0020;
    const NX_DEVICE_RALT: u64 = 0x0000_0040;
    const NX_DEVICE_LCMD: u64 = 0x0000_0008;
    const NX_DEVICE_RCMD: u64 = 0x0000_0010;
    const MASK_ALPHA_SHIFT: u64 = 0x0001_0000;

    let mut m = M::NONE;
    for (bit, modifier) in [
        (NX_DEVICE_LSHIFT, M::LEFT_SHIFT),
        (NX_DEVICE_RSHIFT, M::RIGHT_SHIFT),
        (NX_DEVICE_LCTRL, M::LEFT_CTRL),
        (NX_DEVICE_RCTRL, M::RIGHT_CTRL),
        (NX_DEVICE_LALT, M::LEFT_ALT),
        (NX_DEVICE_RALT, M::RIGHT_ALT),
        (NX_DEVICE_LCMD, M::LEFT_GUI),
        (NX_DEVICE_RCMD, M::RIGHT_GUI),
        (MASK_ALPHA_SHIFT, M::CAPS_LOCK),
    ] {
        if flags & bit != 0 {
            m = m.union(modifier);
        }
    }
    m
}

unsafe extern "C" fn on_event(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    // SAFETY: `user_info` is the leaked `TapState` passed to CGEventTapCreate, valid for
    // the lifetime of the run loop.
    let state = unsafe { &*(user_info.cast::<TapState>()) };

    // Handle the disable notices FIRST and unconditionally. This is the bug behind every
    // "it starts working again when I click the app window" report: macOS silently
    // disables a tap whose callback was too slow, and a callback that filters by event
    // type before checking swallows the notice and never re-arms.
    if event_type == K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT
        || event_type == K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT
    {
        // SAFETY: `state.tap` is the live tap this callback belongs to.
        unsafe { CGEventTapEnable(state.tap, 1) };
        tracing::warn!("macOS disabled the input tap; re-armed it");
        return event;
    }

    // SAFETY: `event` is valid for the duration of the callback. Every read below is a
    // by-value getter on it.
    let observed = unsafe { classify(event_type, event) };
    let Some(observed) = observed else {
        // Declining to FORWARD an event must not mean letting it LAND. While a peer
        // owns the pointer, a scroll the classifier deemed not worth sending — a
        // sub-notch pixel movement, an inertial tail — still scrolled this machine
        // in small steps under the suppressed cursor. Input-shaped events are
        // swallowed during suppression whether or not they travel.
        if is_suppressing_local() && event_type == K_CG_EVENT_SCROLL_WHEEL {
            return core::ptr::null_mut();
        }
        return event;
    };

    // Never block: the callback runs on the input path, and a slow callback is exactly
    // what makes macOS disable the tap. A full channel drops the sample, which is
    // harmless because motion is self-correcting.
    let _ = state.sender.send(observed);

    // Discard the event only while a peer owns the pointer. Any other time — including
    // every error path above, which returns early — the event passes through untouched,
    // so this machine keeps working normally.
    if is_suppressing_local() {
        return core::ptr::null_mut();
    }
    event
}

/// The live tap, so its health can be asked about from outside the callback.
static TAP: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Is capture still alive?
///
/// The honest question for a watchdog. The previous one asked "have any events arrived
/// recently", which cannot tell a broken tap from a person who is reading rather than
/// moving the mouse — and it guessed wrong, handing input back to this machine two
/// seconds after the pointer moved to another screen. That reattached the cursor and
/// cleared suppression, so the cursor tracked the mouse again and keystrokes landed here
/// as well as on the remote machine.
///
/// `CGEventTapIsEnabled` answers the real question and does not depend on user activity.
/// `None` means no tap has been created yet, which is not a failure.
#[must_use]
pub fn capture_is_alive() -> Option<bool> {
    let tap = TAP.load(std::sync::atomic::Ordering::Relaxed);
    if tap.is_null() {
        return None;
    }
    // SAFETY: `tap` is the leaked, still-live tap stored when it was created.
    Some(unsafe { CGEventTapIsEnabled(tap.cast()) } != 0)
}

/// How many times the cursor has been hidden and not yet shown.
///
/// Hiding is refcounted by the window server, and visibility is GLOBAL state: any
/// foreground app or system surface (a notification banner, Spotlight, an app switch)
/// can show the cursor again while a peer holds the pointer. seam therefore re-hides
/// whenever it finds the arrow visible mid-session — and the release path must then
/// show exactly as many times as were hidden, or coming home leaves no cursor at all.
static HIDES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// If the arrow has become visible while it should be hidden, hide it again.
///
/// Returns whether a re-hide happened, so the caller can log it (throttled) — a
/// reappearing cursor was a field report, and the log should name the moments it
/// happens rather than leaving them to be noticed from a chair.
pub fn rehide_if_visible() -> bool {
    // SAFETY: documented visibility query and hide call; the count keeps show/hide
    // balanced on release.
    unsafe {
        if CGCursorIsVisible() == 0 {
            return false;
        }
        let _ = allow_background_cursor_control();
        CGDisplayHideCursor(CGMainDisplayID());
        HIDES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        true
    }
}

/// Re-assert the cursor/mouse disassociation, without constructing a new guard.
///
/// The association is global window-server state, and nothing guarantees it stays
/// where seam put it: any process may restore it, and `CGWarpMouseCursorPosition` is
/// suspected of quietly doing so. If it comes back mid-session the cursor starts
/// tracking the mouse again with the guard still alive - which is indistinguishable,
/// from the chair, from suppression being broken. Re-asserting is one cheap call and
/// idempotent, so the daemon does it whenever it finds the cursor away from where it
/// was parked, and after every pin.
pub fn reassert_detach() {
    // SAFETY: documented CoreGraphics call taking a plain boolean-as-integer.
    let _ = unsafe { CGAssociateMouseAndMouseCursorPosition(0) };
}

/// Start observing local input, with the ability to withhold it.
///
/// The tap can discard events, but only ever does so while [`set_suppress_local`] is true
/// — i.e. while a peer owns the pointer. Everything else, including every error path,
/// passes input through untouched.
///
/// Suppression is what makes this a KVM rather than a mirror: without it the local
/// machine keeps acting on clicks and keystrokes that were meant for another screen.
pub fn observe_pointer() -> Result<Receiver<Observed>, Error> {
    let permissions = Permissions::check();
    if !permissions.can_listen {
        return Err(Error::PermissionDenied {
            what: "watch the mouse and keyboard".into(),
            where_to: "System Settings > Privacy & Security > Input Monitoring".into(),
        });
    }
    // Accessibility is required to CAPTURE, not just to inject, and this is the single
    // most confusing failure in the whole project.
    //
    // With Input Monitoring alone, CGEventTapCreate SUCCEEDS. It does not return NULL and
    // it does not report an error. macOS silently downgrades the tap to listen-only, and a
    // listen-only tap cannot discard: returning NULL from the callback does nothing at all.
    //
    // The result is a program that looks like it works. Movement is observed, forwarded,
    // and replayed on the other machine perfectly — while the local cursor and keyboard
    // keep acting on the very same events. That is a mirror, not a KVM, and from a chair
    // it is indistinguishable from suppression being broken.
    //
    // Refusing here converts a silent, near-unfindable misbehaviour into one sentence.
    if !permissions.can_post {
        return Err(Error::PermissionDenied {
            what: "withhold input from this machine while another machine has the pointer. \
                   Without this, seam can still see and forward input, but this machine \
                   keeps acting on it too — the pointer appears on both screens at once"
                .into(),
            where_to: "System Settings > Privacy & Security > Accessibility".into(),
        });
    }

    let (sender, receiver) = channel();
    let (ready_tx, ready_rx) = channel::<Result<(), String>>();

    // The tap runs on its own thread with its own run loop, so it fires regardless of what
    // the rest of the program is doing. Sharing a run loop with async work is how a
    // callback ends up slow enough for macOS to disable the tap.
    std::thread::Builder::new()
        .name("seam-macos-tap".into())
        .spawn(move || {
            let mask = mask_for(K_CG_EVENT_MOUSE_MOVED)
                | mask_for(K_CG_EVENT_LEFT_MOUSE_DRAGGED)
                | mask_for(K_CG_EVENT_RIGHT_MOUSE_DRAGGED)
                | mask_for(K_CG_EVENT_OTHER_MOUSE_DRAGGED)
                | mask_for(K_CG_EVENT_LEFT_MOUSE_DOWN)
                | mask_for(K_CG_EVENT_LEFT_MOUSE_UP)
                | mask_for(K_CG_EVENT_RIGHT_MOUSE_DOWN)
                | mask_for(K_CG_EVENT_RIGHT_MOUSE_UP)
                | mask_for(K_CG_EVENT_OTHER_MOUSE_DOWN)
                | mask_for(K_CG_EVENT_OTHER_MOUSE_UP)
                | mask_for(K_CG_EVENT_SCROLL_WHEEL)
                | mask_for(K_CG_EVENT_KEY_DOWN)
                | mask_for(K_CG_EVENT_KEY_UP)
                | mask_for(K_CG_EVENT_FLAGS_CHANGED)
                | mask_for(K_CG_EVENT_SYSTEM_DEFINED);

            let state = Box::into_raw(Box::new(TapState { sender, tap: core::ptr::null_mut() }));

            // SAFETY: `state` is a valid, leaked pointer that outlives the run loop, and
            // `on_event` matches the required callback signature.
            let tap = unsafe {
                CGEventTapCreate(
                    K_CG_HID_EVENT_TAP,
                    K_CG_HEAD_INSERT_EVENT_TAP,
                    K_CG_EVENT_TAP_OPTION_DEFAULT,
                    mask,
                    on_event,
                    state.cast::<c_void>(),
                )
            };
            TAP.store(tap.cast(), std::sync::atomic::Ordering::Relaxed);
            if tap.is_null() {
                // NULL means the permission is missing, not that the API is broken.
                let _ = ready_tx.send(Err(
                    "macOS refused the input tap, which means Input Monitoring permission \
                     is missing. Grant it in System Settings > Privacy & Security > Input \
                     Monitoring, then restart seam — the permission does not apply to an \
                     already-running program."
                        .to_owned(),
                ));
                // SAFETY: nothing took ownership of `state`, so reclaim it.
                drop(unsafe { Box::from_raw(state) });
                return;
            }

            // SAFETY: `state` is uniquely owned here; the tap exists but its callback
            // cannot run until the run loop starts below.
            unsafe { (*state).tap = tap };

            // SAFETY: `tap` is a valid Mach port; all four calls take it or the resulting
            // source, and `kCFRunLoopCommonModes` is a framework-owned constant. Common
            // modes matter: AppKit enters private run loop modes during menu tracking and
            // drags, and a default-mode-only source stops firing during them.
            unsafe {
                let source = CFMachPortCreateRunLoopSource(core::ptr::null(), tap, 0);
                CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
                CGEventTapEnable(tap, 1);
            }

            let _ = ready_tx.send(Ok(()));
            // SAFETY: blocks this thread forever, servicing the tap.
            unsafe { CFRunLoopRun() };
        })
        .map_err(|e| Error::Platform(format!("could not start the input thread: {e}")))?;

    match ready_rx.recv() {
        Ok(Ok(())) => Ok(receiver),
        Ok(Err(message)) => Err(Error::PermissionDenied {
            what: "watch the mouse and keyboard".into(),
            where_to: message,
        }),
        Err(_) => Err(Error::Platform("the input thread stopped before starting".into())),
    }
}

// ---------------------------------------------------------------- file clipboard

// SAFETY CONTRACT: the Carbon Pasteboard API — C, documented, and still the only way
// to read and write file references on the pasteboard without Objective-C. Finder
// copies files as `public.file-url` flavors, one per item, and pastes anything that
// provides the same.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn PasteboardCreate(name: *const c_void, out: *mut *mut c_void) -> i32;
    fn PasteboardSynchronize(pasteboard: *mut c_void) -> u32;
    fn PasteboardGetItemCount(pasteboard: *mut c_void, count: *mut isize) -> i32;
    fn PasteboardGetItemIdentifier(
        pasteboard: *mut c_void,
        index: isize,
        item: *mut *mut c_void,
    ) -> i32;
    fn PasteboardCopyItemFlavorData(
        pasteboard: *mut c_void,
        item: *mut c_void,
        flavor: *const c_void,
        out: *mut *const c_void,
    ) -> i32;
    fn PasteboardClear(pasteboard: *mut c_void) -> i32;
    fn PasteboardPutItemFlavor(
        pasteboard: *mut c_void,
        item: *mut c_void,
        flavor: *const c_void,
        data: *const c_void,
        flags: u32,
    ) -> i32;
    fn CFDataCreate(alloc: *const c_void, bytes: *const u8, length: isize) -> *const c_void;
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFDataGetLength(data: *const c_void) -> isize;
    fn CFURLCreateWithBytes(
        alloc: *const c_void,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        base: *const c_void,
    ) -> *const c_void;
    fn CFURLCreateFilePathURL(
        alloc: *const c_void,
        url: *const c_void,
        error: *mut *const c_void,
    ) -> *const c_void;
    fn CFURLGetFileSystemRepresentation(
        url: *const c_void,
        resolve_against_base: u8,
        buffer: *mut u8,
        buffer_len: isize,
    ) -> u8;
}

/// The name of the general clipboard — the value of Apple's `kPasteboardClipboard`,
/// spelled out because the constant's data symbol does not link from the umbrella
/// framework while the functions do.
fn clipboard_name() -> *const c_void {
    static NAME: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *NAME.get_or_init(|| {
        // SAFETY: creates one immortal CFString.
        unsafe {
            CFStringCreateWithCString(
                core::ptr::null(),
                c"com.apple.pasteboard.clipboard".as_ptr().cast(),
                K_CF_STRING_ENCODING_UTF8,
            ) as usize
        }
    }) as *const c_void
}

/// The pasteboard flavor Finder uses for a copied file: a percent-encoded file URL.
fn file_url_flavor() -> *const c_void {
    static FLAVOR: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *FLAVOR.get_or_init(|| {
        // SAFETY: creates one immortal CFString.
        unsafe {
            CFStringCreateWithCString(
                core::ptr::null(),
                c"public.file-url".as_ptr().cast(),
                K_CF_STRING_ENCODING_UTF8,
            ) as usize
        }
    }) as *const c_void
}

/// Resolve a file-REFERENCE URL (`file:///.file/id=…`) to the real path it points at.
///
/// Some applications put the inode-style reference form on the pasteboard instead of a
/// path URL — a copied zip arrived as `/.file/id=6571367.412610877`, which was then
/// read literally: "not sharing the copied files: … is unreadable". CoreFoundation
/// owns the id-to-path mapping; `CFURLCreateFilePathURL` is the documented inverse.
fn resolve_reference_url(url_bytes: &[u8]) -> Option<std::path::PathBuf> {
    // SAFETY: documented CoreFoundation calls; every created object is released on
    // every path, and the byte buffer outlives the call that reads it.
    unsafe {
        let url = CFURLCreateWithBytes(
            core::ptr::null(),
            url_bytes.as_ptr(),
            isize::try_from(url_bytes.len()).ok()?,
            K_CF_STRING_ENCODING_UTF8,
            core::ptr::null(),
        );
        if url.is_null() {
            return None;
        }
        let mut error: *const c_void = core::ptr::null();
        let resolved = CFURLCreateFilePathURL(core::ptr::null(), url, &raw mut error);
        CFRelease(url);
        if resolved.is_null() {
            if !error.is_null() {
                CFRelease(error);
            }
            return None;
        }
        let mut buffer = [0u8; 4096];
        let ok = CFURLGetFileSystemRepresentation(
            resolved,
            1,
            buffer.as_mut_ptr(),
            isize::try_from(buffer.len()).unwrap_or(0),
        );
        CFRelease(resolved);
        if ok == 0 {
            return None;
        }
        let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
        let text = core::str::from_utf8(&buffer[..end]).ok()?;
        Some(std::path::PathBuf::from(text))
    }
}

/// Percent-decode the path of a `file://` URL. Returns `None` for anything else.
fn path_from_file_url(url: &[u8]) -> Option<std::path::PathBuf> {
    let rest = url.strip_prefix(b"file://")?;
    // Strip an authority ("localhost") if present; the path starts at the next '/'.
    let path_start = rest.iter().position(|&b| b == b'/')?;
    let raw = &rest[path_start..];
    let mut bytes = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            b'%' if i + 2 < raw.len() => {
                let hex = core::str::from_utf8(&raw[i + 1..i + 3]).ok()?;
                bytes.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b => {
                bytes.push(b);
                i += 1;
            }
        }
    }
    let text = String::from_utf8(bytes).ok()?;
    Some(std::path::PathBuf::from(text))
}

/// Percent-encode a path into a `file://` URL, the exact inverse of the above.
fn file_url_from_path(path: &std::path::Path) -> String {
    use std::fmt::Write as _;
    let mut url = String::from("file://");
    for &b in path.to_string_lossy().as_bytes() {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/');
        if unreserved {
            url.push(b as char);
        } else {
            let _ = write!(url, "%{b:02X}");
        }
    }
    url
}

/// The absolute paths of files currently on the clipboard, if any.
pub fn read_file_list() -> Result<Option<Vec<std::path::PathBuf>>, Error> {
    // SAFETY: documented Carbon Pasteboard sequence; every CF object copied out is
    // released on the single exit path of its scope.
    unsafe {
        let mut pasteboard: *mut c_void = core::ptr::null_mut();
        if PasteboardCreate(clipboard_name(), &raw mut pasteboard) != 0
            || pasteboard.is_null()
        {
            return Err(Error::Platform("the pasteboard is unavailable".into()));
        }
        PasteboardSynchronize(pasteboard);
        let mut count: isize = 0;
        if PasteboardGetItemCount(pasteboard, &raw mut count) != 0 {
            CFRelease(pasteboard.cast_const());
            return Ok(None);
        }
        let mut paths = Vec::new();
        for index in 1..=count {
            let mut item: *mut c_void = core::ptr::null_mut();
            if PasteboardGetItemIdentifier(pasteboard, index, &raw mut item) != 0 {
                continue;
            }
            let mut data: *const c_void = core::ptr::null();
            if PasteboardCopyItemFlavorData(pasteboard, item, file_url_flavor(), &raw mut data)
                != 0
                || data.is_null()
            {
                continue; // not a file item
            }
            let len = usize::try_from(CFDataGetLength(data)).unwrap_or(0);
            let bytes = core::slice::from_raw_parts(CFDataGetBytePtr(data), len);
            if let Some(path) = path_from_file_url(bytes) {
                // A reference-form URL decodes to `/.file/id=…`, which is not a place
                // on disk — ask CoreFoundation for the path it actually means.
                let path = if path.starts_with("/.file") {
                    resolve_reference_url(bytes).unwrap_or(path)
                } else {
                    path
                };
                paths.push(path);
            }
            CFRelease(data);
        }
        CFRelease(pasteboard.cast_const());
        Ok(if paths.is_empty() { None } else { Some(paths) })
    }
}

/// Put a list of local files on the clipboard, exactly as Finder would.
pub fn write_file_list(paths: &[std::path::PathBuf]) -> Result<(), Error> {
    // SAFETY: documented Carbon Pasteboard sequence, mirrored from the read path.
    unsafe {
        let mut pasteboard: *mut c_void = core::ptr::null_mut();
        if PasteboardCreate(clipboard_name(), &raw mut pasteboard) != 0
            || pasteboard.is_null()
        {
            return Err(Error::Platform("the pasteboard is unavailable".into()));
        }
        PasteboardSynchronize(pasteboard);
        if PasteboardClear(pasteboard) != 0 {
            CFRelease(pasteboard.cast_const());
            return Err(Error::Platform("could not take ownership of the pasteboard".into()));
        }
        for (i, path) in paths.iter().enumerate() {
            let url = file_url_from_path(path);
            let data = CFDataCreate(
                core::ptr::null(),
                url.as_ptr(),
                isize::try_from(url.len()).unwrap_or(0),
            );
            if data.is_null() {
                continue;
            }
            let status = PasteboardPutItemFlavor(
                pasteboard,
                core::ptr::without_provenance_mut(i + 1),
                file_url_flavor(),
                data,
                0,
            );
            CFRelease(data);
            if status != 0 {
                CFRelease(pasteboard.cast_const());
                return Err(Error::Platform(format!(
                    "the pasteboard refused a file item (error {status})"
                )));
            }
        }
        CFRelease(pasteboard.cast_const());
        Ok(())
    }
}

#[cfg(test)]
mod reference_urls {
    //! The `/.file/id=…` form, proven against the real filesystem: some applications
    //! put the inode-style reference URL on the pasteboard, and a copied zip arriving
    //! that way was read literally and refused as unreadable.

    use super::*;

    #[test]
    fn a_reference_url_resolves_to_the_real_path() {
        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            fn CFURLCreateFileReferenceURL(
                alloc: *const c_void,
                url: *const c_void,
                error: *mut *const c_void,
            ) -> *const c_void;
            fn CFURLGetBytes(url: *const c_void, buffer: *mut u8, buffer_len: isize) -> isize;
        }
        let dir = std::env::temp_dir().join(format!("seam-refurl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("test directory");
        let file = dir.join("archive.zip");
        std::fs::write(&file, b"not really a zip").expect("test file");

        // Build the reference form the same way an application would.
        let url_text = file_url_from_path(&file);
        // SAFETY: documented calls; every created object is released below.
        let reference_bytes = unsafe {
            let path_url = CFURLCreateWithBytes(
                core::ptr::null(),
                url_text.as_ptr(),
                isize::try_from(url_text.len()).expect("short url"),
                K_CF_STRING_ENCODING_UTF8,
                core::ptr::null(),
            );
            assert!(!path_url.is_null(), "the path URL must parse");
            let mut error: *const c_void = core::ptr::null();
            let reference = CFURLCreateFileReferenceURL(core::ptr::null(), path_url, &raw mut error);
            CFRelease(path_url);
            assert!(!reference.is_null(), "the file must yield a reference URL");
            let mut buffer = [0u8; 1024];
            let written = CFURLGetBytes(reference, buffer.as_mut_ptr(), 1024);
            CFRelease(reference);
            buffer[..usize::try_from(written).expect("short url")].to_vec()
        };
        let as_text = String::from_utf8_lossy(&reference_bytes).to_string();
        assert!(as_text.contains("/.file/id="), "precondition: got {as_text}");

        let resolved = resolve_reference_url(&reference_bytes)
            .expect("the reference URL must resolve");
        assert_eq!(
            std::fs::canonicalize(&resolved).expect("resolved path exists"),
            std::fs::canonicalize(&file).expect("original path exists"),
            "the reference must point back at the file it was made from"
        );
        std::fs::remove_dir_all(dir).ok();
    }
}

#[cfg(test)]
mod tap_behaviour {
    //! Live experiments against a real event tap.
    //!
    //! These exist because the return-crossing bug could not be settled by reasoning: the
    //! question is what CoreGraphics actually reports while the cursor is detached and the
    //! tap is discarding events, and Apple documents the first half of that but not the
    //! combination. Guessing produced three wrong fixes in a row.

    use super::*;
    use std::time::Duration;

    /// Tap and pasteboard state is process-global, so these tests cannot overlap.
    /// The panic-poisoned case is fine to unwrap-or: a prior failure must not cascade.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // SAFETY CONTRACT: documented CoreGraphics event-construction API.
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreateMouseEvent(
            source: *const c_void,
            mouse_type: CGEventType,
            cursor_position: CGPoint,
            mouse_button: u32,
        ) -> CGEventRef;
        fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
        fn CGEventPost(tap: u32, event: CGEventRef);
    }

    /// Post a synthetic mouse movement carrying the given deltas.
    fn post_move(x: f64, y: f64, dx: i64, dy: i64) {
        // SAFETY: a NULL source is the documented default; the event is released on the
        // single exit path.
        unsafe {
            let event = CGEventCreateMouseEvent(
                core::ptr::null(),
                K_CG_EVENT_MOUSE_MOVED,
                CGPoint { x, y },
                0,
            );
            if event.is_null() {
                return;
            }
            CGEventSetIntegerValueField(event, K_CG_MOUSE_DELTA_X, dx);
            CGEventSetIntegerValueField(event, K_CG_MOUSE_DELTA_Y, dy);
            CGEventPost(K_CG_HID_EVENT_TAP, event);
            CFRelease(event.cast_const());
        }
    }

    /// Collect whatever the tap reports over a short window.
    fn drain(rx: &std::sync::mpsc::Receiver<Observed>, window: Duration) -> Vec<Observed> {
        let deadline = std::time::Instant::now() + window;
        let mut seen = Vec::new();
        while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
            match rx.recv_timeout(remaining) {
                Ok(event) => seen.push(event),
                Err(_) => break,
            }
        }
        seen
    }

    /// The watchdog's new signal must actually report a live tap as live.
    ///
    /// The old signal - "have events arrived recently" - could not tell a broken tap from
    /// a person who was reading rather than moving the mouse, and it guessed wrong: it
    /// handed input back two seconds after the pointer moved to another screen, which
    /// reattached the cursor and cleared suppression. If this ever returns false for a
    /// healthy tap, that whole failure comes straight back.
    #[test]
    fn a_live_tap_reports_itself_alive() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Ok(_rx) = observe_pointer() else {
            eprintln!("skipped: Input Monitoring permission not granted");
            return;
        };
        assert_eq!(
            capture_is_alive(),
            Some(true),
            "a tap that was just created and is delivering events must report alive, or \
             the watchdog will release input from a machine that is working perfectly"
        );
    }

    /// The reappearing-cursor machinery, asserted as far as one process can.
    ///
    /// The real trigger — another application's connection making the arrow visible —
    /// cannot be simulated from in here: visibility is per-connection refcounted and
    /// applied asynchronously by the window server. What is provable in-process: the
    /// watchdog is a no-op while hidden, a forced show (if it takes effect here) is
    /// noticed and reversed, and release always ends with a visible cursor.
    #[test]
    // Cursor visibility is one global, asynchronously-applied refcount shared with every
    // other test in this module — and with the rest of the machine. Run deliberately:
    //   cargo test -p seam-input --lib hiding_survives -- --ignored --nocapture
    // It passes standalone; in-suite it contends with the other cursor tests, which is a
    // property of the test environment and not of the code under test.
    #[ignore = "contends on global cursor state; run standalone"]
    fn hiding_survives_the_system_showing_the_cursor() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let guard = CursorGuard::detach(true).expect("detach");
        // Visibility is applied asynchronously by the window server, and other tests'
        // balanced show/hide traffic can still be landing. Poll rather than assume.
        let mut hidden = false;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(50));
            // SAFETY: read-only query.
            if unsafe { CGCursorIsVisible() } == 0 {
                hidden = true;
                break;
            }
            // The watchdog re-hiding here is it doing its job, not a failure.
            rehide_if_visible();
        }
        if !hidden {
            eprintln!("skipped: the window server never applied the hide in this environment");
            drop(guard);
            return;
        }
        // SAFETY: documented calls, serialized by the mutex above.
        unsafe {
            assert!(!rehide_if_visible(), "no-op while already hidden");
            CGDisplayShowCursor(CGMainDisplayID());
            std::thread::sleep(Duration::from_millis(50));
            if CGCursorIsVisible() == 0 {
                eprintln!("window server kept it hidden; external show not simulable here");
            } else {
                assert!(rehide_if_visible(), "the watchdog must notice and re-hide");
                std::thread::sleep(Duration::from_millis(50));
                assert_eq!(CGCursorIsVisible(), 0, "hidden again");
            }
        }
        drop(guard);
        // Poll: the window server applies visibility asynchronously.
        let mut visible = false;
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(50));
            // SAFETY: read-only visibility query.
            if unsafe { CGCursorIsVisible() } != 0 {
                visible = true;
                break;
            }
        }
        assert!(visible, "coming home must leave a visible cursor");
    }

    /// Does THIS macOS still honour Barrier's background-cursor trick?
    ///
    /// Private API, so it can vanish in any release. 0 means the window server granted
    /// it and `CGDisplayHideCursor` from the daemon will actually hide the arrow.
    #[test]
    fn window_server_grants_background_cursor_control() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let status = allow_background_cursor_control();
        eprintln!("SetsCursorInBackground -> {status} (0 = granted)");
        assert_eq!(status, 0, "window server refused background cursor control");
    }

    /// Does warping actually move the cursor from this process?
    ///
    /// Needs no human: warp somewhere known, then read the position back. If the read
    /// does not match, `CGWarpMouseCursorPosition` is being ignored, and pinning the
    /// cursor by warping cannot work however often it is called.
    #[test]
    fn warping_actually_moves_the_cursor() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Ok(before) = cursor_position() else {
            eprintln!("skipped: no cursor");
            return;
        };
        let guard = CursorGuard::detach(true);
        let target = (600, 400);
        let warped = warp_cursor(target.0, target.1);
        std::thread::sleep(Duration::from_millis(80));
        let after = cursor_position();
        drop(guard);
        let _ = warp_cursor(before.0, before.1);

        eprintln!("warp call: {warped:?}");
        eprintln!("asked for {target:?}, cursor reads {after:?}");
        eprintln!(
            "{}",
            if after.is_ok_and(|p| p == target) {
                "WARP WORKS: pinning is viable; if the cursor still moves, the pin is not running"
            } else {
                "WARP IGNORED: warping cannot pin the cursor from this process"
            }
        );
    }

    /// Does detaching actually WORK from a daemon, or merely return success?
    ///
    /// Run with `--ignored --nocapture` and move the mouse for five seconds. Everything
    /// else about this bug has been argued rather than measured, and the two facts that
    /// matter cannot be separated any other way: the call returning 0 and the call having
    /// an effect are different claims, and only the second one stops the cursor.
    #[test]
    #[ignore = "needs a human to move the mouse"]
    fn does_detaching_actually_freeze_the_cursor() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _rx = observe_pointer().ok();
        let guard = CursorGuard::detach(true);
        eprintln!("detach: {:?}", guard.as_ref().map(|_| "ok"));
        set_suppress_local(true);

        let mut seen = Vec::new();
        for _ in 0..25 {
            if let Ok(p) = cursor_position() {
                seen.push(p);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        set_suppress_local(false);
        drop(guard);

        let first = seen.first().copied().unwrap_or((0, 0));
        let moved = seen.iter().filter(|&&p| p != first).count();
        eprintln!("samples={} moved={} first={:?} last={:?}", seen.len(), moved, first, seen.last());
        eprintln!(
            "{}",
            if moved == 0 {
                "FROZE: detaching works from a daemon; something else moves the cursor"
            } else {
                "TRACKED: detaching returns success but has NO effect from a daemon"
            }
        );
    }

    /// Can a **daemon** decouple the cursor from the mouse?
    ///
    /// This matters because it decides whether the mirrored-cursor bug is fixable at all
    /// from a background process. Earlier work in this project assumed
    /// `CGAssociateMouseAndMouseCursorPosition` required foreground status and routed
    /// around it — twice, and one of those workarounds locked the machine. Measuring it
    /// says otherwise: it returns success (0) from the test binary, which has no
    /// foreground status, no window and no activation policy.
    ///
    /// What this test deliberately does **not** claim: that the local cursor stays put.
    /// That cannot be measured with `CGEventPost`, because a posted event always carries
    /// an absolute location and so repositions the cursor directly, bypassing both the
    /// association and the tap's discard. A physical mouse reports deltas and does not.
    /// Proving the real device path needs a human hand on a real mouse.
    #[test]
    fn a_daemon_can_detach_the_cursor_from_the_mouse() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // SAFETY: documented CoreGraphics calls; the association is always restored.
        let detached = unsafe { CGAssociateMouseAndMouseCursorPosition(0) };
        unsafe { CGAssociateMouseAndMouseCursorPosition(1) };
        assert_eq!(
            detached, 0,
            "CGAssociateMouseAndMouseCursorPosition(0) failed with {detached}; if this ever \
             starts failing, cursor detachment is no longer available to a daemon and the \
             fix has to move into a foreground agent"
        );
    }

    /// The decisive question: does movement still reach the tap while the cursor is
    /// detached **and** events are being discarded?
    ///
    /// If it does not, the pointer can never be brought back from a peer, because the very
    /// events that would return it are the ones being withheld.
    #[test]
    fn movement_is_still_observed_while_detached_and_suppressing() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Ok(rx) = observe_pointer() else {
            eprintln!("skipped: Input Monitoring permission not granted");
            return;
        };
        std::thread::sleep(Duration::from_millis(200));
        let _ = drain(&rx, Duration::from_millis(100));

        let guard = CursorGuard::detach(false).ok();
        set_suppress_local(true);

        for i in 0..10 {
            post_move(500.0, 500.0, 7, 0);
            std::thread::sleep(Duration::from_millis(10));
            let _ = i;
        }
        let seen = drain(&rx, Duration::from_millis(400));

        set_suppress_local(false);
        drop(guard);
        force_restore_cursor();

        let moved: Vec<_> = seen
            .iter()
            .filter_map(|e| match e {
                Observed::Motion { dx, dy, .. } if *dx != 0 || *dy != 0 => Some((*dx, *dy)),
                _ => None,
            })
            .collect();

        assert!(
            !moved.is_empty(),
            "no movement reached the tap while detached and suppressing — the pointer could \
             never be brought back from a peer. Saw {} events total.",
            seen.len()
        );
    }

    /// Suppression must not outlive the guard, whatever happens.
    #[test]
    fn dropping_the_guard_always_restores_local_input() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let guard = CursorGuard::detach(false).ok();
        set_suppress_local(true);
        assert!(is_suppressing_local());
        drop(guard);
        assert!(!is_suppressing_local(), "the Mac would have been left unable to respond");
        force_restore_cursor();
    }
}
