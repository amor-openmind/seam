# Platform input capture & injection — 2026 findings

Research pass completed 2026-07-25. macOS claims verified against the SDK headers on
this machine (macOS 26.5.2, build 25F84); Windows against Microsoft Learn; Wayland
against libei/portal upstream. Crate versions from crates.io on the same date.
Items marked **[UNVERIFIED]** need testing on real hardware before we depend on them.

---

## 1. macOS 26.x (Apple Silicon)

### Capture — `CGEventTapCreate`

Verified constants in `CGEventTypes.h`:

| Enum | Values |
|---|---|
| `CGEventTapLocation` | `kCGHIDEventTap=0`, `kCGSessionEventTap=1`, `kCGAnnotatedSessionEventTap=2` |
| `CGEventTapPlacement` | `kCGHeadInsertEventTap=0`, `kCGTailAppendEventTap=1` |
| `CGEventTapOptions` | `kCGEventTapOptionDefault=0x0`, `kCGEventTapOptionListenOnly=0x1` |
| disable reasons | `kCGEventTapDisabledByTimeout=0xFFFFFFFE`, `kCGEventTapDisabledByUserInput=0xFFFFFFFF` |

**Critical, quoted from `CGEvent.h`:** *"Taps may only be placed at `kCGHIDEventTap` by a
process running as the root user. NULL is returned for other users."*

→ Use `kCGSessionEventTap` for the unprivileged agent. A root daemon at
`kCGHIDEventTap` is only needed for the login window.

Also: key up/down events are only delivered if the caller is enabled for assistive
device access. Otherwise those bits are **silently cleared from the mask**, and if the
mask becomes empty `CGEventTapCreate` returns NULL.
→ **Treat NULL as "permission missing", never as "API broken".** This must produce a
specific message (goal criterion O5).

**Tap-disable-on-timeout must be handled.** A slow callback gets the tap disabled and a
`kCGEventTapDisabledByTimeout` event delivered. Re-enable with `CGEventTapEnable`.
→ **Never do network I/O in the callback.** Enqueue to a channel and return. This is
directly why seam's hot path is allocation-free.

**Unaccelerated deltas are available and are what we want:**
`kCGEventUnacceleratedPointerMovementX = 170` / `Y = 171`, plus
`kCGScrollWheelEventRawDeltaAxis1/2` (178/177). Read via `CGEventGetDoubleValueField`.
Do *not* forward `kCGMouseEventDeltaX/Y` — already accelerated by the sender's curve.

### Injection

`CGEventPost(kCGHIDEventTap | kCGSessionEventTap, event)`. Header notes injected events
**pass through your own tap** → tag them with `CGEventSourceSetUserData` /
`kCGEventSourceUserData` and filter, or you get a feedback loop.

Unicode: `CGEventKeyboardSetUnicodeString`. Header warning, verbatim: *"application
frameworks may ignore the Unicode string in a keyboard event and do their own
translation based on the virtual keycode and perceived event state."*
→ Real: Electron, Java and some games ignore it. **Primary path must be a
`UCKeyTranslate` + `TISCopyCurrentKeyboardLayoutInputSource` /
`kTISPropertyUnicodeKeyLayoutData` reverse mapping** (find the keycode+modifiers that
produce the target glyph on the *local* layout), with `SetUnicodeString` as fallback.
This matters for `LayoutPolicy::Logical` / `Auto`.

### Cursor — verified header text

- `CGWarpMouseCursorPosition(CGPoint)` — moves the cursor *"without generating events"*.
- `CGAssociateMouseAndMouseCursorPosition(false)` — *"all events received by your
  application have a constant absolute location but contain mouse delta data."*
  **This is exactly the "cursor frozen at the edge while forwarding" primitive.**
- `CGDisplayHideCursor` / `CGDisplayShowCursor` — the `display` parameter is ignored, and
  they maintain a **refcount**. Unbalanced calls leave the cursor permanently hidden.
- `CGEventSourceSetLocalEventsSuppressionInterval` — **default 0.25 s**. After posting an
  event, local hardware input is ignored for 250 ms. **Set it to 0.0** or the machine
  feels broken while forwarding. This is a classic bug in this software class.

deskflow calls `CGAssociateMouseAndMouseCursorPosition(true/false)` around hide/show,
noting it *"appears to fix 'mouse randomly not showing' bug."*

### Permissions

