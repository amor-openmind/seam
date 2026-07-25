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
    INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
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

/// One notch of wheel movement, as Windows defines it.
const WHEEL_DELTA: i32 = 120;

fn send_one(input: INPUT) -> Result<(), Error> {
    // SAFETY: one correctly initialised INPUT, with the size the API requires.
    let sent =
        unsafe { SendInput(1, &raw const input, i32::try_from(size_of::<INPUT>()).unwrap_or(0)) };
    if sent == 1 { Ok(()) } else { Err(Error::Platform("SendInput accepted no events".into())) }
}

fn mouse_input(flags: u32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        // SAFETY: INPUT is a union of POD; zeroing is a valid initial state.
        Anonymous: unsafe { core::mem::zeroed() },
    }
    .tap_mouse(flags, data)
}

trait TapMouse {
    fn tap_mouse(self, flags: u32, data: i32) -> Self;
}

impl TapMouse for INPUT {
    fn tap_mouse(mut self, flags: u32, data: i32) -> Self {
        self.Anonymous.mi = MOUSEINPUT {
            dx: 0,
            dy: 0,
            // `mouseData` is signed for wheels but the field is unsigned; the cast is the
            // documented way to pass a negative scroll amount.
            mouseData: data.cast_unsigned(),
            dwFlags: flags,
            time: 0,
            dwExtraInfo: 0,
        };
        self
    }
}

/// Press or release a mouse button. `button` follows the protocol's evdev numbering.
pub fn inject_button(button: u8, down: bool) -> Result<(), Error> {
    let flags = match (button, down) {
        (1, true) => MOUSEEVENTF_LEFTDOWN,
        (1, false) => MOUSEEVENTF_LEFTUP,
        (2, true) => MOUSEEVENTF_RIGHTDOWN,
        (2, false) => MOUSEEVENTF_RIGHTUP,
        (3, true) => MOUSEEVENTF_MIDDLEDOWN,
        (3, false) => MOUSEEVENTF_MIDDLEUP,
        _ => return Err(Error::Unsupported("that mouse button")),
    };
    send_one(mouse_input(flags, 0))
}

/// Scroll. Positive `dy` is away from the user, matching the protocol.
pub fn inject_scroll(dx: i32, dy: i32) -> Result<(), Error> {
    if dy != 0 {
        send_one(mouse_input(MOUSEEVENTF_WHEEL, dy.saturating_mul(WHEEL_DELTA)))?;
    }
    if dx != 0 {
        send_one(mouse_input(MOUSEEVENTF_HWHEEL, dx.saturating_mul(WHEEL_DELTA)))?;
    }
    Ok(())
}

/// Type text, by Unicode code unit rather than by key.
///
/// `KEYEVENTF_UNICODE` bypasses the keyboard layout entirely, which is exactly what
/// mismatched layouts need: the sender already resolved `Option+L` to `@` using *its*
/// layout, and this reproduces that character without the receiver having to agree about
/// which physical key it lives on. Non-BMP characters arrive as surrogate pairs and are
/// sent as two events, which is what Windows expects.
pub fn inject_text(text: &str, down: bool) -> Result<(), Error> {
    for unit in text.encode_utf16() {
        let mut input = INPUT {
            r#type: INPUT_KEYBOARD,
            // SAFETY: INPUT is a union of POD; zeroing is a valid initial state.
            Anonymous: unsafe { core::mem::zeroed() },
        };
        input.Anonymous.ki = KEYBDINPUT {
            wVk: 0,
            wScan: unit,
            dwFlags: KEYEVENTF_UNICODE | if down { 0 } else { KEYEVENTF_KEYUP },
            time: 0,
            dwExtraInfo: 0,
        };
        send_one(input)?;
    }
    Ok(())
}

