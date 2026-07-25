//! Windows platform backend.
//!
//! # The two traps
//!
//! **Absolute coordinates map to the primary monitor unless you say otherwise.** Microsoft
//! documents `MOUSEEVENTF_ABSOLUTE` as normalising 0–65535 across the *primary* display;
//! only adding `MOUSEEVENTF_VIRTUALDESK` spreads it across the whole virtual desktop.
//! Omitting it means every secondary monitor is unreachable, which is one of the oldest
//! bugs in this software category.
//!
//! **`SendInput` blocked by UIPI fails silently.** Microsoft's own documentation is
//! explicit: *"neither GetLastError nor the return value will indicate the failure was
//! caused by UIPI blocking."* So when the user focuses an elevated window, injection
//! stops with no error at all. seam therefore verifies by reading the cursor back, so it
//! can say "input is being blocked by an elevated window" instead of appearing to work.

#![allow(unsafe_code)]

use windows_sys::Win32::Foundation::{LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK,
    MOUSEINPUT, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, MONITORINFOF_PRIMARY, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SetCursorPos,
};
use windows_sys::core::BOOL;

use crate::Error;
use crate::screen::{Desktop, Display, PixelRect};

/// Enumerate the machine's displays.
pub fn desktop() -> Result<Desktop, Error> {
    let mut displays: Vec<Display> = Vec::new();
    // SAFETY: `EnumDisplayMonitors` calls our callback once per monitor with a valid
    // handle. The LPARAM is our `Vec`, which outlives the call because this function
    // blocks until enumeration finishes.
    let ok = unsafe {
        EnumDisplayMonitors(
            core::ptr::null_mut(),
            core::ptr::null(),
            Some(collect_monitor),
            core::ptr::from_mut(&mut displays) as LPARAM,
        )
    };
    if ok == 0 && displays.is_empty() {
        return Err(Error::Platform("EnumDisplayMonitors returned no displays".into()));
    }
    Ok(Desktop::new(displays))
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _dc: HDC,
    _clip: *mut RECT,
    userdata: LPARAM,
) -> BOOL {
    // SAFETY: `userdata` is the `&mut Vec<Display>` passed by `desktop`, still alive for
    // the duration of the enumeration.
    let displays = unsafe { &mut *(userdata as *mut Vec<Display>) };

    let mut info: MONITORINFOEXW = unsafe { core::mem::zeroed() };
    info.monitorInfo.cbSize = u32::try_from(size_of::<MONITORINFOEXW>()).unwrap_or(0);

    // SAFETY: `monitor` is a valid handle from the enumeration, and `info` is a correctly
    // sized MONITORINFOEXW as required by the `cbSize` contract.
    let got =
        unsafe { GetMonitorInfoW(monitor, core::ptr::from_mut(&mut info).cast::<MONITORINFO>()) };
    if got == 0 {
        // Skip a monitor we cannot read rather than aborting the whole enumeration: one
        // unreadable display should not cost the user every other display.
        return 1;
    }

    let r = info.monitorInfo.rcMonitor;
    displays.push(Display {
        // The HMONITOR value is stable for the lifetime of the monitor's configuration,
        // which is exactly the scope over which we need to recognise it.
        id: (monitor as usize as u64 & 0xFFFF_FFFF) as u32,
        pixels: PixelRect::new(r.left, r.top, r.right - r.left, r.bottom - r.top),
        // Windows exposes physical size only through EDID in the registry, which is not
        // worth a registry walk here: `Desktop::new` substitutes a nominal 96 DPI size,
        // and physical layout is only advisory for the edge graph.
        width_mm: 0,
        height_mm: 0,
        scale: 256,
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    });
    1
}