Two **separate** TCC grants, both needed:
- **Accessibility** (`kTCCServiceAccessibility`) — `AXIsProcessTrustedWithOptions` +
  `kAXTrustedCheckOptionPrompt`.
- **Input Monitoring** (`kTCCServiceListenEvent`) —
  `IOHIDRequestAccess(kIOHIDRequestTypeListenEvent)` / `IOHIDCheckAccess`.

TCC keys on **code-signing identity + bundle ID**. An ad-hoc-signed binary loses its
grant on every rebuild. → **Ship a signed, notarized `.app` with a stable Team ID.** A
bare CLI binary is a support nightmare. Reset with
`tccutil reset Accessibility <bundle-id>`. A sandboxed Mac App Store app **cannot do
this at all** — no entitlement exists for global event taps.

### Secure input

`EnableSecureEventInput()` suppresses keystroke delivery to taps — **including at
`kCGHIDEventTap`**. It cannot be defeated and should not be. Detect with
`IsSecureEventInputEnabled()`; identify the offending process via the IORegistry
console-user key `kCGSSessionSecureInputPID`.
→ **Correct behaviour: detect, name the app in a clear warning, pause forwarding.**
Many apps (some terminals, 1Password) leave it on longer than expected.

### Login window

deskflow uses a long-standing Mach-O section trick plus a root `LaunchDaemon`:

```c
__attribute__((used)) __attribute__((section("__CGPreLoginApp,__cgpreloginapp")))
static const char magic_section[] = "";
```

Track the console session with `CGSessionCopyCurrentDictionary` / `kCGSSessionOnConsoleKey`
for fast user switching.

**[UNVERIFIED]** Whether macOS 26 extended periodic re-authorization prompts to Input
Monitoring. Test on real hardware.

---

## 2. Windows 11 (24H2 / 25H2)

### Capture — ranked

1. **`SetWindowsHookEx(WH_KEYBOARD_LL=13 / WH_MOUSE_LL=14)` — the pragmatic choice.**
   Global only; no DLL required (callback runs in-process), but the installing thread
   **must pump messages**. Filter own injections via `LLKHF_INJECTED` (0x10) /
   `LLMHF_INJECTED` (0x01) and `LLKHF_LOWER_IL_INJECTED` (0x02).
   **If the callback exceeds `HKCU\Control Panel\Desktop\LowLevelHooksTimeout`
   (~300 ms default) the hook is silently removed** — detect and reinstall.
2. **Raw Input** (`RegisterRawInputDevices` + `WM_INPUT`, `usUsagePage=0x01`,
   `usUsage=0x02`/`0x06`, `RIDEV_INPUTSINK`) — clean unfiltered relative deltas.
   `RIDEV_NOLEGACY` only affects *your own* queue; it is **not** system-wide suppression.
   → **Use both: LL hook for suppression, Raw Input for delta fidelity.**
3. **Interception driver** — real system-wide suppression, but Secure Boot / HVCI /
   driver-blocklist friction, admin install, reboot. **[UNVERIFIED]** current viability
   under 2026 driver policy. Research spike, not a plan.

`WH_JOURNALRECORD` / `WH_JOURNALPLAYBACK` are **dead** — Learn states journaling hooks
are not supported on Windows 11.

### Suppression & UIPI

Return `1` instead of `CallNextHookEx`. **Cannot** suppress Ctrl+Alt+Del, Win+L, the UAC
secure desktop, or anything while a **higher-integrity-level window** has focus.

That last one is the #1 support complaint for this software class: an unelevated hook
"randomly stops working" when the user focuses an elevated app. Mitigate by running
elevated, or `uiAccess="true"` in the manifest (requires Authenticode signing **and**
installation under `%ProgramFiles%`).

### Injection — `SendInput`

Verified Remarks, and this one is important: *"This function fails when it is blocked by
UIPI. Note that **neither GetLastError nor the return value will indicate the failure was
caused by UIPI blocking**."*
→ UIPI failure is **invisible**. Instrument it or drown in "it randomly stops working".

Also: events in one call are atomic (*"not interspersed with other input events"* —
batch a chord into one `SendInput`), and it *"does not reset the keyboard's current
state"* → always send explicit key-ups on teardown, or strand a stuck modifier. This is
independent confirmation of the `KeyState` reconciliation design.