/// USB HID usage to Windows virtual-key code.
///
/// Only for keys that produce no text. Anything that types a character is injected as
/// that character with `KEYEVENTF_UNICODE`, which is layout-independent; these keys have
/// no character, so they must be named by which key they are.
const fn virtual_key_for(usage: u16) -> u16 {
    // Consumer-page usages (volume and media keys) carry the page in the top bit — see
    // `PhysicalKey::CONSUMER_FLAG`. They map to their own family of virtual keys.
    if usage & 0x8000 != 0 {
        return match usage & 0x7FFF {
            0xE9 => 0xAF, // VK_VOLUME_UP
            0xEA => 0xAE, // VK_VOLUME_DOWN
            0xE2 => 0xAD, // VK_VOLUME_MUTE
            0xCD => 0xB3, // VK_MEDIA_PLAY_PAUSE
            0xB5 => 0xB0, // VK_MEDIA_NEXT_TRACK
            0xB6 => 0xB1, // VK_MEDIA_PREV_TRACK
            // Brightness has no Windows virtual key; dropped is better than guessed.
            _ => 0,
        };
    }
    match usage {
        0x2A => 0x08, // Backspace
        0x28 => 0x0D, // Return
        0x2B => 0x09, // Tab
        0x29 => 0x1B, // Escape
        0x4C => 0x2E, // Delete
        0x50 => 0x25, // Left
        0x4F => 0x27, // Right
        0x51 => 0x28, // Down
        0x52 => 0x26, // Up
        0x4A => 0x24, // Home
        0x4D => 0x23, // End
        0x4B => 0x21, // Page Up
        0x4E => 0x22, // Page Down
        // F1..F12 are contiguous in both encodings, so the offset maps them all.
        0x3A..=0x45 => 0x70 + (usage - 0x3A),
        // Modifiers. Without these a shortcut can never be reproduced: the letter
        // arrives but nothing is holding Ctrl, so it is just typed.
        0xE0 => 0xA2, // VK_LCONTROL
        0xE4 => 0xA3, // VK_RCONTROL
        0xE1 => 0xA0, // VK_LSHIFT
        0xE5 => 0xA1, // VK_RSHIFT
        0xE2 => 0xA4, // VK_LMENU (Alt)
        0xE6 => 0xA5, // VK_RMENU (AltGr)
        // Command maps to Windows, which is what a Mac user reaches for.
        0xE3 => 0x5B, // VK_LWIN
        0xE7 => 0x5C, // VK_RWIN
        // Letters and digits, needed for shortcuts addressed by key position.
        0x04..=0x1D => 0x41 + (usage - 0x04), // A..Z
        0x1E..=0x26 => 0x31 + (usage - 0x1E), // 1..9
        0x27 => 0x30,                         // 0
        0x2C => 0x20,                         // Space
        _ => 0,
    }
}

/// Press or release a key by its physical identity.
///
/// Returns `Unsupported` for a key this build does not know, so the caller can fall back
/// to typing the character instead of silently doing nothing.
pub fn inject_key(usage: u16, down: bool) -> Result<(), Error> {
    let vk = virtual_key_for(usage);
    if vk == 0 {
        return Err(Error::Unsupported("that key"));
    }
    let mut input = INPUT {
        r#type: INPUT_KEYBOARD,
        // SAFETY: INPUT is a union of POD; zeroing is a valid initial state.
        Anonymous: unsafe { core::mem::zeroed() },
    };
    input.Anonymous.ki = KEYBDINPUT {
        wVk: vk,
        wScan: 0,
        dwFlags: if down { 0 } else { KEYEVENTF_KEYUP },
        time: 0,
        dwExtraInfo: 0,
    };
    send_one(input)
}

// ---------------------------------------------------------------- file clipboard

