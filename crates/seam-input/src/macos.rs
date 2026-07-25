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
        // Releasing the cursor and withholding input must end together: leaving
        // suppression on with the cursor reattached would be a Mac that ignores its own
        // keyboard.
        set_suppress_local(false);
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

/// `kCGSessionEventTap` — the login session, which an ordinary user process may tap.
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

/// `kCGScrollWheelEventDeltaAxis1` — vertical, in wheel notches.
const K_CG_SCROLL_DELTA_AXIS1: u32 = 11;
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
    /// A key changed state, carrying the text the *local* layout produced.
    ///
    /// The text is what makes mismatched layouts work: a German Apple keyboard types `@`
    /// as `Option+L`, and sending the resulting glyph is the only way the receiver can
    /// reproduce it without knowing anything about the sender's layout.
    Key { text: LogicalText, down: bool },
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

/// Turn a CoreGraphics event into something the protocol can carry.
///
/// # Safety
/// `event` must be a valid `CGEventRef` for the duration of the call.
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
            // Prefer raw movement; fall back to cursor delta if the field is absent.
            let (dx, dy) = if raw_x != 0 || raw_y != 0 { (raw_x, raw_y) } else { (dx, dy) };
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
                // A Magic Mouse emits a stream of zero-delta events at the end of an
                // inertial scroll; forwarding them is pure noise.
                return None;
            }
            Some(Observed::Scroll {
                dx: i32::try_from(dx).unwrap_or(0),
                dy: i32::try_from(dy).unwrap_or(0),
            })
        }

        K_CG_EVENT_KEY_DOWN | K_CG_EVENT_KEY_UP => {
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
            if logical.is_empty() {
                return None;
            }
            Some(Observed::Key { text: logical, down: event_type == K_CG_EVENT_KEY_DOWN })
        }

        _ => None,
    }
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
    let Some(observed) = observed else { return event };

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
                | mask_for(K_CG_EVENT_KEY_UP);

            let state = Box::into_raw(Box::new(TapState { sender, tap: core::ptr::null_mut() }));

            // SAFETY: `state` is a valid, leaked pointer that outlives the run loop, and
            // `on_event` matches the required callback signature.
            let tap = unsafe {
                CGEventTapCreate(
                    K_CG_SESSION_EVENT_TAP,
                    K_CG_HEAD_INSERT_EVENT_TAP,
                    K_CG_EVENT_TAP_OPTION_DEFAULT,
                    mask,
                    on_event,
                    state.cast::<c_void>(),
                )
            };
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