Verified `MOUSEINPUT.dwFlags`: `MOVE=0x0001`, `LEFTDOWN=0x0002`, `LEFTUP=0x0004`,
`RIGHTDOWN=0x0008`, `RIGHTUP=0x0010`, `MIDDLEDOWN=0x0020`, `MIDDLEUP=0x0040`,
`XDOWN=0x0080`, `XUP=0x0100`, `WHEEL=0x0800`, `HWHEEL=0x1000`, `MOVE_NOCOALESCE=0x2000`,
`VIRTUALDESK=0x4000`, `ABSOLUTE=0x8000`. `WHEEL_DELTA=120`.

**Multi-monitor requires `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`** — without
`VIRTUALDESK`, absolute coordinates map to the **primary monitor only**. Conversion (note
the `-1`, the off-by-one that makes the last row/column unreachable):

```rust
let dx = ((x - vx) * 65535) / (vcx - 1);
let dy = ((y - vy) * 65535) / (vcy - 1);
```

with `SM_XVIRTUALSCREEN` / `SM_YVIRTUALSCREEN` (**negative** for monitors left/above
primary) and `SM_CXVIRTUALSCREEN` / `SM_CYVIRTUALSCREEN`.

**Never forward relative motion for cursor positioning on Windows** — relative
`MOUSEEVENTF_MOVE` is subject to mouse speed and the two-mouse threshold and can be
multiplied by **up to 4×**. Use absolute + `VIRTUALDESK`.

Keyboard: `KEYEVENTF_KEYUP=0x0002`, `KEYEVENTF_SCANCODE=0x0008` (**prefer scancodes** —
DirectInput games ignore virtual-key injection), `KEYEVENTF_EXTENDEDKEY=0x0001` (required
for arrows, Ins/Del/Home/End/PgUp/PgDn, NumLock, RCtrl, RAlt, Numpad `/`, Numpad Enter,
PrintScreen), `KEYEVENTF_UNICODE=0x0004` (set `wVk=0`; non-BMP needs an explicit
**surrogate pair** = two INPUT entries).

### Cursor

`ClipCursor` to confine — *"The cursor is a shared resource… must release… before
relinquishing control"*, needs `WINSTA_WRITEATTRIBUTES`. Confinement is lost on focus
change; reassert on `WM_ACTIVATEAPP` / `WM_DISPLAYCHANGE`.

**You cannot hide the cursor system-wide.** `ShowCursor(FALSE)` is refcounted and
per-thread-queue only. `SetSystemCursor` with a blank cursor works but leaks a blank
cursor if you crash. → **Park the cursor at a screen corner instead of hiding it.**

### DPI

Declare **PerMonitorV2** in the manifest, or `SetProcessDpiAwarenessContext` before any
window exists. A DPI-unaware process gets *virtualized, lied-to* coordinates from
`GetCursorPos`, which silently breaks edge detection on mixed-DPI setups. Not optional.

### Deployment

No admin needed for LL hooks or `SendInput` at equal IL. A **session-0 service cannot
inject into the user session** — needs the standard split:
`WTSGetActiveConsoleSessionId` → `WTSQueryUserToken` → `CreateProcessAsUser`.
Budget for an **EV code-signing certificate**: a global keyboard hook is textbook
keylogger behaviour and unsigned builds get flagged by Defender/SmartScreen.

---

## 3. Linux / X11 — legacy tier

X11 has **no permission model**: any client with the `MIT-MAGIC-COOKIE` captures and
injects everything. This is exactly why Wayland broke it.

- **Capture (non-suppressing):** XInput2 raw events on the root window — `XISelectEvents`
  with `XI_RawMotion` / `XI_RawKeyPress` / `XI_RawButtonPress` on `XIAllMasterDevices`.
  `XIRawEvent.raw_values` gives **unaccelerated** valuators (forward these);
  `valuators.values` is accelerated.
- **Capture (suppressing):** `XIGrabDevice` / `XGrabPointer` / `XGrabKeyboard`. Handle
  `AlreadyGrabbed`, `GrabFrozen`, `GrabNotViewable`, `GrabInvalidTime`. Classic failure:
  **an open GTK/Qt menu already holds a grab** → retry with backoff. Never `XGrabServer`.
- **Edge detection:** XFixes **pointer barriers** (`XFixesCreatePointerBarrier`,
  `XI_BarrierHit` / `XI_BarrierLeave`, `XIBarrierReleasePointer`). Far better than
  polling `XQueryPointer`. Same conceptual model as the Wayland InputCapture portal →
  **one shared barrier/zone abstraction covers both.**