// SAFETY CONTRACT: the classic Win32 clipboard file-list protocol. Explorer copies
// files as CF_HDROP — a global memory block holding a DROPFILES header and a
// double-null-terminated list of wide paths — and pastes anything that provides the
// same. Declared by hand like the rest of this backend.
#[link(name = "user32")]
unsafe extern "system" {
    fn OpenClipboard(hwnd: *mut core::ffi::c_void) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn IsClipboardFormatAvailable(format: u32) -> i32;
    fn GetClipboardData(format: u32) -> *mut core::ffi::c_void;
    fn SetClipboardData(format: u32, mem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
}
#[link(name = "shell32")]
unsafe extern "system" {
    fn DragQueryFileW(
        drop: *mut core::ffi::c_void,
        index: u32,
        out: *mut u16,
        capacity: u32,
    ) -> u32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> *mut core::ffi::c_void;
    fn GlobalLock(mem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    fn GlobalUnlock(mem: *mut core::ffi::c_void) -> i32;
    fn GlobalFree(mem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
}

const CF_HDROP: u32 = 15;
const GMEM_MOVEABLE: u32 = 0x0002;

/// The absolute paths of files currently on the clipboard, if any.
pub fn read_file_list() -> Result<Option<Vec<std::path::PathBuf>>, Error> {
    use std::os::windows::ffi::OsStringExt;
    // SAFETY: documented clipboard sequence; the clipboard is closed on every path.
    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP) == 0 {
            return Ok(None);
        }
        if OpenClipboard(core::ptr::null_mut()) == 0 {
            // Another program holds it; a poll simply tries again next tick.
            return Ok(None);
        }
        let drop = GetClipboardData(CF_HDROP);
        if drop.is_null() {
            CloseClipboard();
            return Ok(None);
        }
        let count = DragQueryFileW(drop, u32::MAX, core::ptr::null_mut(), 0);
        let mut paths = Vec::with_capacity(count as usize);
        for index in 0..count {
            let needed = DragQueryFileW(drop, index, core::ptr::null_mut(), 0);
            if needed == 0 {
                continue;
            }
            let mut buffer = vec![0u16; needed as usize + 1];
            let written =
                DragQueryFileW(drop, index, buffer.as_mut_ptr(), needed + 1) as usize;
            paths.push(std::ffi::OsString::from_wide(&buffer[..written]).into());
        }
        CloseClipboard();
        Ok(if paths.is_empty() { None } else { Some(paths) })
    }
}

/// Put a list of local files on the clipboard, exactly as Explorer would.
pub fn write_file_list(paths: &[std::path::PathBuf]) -> Result<(), Error> {
    use std::os::windows::ffi::OsStrExt;

    // DROPFILES: offset-to-paths, drop point, non-client flag, wide flag — then every
    // path NUL-terminated, then one more NUL to end the list.
    let mut wide: Vec<u16> = Vec::new();
    for path in paths {
        wide.extend(path.as_os_str().encode_wide());
        wide.push(0);
    }
    wide.push(0);
    let header: [u32; 5] = [20, 0, 0, 0, 1];
    let total = 20 + wide.len() * 2;

    // SAFETY: documented clipboard handoff. On success the system owns the memory; on
    // any failure it is freed here.
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, total);
        if mem.is_null() {
            return Err(Error::Platform("out of clipboard memory".into()));
        }
        let locked = GlobalLock(mem);
        if locked.is_null() {
            GlobalFree(mem);
            return Err(Error::Platform("could not lock clipboard memory".into()));
        }
        core::ptr::copy_nonoverlapping(header.as_ptr().cast::<u8>(), locked.cast(), 20);
        core::ptr::copy_nonoverlapping(
            wide.as_ptr(),
            locked.cast::<u8>().add(20).cast::<u16>(),
            wide.len(),
        );
        GlobalUnlock(mem);

        if OpenClipboard(core::ptr::null_mut()) == 0 {
            GlobalFree(mem);
            return Err(Error::Platform("the clipboard is held by another program".into()));
        }
        EmptyClipboard();
        if SetClipboardData(CF_HDROP, mem).is_null() {
            CloseClipboard();
            GlobalFree(mem);
            return Err(Error::Platform("the clipboard refused the file list".into()));
        }
        CloseClipboard();
    }
    Ok(())
}