/// The virtual desktop rectangle: every monitor, including those left of or above the
/// primary, which have **negative** coordinates.
fn virtual_screen() -> PixelRect {
    // SAFETY: `GetSystemMetrics` takes a constant index and cannot fail.
    unsafe {
        PixelRect::new(
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// Where the pointer is, in virtual-desktop pixel coordinates.
pub fn cursor_position() -> Result<(i32, i32), Error> {
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: `point` is a valid, initialised out-parameter.
    if unsafe { GetCursorPos(&raw mut point) } == 0 {
        return Err(Error::Platform("GetCursorPos failed".into()));
    }
    Ok((point.x, point.y))
}

/// Move the pointer without generating input events.
pub fn warp_cursor(x: i32, y: i32) -> Result<(), Error> {
    // SAFETY: plain integer arguments; the OS clamps out-of-range values.
    if unsafe { SetCursorPos(x, y) } == 0 {
        return Err(Error::Platform("SetCursorPos failed".into()));
    }
    Ok(())
}

/// Move the pointer by injecting a real input event.
///
/// Unlike [`warp_cursor`], this goes through the input stack, so applications see genuine
/// mouse movement rather than a teleport — which is what a remote pointer must look like.
pub fn inject_motion(x: i32, y: i32) -> Result<(), Error> {
    let vs = virtual_screen();
    if vs.is_empty() {
        return Err(Error::Platform("the virtual desktop has no size".into()));
    }

    // Normalise across the *virtual* desktop, not the primary monitor. The `- 1` matters:
    // without it the last row and column are unreachable, a classic off-by-one here.
    let nx = i64::from(x - vs.x) * 65535 / i64::from((vs.width - 1).max(1));
    let ny = i64::from(y - vs.y) * 65535 / i64::from((vs.height - 1).max(1));

    let mut input = INPUT {
        r#type: INPUT_MOUSE,
        // SAFETY: INPUT is a plain C union of POD; zeroing is a valid initial state.
        Anonymous: unsafe { core::mem::zeroed() },
    };
    input.Anonymous.mi = MOUSEINPUT {
        dx: i32::try_from(nx.clamp(0, 65535)).unwrap_or(0),
        dy: i32::try_from(ny.clamp(0, 65535)).unwrap_or(0),
        mouseData: 0,
        // VIRTUALDESK is mandatory: without it these coordinates address the primary
        // monitor only, and every other display becomes unreachable.
        dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        time: 0,
        dwExtraInfo: 0,
    };

    // SAFETY: one correctly initialised INPUT, with the size the API requires.
    let sent =
        unsafe { SendInput(1, &raw const input, i32::try_from(size_of::<INPUT>()).unwrap_or(0)) };
    if sent != 1 {
        return Err(Error::Platform("SendInput accepted no events".into()));
    }
    Ok(())
}

/// Inject motion and verify it actually happened.
///
/// `SendInput` reports success even when UIPI silently discarded the event, so the only
/// way to know is to read the cursor back. Being able to say *"an elevated window is
/// blocking input"* is the difference between a diagnosable problem and the "it randomly
/// stops working" reports that define this software category.
pub fn inject_motion_verified(x: i32, y: i32) -> Result<(), Error> {
    inject_motion(x, y)?;
    let (ax, ay) = cursor_position()?;
    // A few pixels of tolerance: the OS clamps to the display edge and rounds the
    // normalised coordinate back, so an exact match is not expected.
    if (ax - x).abs() <= 2 && (ay - y).abs() <= 2 {
        return Ok(());
    }
    Err(Error::PermissionDenied {
        what: "move the pointer".into(),
        where_to: "a window running as administrator currently has focus. Windows blocks \
                   input from a lower-privilege program (UIPI) and reports no error. Run \
                   seam as administrator, or click a non-elevated window"
            .into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_machine_reports_at_least_one_display() {
        let desktop = desktop().expect("EnumDisplayMonitors should succeed");
        assert!(!desktop.displays.is_empty());
        assert!(desktop.primary().is_some());
    }

    #[test]
    fn the_virtual_screen_covers_every_display() {
        let vs = virtual_screen();
        assert!(!vs.is_empty());
        for d in desktop().unwrap().displays {
            assert!(d.pixels.x >= vs.x, "display starts left of the virtual screen");
            assert!(d.pixels.right() <= vs.right(), "display extends past the virtual screen");
        }
    }

    #[test]
    fn the_cursor_is_somewhere_on_the_desktop() {
        let (x, y) = cursor_position().expect("GetCursorPos should succeed");
        assert!(desktop().unwrap().contains(x, y), "cursor at ({x}, {y}) is on no display");
    }
}