- **Injection:** XTEST. **Call `XTestGrabControl(dpy, True)` or your own grabs block your
  own injection.** `XSendEvent` is useless (apps check the `send_event` flag).
- **Unicode:** the xdotool trick — bind keysym `0x01000000 + codepoint` to a spare keycode
  via `XChangeKeyboardMapping`, inject, restore. **Inherently racy**; causes
  `MappingNotify` storms. → Cache one persistent scratch keycode rather than remapping
  per character.
- **Xwayland trap:** XTEST and XI2 grabs inside Xwayland reach **X clients only**. An X11
  backend running under Wayland does not work.

**Strategic:** GNOME **removed X11 session code in GNOME 49**; Fedora 43 ships
Wayland-only; KDE plans to drop X11 in **Plasma 6.8 (early 2027)**.
→ Build the X11 backend as a **compatibility tier**, mainly for receive-side injection.

---

## 4. Linux / Wayland — the important one

2023-era answers are wrong. Current architecture:

- **Injection** = `org.freedesktop.portal.RemoteDesktop` → `ConnectToEIS()` → libei sender
- **Capture** = `org.freedesktop.portal.InputCapture` → `ConnectToEIS()` → libei receiver

libei has been in both portals since **xdg-desktop-portal 1.17** (mid-2023).

### InputCapture portal — currently **version 2**

| Member | Ver | Notes |
|---|---|---|
| `CreateSession` | 1 | **Deprecated** — use `CreateSession2` |
| `CreateSession2` | 2 | creates without starting |
| `Start` | 2 | triggers the consent dialog |
| `GetZones` | 1 | `zone_set` id + regions |
| `SetPointerBarriers` | 1 | horizontal/vertical only |
| `Enable` / `Disable` / `Release` | 1 | `Release` takes `activation_id` |
| `ConnectToEIS` | 1 | **must be called before `Enable`** |

Signals: `Activated` (`activation_id`, `barrier_id`), `Deactivated`, `Disabled`,
`ZonesChanged`. `SupportedCapabilities`: `KEYBOARD=1`, `POINTER=2`, `TOUCHSCREEN=4`.

### libei is moving fast

- **1.6 (May 2026)** — **keysym and text events** via a new `ei_text` interface. This
  solves Unicode injection on Wayland far more cleanly than the X11 remap hack. But
  upstream says *"expect this to hit the next compositor version (or the one after
  that)"* → **not usable in shipping compositors as of July 2026.** Keep a
  keycode+keymap fallback.
- **1.7 (imminent)** — `ei_gestures`.
- **Session persistence** (no repeated permission prompts) landed in **portals 1.21.0** —
  important for our onboarding goal (O2).

### Compositor reality matrix (2026)

| Compositor | Capture | Injection |
|---|---|---|
| **GNOME / Mutter** (≥45) | ✅ InputCapture portal | ✅ libei / RemoteDesktop |
| **KDE / KWin** (Plasma ≥6.1) | ✅ InputCapture portal | ✅ libei / RemoteDesktop |
| **wlroots / Sway** (≥1.8) | ⚠️ layer-shell edge windows only | ✅ `zwlr_virtual_pointer_v1` + `zwp_virtual_keyboard_v1` |
| **Hyprland** | ⚠️ layer-shell | ✅ wlroots protocols |
| **COSMIC** | ❌ | ✅ wlroots protocols **[partly UNVERIFIED]** |

**Honest summary: injection is solved everywhere; capture is solved only on GNOME and
KDE.** On wlroots-family compositors the only option is 1-pixel layer-shell edge
surfaces (what lan-mouse does), which do not truly suppress.

### Supporting protocols

- `zwp_locked_pointer_v1` / `zwp_confined_pointer_v1` + `zwp_relative_pointer_v1` — the
  "cursor frozen at edge, deliver deltas" primitive. Direct analogue of macOS
  `CGAssociateMouseAndMouseCursorPosition(false)`.
- `zwp_keyboard_shortcuts_inhibitor_v1` — swallow compositor shortcuts (Super, Alt+Tab).
- **Cursor hiding is per-surface only** on Wayland, by design. Use pointer-lock.

### Universal fallback: uinput + EVIOCGRAB

Below the compositor, so it works everywhere including the console.
- **Inject:** `/dev/uinput` (`UI_SET_EVBIT`, `UI_SET_KEYBIT`, `UI_DEV_SETUP`,
  `UI_DEV_CREATE`). **Hotplug race:** wait ~200 ms after create or the first events drop.
- **Capture + suppress:** `EVIOCGRAB` — exclusive, true suppression, any compositor.
- **Cost:** root or an `input`-group udev rule. `EVIOCGRAB` on the user's only keyboard is
  **genuinely dangerous** — a crash locks them out.
  → **Always install a deadman watchdog that releases the grab.** This is the Linux
  expression of goal criterion R2.

---

## 5. ChromeOS — the honest answer

**Google documents that this does not work.** The official ChromeOS Linux FAQ names
Synergy specifically: *"Synergy will not work"*, because it requires capturing and
spoofing input for all windows — described as a **deliberate security boundary to prevent
container escape**. This is design, not oversight.

Ozone reads `/dev/input/event*` on the **host**. Crostini is a separate VM; input reaches
container apps only as Wayland events forwarded host→Sommelier. There is no path from
inside the VM to the host's evdev stack.

**Second, independent blocker:** Crostini networking is *"only at layer 3"*, NAT'd, with
**no inbound LAN connections to the container**. So a Crostini peer cannot even be
reached normally without an outbound-initiated tunnel.

| Surface | Global capture | Suppression | Injection | Cursor lock |
|---|---|---|---|---|
| Crostini app | ❌ VM-isolated | ❌ | ❌ host only own X clients | ⚠️ own window |
| Android (ARCVM) | ❌ `INJECT_EVENTS` is signature-level | ❌ | ❌ | ❌ |
| Chrome extension (MV3) | ❌ | ❌ | ⚠️ `chrome.debugger` CDP, tabs only | ❌ |
| **PWA (fullscreen)** | ⚠️ **only while focused** | ⚠️ within page | ❌ | ✅ Pointer Lock + Keyboard Lock |
| Dev-mode native | ⚠️ | ⚠️ | ⚠️ | ⚠️ Powerwash, enterprise-blocked |

**The one viable degraded mode:** a fullscreen PWA as a **receive-only client** —
`requestFullscreen()` → `requestPointerLock()` (relative deltas, hidden cursor) →
`navigator.keyboard.lock()`. Both need a user prompt (Chrome 130+), and Chrome enforces a
**two-second Esc long-press escape hatch that cannot be suppressed**.

> **Verdict for `docs/GOAL.md` §7:** a Chromebook can be a **client only** — receiving
> input into a fullscreen PWA plus clipboard sync. It can **never** be a capture source
> and can **never** inject into the ChromeOS shell. Dev mode is not shippable.
> This is a hard limit to document, not a gap to close.

**[UNVERIFIED]** ChromeOS/Android convergence ("Aluminium") and Borealis input access.

---

## 6. Rust crate audit (crates.io, 2026-07-25)

### macOS
| Crate | Ver | Released | Verdict |
|---|---|---|---|
| `objc2` | **0.6.4** | 2026-02-26 | ✅ Actively maintained |
| `objc2-core-graphics` | 0.3.2 | 2025-10-04 | ✅ **Modern choice** |
| `core-graphics` | 0.25.0 | 2025-05-27 | ⚠️ Servo family; what lan-mouse/enigo use |
| `cocoa` / `objc` | — | — | ❌ Deprecated |

We will write raw `extern "C"` for `CGEventTapCreate` run-loop plumbing,
`CGAssociateMouseAndMouseCursorPosition`, `CGEventSourceSetLocalEventsSuppressionInterval`,
`IsSecureEventInputEnabled`, `IOHIDRequestAccess` — a thin internal `macos-sys` module
beats fighting binding gaps.

### Windows
| Crate | Ver | Released | Verdict |
|---|---|---|---|
| `windows-sys` | **0.61.2** | 2025-10-06 | ✅ **Use this.** Raw FFI, fast compile |
| `windows` | 0.62.2 | 2025-10-06 | Only if WinRT `InputInjector` is needed |
| `winapi` | 0.3.9 | **2020-06-26** | ❌ Retired |

Pin it — windows-rs breaks API every minor release.

### Linux
| Crate | Ver | Released | Verdict |
|---|---|---|---|
| `x11rb` | **0.14.0** | **2026-07-16** | ✅ Pure Rust; `xinput`/`xtest`/`xfixes`/`xkb`/`randr` |
| `wayland-client` | **0.31.15** | **2026-07-22** | ✅ Smithay, very active |
| `wayland-protocols-wlr` | 0.3.12 | 2026-03-31 | ✅ `virtual_pointer`, `layer_shell` |
| `wayland-protocols-misc` | 0.3.12 | 2026-03-31 | ✅ `zwp_virtual_keyboard_v1` |
| **`reis`** | **0.7.0** | **2026-06-16** | ⚠️ **Load-bearing, use with caution** |
| `ashpd` | **0.13.13** | **2026-07-17** | ✅ Has `desktop::input_capture` + `remote_desktop` |
| `evdev` | 0.13.2 | 2025-09-15 | ✅ `uinput::VirtualDeviceBuilder`, `grab()` |

**`reis` deserves scrutiny — it is our only libei path.** Pure-Rust libei/libeis, healthy
cadence (0.5→0.6→0.7), but **its own docs say "currently *incomplete* and subject to
change"** and it has only ~95K downloads. It is used in production by lan-mouse, which is
the strongest signal available.
→ **Use it, pin the exact version, vendor a fork.** Do not write our own libei; the
protocol is large and moving.

`ashpd` documents a real gotcha: xdg-desktop-portal-gnome 46.0 has a bug preventing
re-enabling a disabled session, so barriers can't be reconfigured after enabling.

### Cross-platform "do it all" crates — all rejected
| Crate | Ver | Verdict |
|---|---|---|
| `enigo` | 0.6.1 | ⚠️ **Injection only, no capture.** Self-describes as "early alpha" |
| `rdev` | 0.5.3 (2023-06) | ❌ **Abandoned** |
| `device_query` | 4.0.1 | ❌ Polling-based; wrong model, no suppression or ordering |
| `inputbot`, `mouce`, `autopilot` | — | ❌ Stale or wrong problem |

**None of them do capture + suppression + cursor locking.** That combination *is* the
product. Every one stops at injection. This settles it: we write the backends ourselves.

### What lan-mouse actually ships (most valuable single data point)

- capture/libei: `reis 0.7.0` + `ashpd 0.13.9` *(default feature)*
- capture/layer-shell: `wayland-client 0.31.1`, `wayland-protocols-wlr 0.3.1`
- capture/x11: `x11 2.21` (we can do better with `x11rb 0.14`)
- emulation/wlroots: + `wayland-protocols-misc 0.3.1`
- macOS: `core-graphics 0.25`, `core-foundation 0.10`, `keycode 1.0`
- Windows: `windows 0.61.2`
- transport: `tokio`, `rustls 0.23`, `webrtc-dtls`, `rcgen` — **DTLS over UDP/4242**

→ **Read lan-mouse's source before writing a backend.** Note its transport choice
(DTLS/UDP) differs from ours (QUIC) — see `docs/research/transport.md` once that
research pass lands.

---

## 7. Decisions this research settles

| Decision | Rationale |
|---|---|
| Trait-per-capability core (`Capturer`, `Injector`, `CursorController`) with per-platform backends | No crate provides capture+suppression; we own the backends |
| **One shared barrier/zone abstraction** across XFixes pointer barriers and the Wayland InputCapture portal | Verified: they use the same conceptual model |
| macOS: `kCGSessionEventTap`, suppression interval 0, tag our own injected events, forward *unaccelerated* deltas | Verified from SDK headers |
| Windows: `windows-sys 0.61`, LL hook **+** Raw Input, always `ABSOLUTE\|VIRTUALDESK`, PerMonitorV2 manifest | Verified from Learn |
| Wayland: `ashpd` + `reis` (pinned, vendored); wlroots protocols for injection; `uinput`+`EVIOCGRAB` escape hatch with deadman timer | Capture only exists on GNOME/KDE |
| X11: `x11rb 0.14`, compatibility tier only | GNOME 49 already deleted its X11 session |
| **ChromeOS: receive-only PWA client, documented as a hard limit** | Google documents the server role as deliberately impossible |

### Top risks
1. `reis` self-declares as incomplete and is our only libei path → vendor it.
2. Wayland **capture** exists only on GNOME/KDE → wlroots users get a degraded tier.
3. **macOS secure input and Windows UIPI both fail silently and invisibly** → instrument
   both explicitly, or we inherit the "it randomly stops working" bug reports that define
   this software category.
