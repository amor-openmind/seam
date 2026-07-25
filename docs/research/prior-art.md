# Prior art — protocols, bug classes and lessons

Research pass completed 2026-07-25. Protocol details read from source; issue data
verified live via the GitHub API.

## Landscape status

| Project | Lang | Stars | Last commit | Commits/52w | Open | Verdict |
|---|---|---:|---|---:|---:|---|
| `debauchee/barrier` | C++ | 30.8k | **2022-02-04** | **0** | 1046 | **Dead.** Not formally archived — no banner, tracker still open |
| `input-leap/input-leap` | C++ | 8.3k | 2025-11-27 | **5** | 815 | **Stalled.** Zero commits in 2026; maintainer resigned 2026-05-25 |
| `deskflow/deskflow` | C++/Qt6 | 27.6k | 2026-07-23 | **1202** | 189 | **The live upstream** |
| `feschber/lan-mouse` | **Rust** | 5.1k | 2026-05-26 | 96 | 115 | **Active**, single maintainer |
| `htrefil/rkvm` | **Rust** | 541 | **2024-07-13** | 0 | 35 | **Dormant** |
| `~nickbp/nikau` (SourceHut) | **Rust** | — | 2025-10 | — | — | **Active, nearly invisible.** Best-designed Linux-only option |

Deskflow *is* the old `symless/synergy-core`, renamed Sept 2024 — issue numbers below
~#7400 are legacy Synergy threads. Deskflow is the open upstream; Synergy is the
commercial downstream.

## The Barrier/Synergy/Deskflow wire protocol

TCP port 24800, 4-byte big-endian length prefix, then a 4-ASCII-char opcode plus
parameters marshalled by a `%` mini-language (`%1i`/`%2i`/`%4i` big-endian ints,
`%s` = 4-byte length + bytes, `%7s` = the *only* fixed-length field, used in the greeting).

Handshake: `kMsgHello = "Barrier%2i%2i"` → `kMsgHelloBack = "Barrier%2i%2i%s"`. Deskflow
generalised the literal to `%7s`, selectable between `"Barrier"` and `"Synergy"` — **that
string is the entire difference between the two "protocols".** Both sides negotiate down
to the minimum version.

Key messages: `CINN` enter (x, y, **sequence**, toggle-modifier mask) · `COUT` leave ·
`CALV` keep-alive (3 s interval, 9 s death) · `CCLP` clipboard grab · `CIAK` ack ·
`DKDN`/`DKUP`/`DKRP` key down/up/repeat (KeyID, modifier mask, **KeyButton**) ·
`DMMV` absolute motion · `DMRM` relative · `DMWM` wheel (±120/detent) ·
`DCLP` clipboard data (chunked) · `DINF` screen info · `SECN` macOS secure-input notice ·
`LSYN`/`DKDL` language sync (1.8+).

Versions: 1.0 base · 1.1 KeyButton · 1.2 relative motion · 1.3 keep-alive · 1.4 TLS ·
1.5 file transfer · 1.6 clipboard streaming · 1.7 secure-input · **1.8 (Jun 2025)
language sync**. Barrier/InputLeap stop at 1.6; Deskflow is at 1.8.

### Security history — read before designing anything

| CVE | CVSS | Defect |
|---|---:|---|
| **CVE-2021-42072** | **8.8** | *"does not sufficiently verify the identity of connecting clients"* — **no application-layer authentication at all** |
| **CVE-2021-42073** | **8.2** | Any client supplying a valid label (**default `"Unnamed"`**) could join, *"capture input device events from the server, and also modify the clipboard content"* |
| CVE-2021-42074 | — | Crash on TCP disconnect right after `Hello` |
| CVE-2021-42075 | — | FD leak on failed handshake blocked all new connections |
| CVE-2021-42076 | — | **No maximum message length** → trivial memory-exhaustion DoS |

All fixed in v2.3.4/v2.4.0 (2021-11-01) — but **the auth fix shipped disabled by default**
for backward compatibility. seam's `MAX_FRAME_LEN` and `PeerId`-based identity exist
because of this list.

## Bug classes, with issue numbers

### Keyboard layouts — the hardest problem
- **The AltGr disaster.** Barrier synthesizes AltGr as **Ctrl+Alt**, which Windows treats
  as its own layout-switch chord — so AltGr+2 on a Spanish layout makes *Windows itself*
  switch to English and type `2` instead of `@`. deskflow#4411 (**161 comments**, top-
  reacted open bug), barrier#100, #217 (Swedish ÅÄÖ), #1280 (Polish), #186 (Neo2),
  inputleap#2084, #1600 (bépo).
  → seam's `Modifiers::is_command_chord` explicitly excludes `RIGHT_ALT`, with a test.
- **Dead keys.** After `ToUnicodeEx` returns a dead key, Windows never resets the state, so
  the *next* call composes against the stale one. The fix (inject `VK_SPACE` to flush)
  exists three times: **merged** in Deskflow (PR #7149, 2022), **never merged** in Barrier
  (PR #1583), **closed unmerged** in InputLeap (PR #2107).
- barrier#860 *"Work on UTF-8 support"* — **74 reactions**, went nowhere.
- **Stuck modifiers, two root causes:** (1) lost key-up on screen switch — inputleap#2045
  is the cleanest repro; barrier#1845 (switch hotkey's own modifiers latch at the
  destination); barrier#207. (2) Wayland/libei modifier desync — libei's own author
  diagnosed inputleap#1869 as a `FIXME` leaving `EI_EVENT_KEYBOARD_MODIFIERS` unhandled;
  still open as deskflow#8437, #9011. Deskflow tracks the class under #8450 (32 sub-issues).
  → On X11 the latched state lives in the **X server, not the app** — killing the client
  does nothing.

### Cursor trapped at the edge *is* the DPI bug
Barrier's maintainers labelled #94 and #206 **`HiDPI`**. The server sends coordinates in an
unscaled space the client cannot invert, so position clamps to the far corner. Fixed in
InputLeap 3.0.3 and in Deskflow; **never released in Barrier**.
barrier#1638 is a trap: 4K@250% worked on 2.3.3, broke on 2.4.0; the fix (commit
`00a57ea9` "Restore dpiAwareness") **is on master but was never released**.

> **Structural flaw not to inherit:** `DINF`/`DMMV` use `%2i` — **signed 16-bit**. A
> virtual desktop wider than 32767 px cannot be expressed. seam uses `i32` throughout.

### Laggy pointer — three separable causes
1. **The TLS/OpenSSL write path.** inputleap#2102 — the workaround is *disable SSL*,
   independently confirmed by 3+ users. Same in deskflow#8773, #8108, #9364.
2. **macOS SDK regression.** inputleap#2367, the sharpest report anywhere: built against
   the **Tahoe (macOS 26) SDK** → 2–3 s lag. Instrumented
   `IOHIDManagerRegisterInputReportCallback`: **P50 998 µs vs P99 154 ms / P99.9 163 ms**,
   ~5.3 stalls/s of 50–164 ms.
3. **Coalescing / NIC power-save.** barrier#300 (79c): smooth while moving continuously,
   but any pause of a few hundred ms makes the next movement *jump*. barrier#1755 —
   extreme Wi-Fi lag **unless a speedtest is running**. barrier#1110 — >1000 Hz polling
   melts the machine.

### First connection / TLS setup — the user's exact complaint
**Barrier's missing-`Barrier.pem` family is its single biggest user-facing killer:**
#231 (**131 comments**) → #1377 → #1609. A direct regression from v2.4.0's own release
note (*"no longer uses openssl CLI tool… hooks into the openssl library directly"*): the
in-process path silently fails on fresh installs and the server won't start. Unfixable —
v2.4.0 is final. deskflow#8598: endless TLS fingerprint dialog loop.
inputleap#2188: running InputLeap and Deskflow on one host makes both fight over port 24800.

### macOS permissions
Root cause is a **TCC identity mismatch**, named in inputleap#2224: the `.app` bundle and
the CLI binary inside it are **separate TCC subjects** with inconsistent bundle IDs, so
`FATAL: assistive devices does not trust this process` persists even with Accessibility
granted. Compounded by an unsigned, un-notarized DMG (barrier#1860, #448). Recurs with
*every* macOS major release. deskflow#8400 (**still open**): Deskflow doesn't declare
**Input Monitoring** in its plist, so keystroke forwarding from a macOS server silently
fails.

### Windows UAC — and a real security defect
UAC prompts render on a separate secure desktop a normal-privilege hook cannot reach.
There is a real fix and it is a setting: **`Elevate: Always`** — inputleap#2346 states
there is *no* security benefit to `As Needed`; Deskflow removed the auto option (#8458).

> 🔴 **inputleap#2143:** on the Windows lock screen the **mouse crosses over** and
> dismisses the screensaver, but **keyboard input silently stays on the server** — so the
> typed password goes to whatever has focus on the *source* machine. The reporter nearly
> posted his password into a web form. barrier#1559 is the same class: no UI feedback at
> all when input stops because an elevated window took focus.
> → **seam rule: if keyboard forwarding stops, stop the pointer too, and say so visibly.**

### Wayland
Barrier never delivered it. #109 is the most-commented issue in the repo (243c/187r),
rewritten into a BountySource campaign that **collected $2,550.42** with a public 3-month
commitment. The promised progress tracker (#1251) has 53 reactions, 3 comments, last
updated May 2023. Nothing shipped.

## Clipboard — the biggest opportunity in the ecosystem

**Only three formats, ever:** Text (UTF-8, LF only), Bitmap (BMP *without* the 14-byte
file header), HTML. Chunked at 32 KiB (Barrier) / 512 KiB (Deskflow).

**Files and folders: never supported by any of them.**
[barrier#855](https://github.com/debauchee/barrier/issues/855) (drag-and-drop) has
**236 reactions — the most-reacted issue in the repo** — and was never implemented.
Deskflow **deleted** the broken D&D code in v1.22.0; `DFTR` is marked `@deprecated`.
lan-mouse has **no clipboard at all** (#105, 36 comments since 2024; PR #327 still open).
rkvm refuses it on Unix-philosophy grounds (#69). nikau has it, but is Linux-only.

Failure modes to avoid:
- 🔴 **Size blocks the input path.** barrier#775: an ~85 MB JPG in the Windows clipboard
  plus a hotkey switch → **all mouse and keyboard forwarding dies** until you kill
  `barriers.exe`. deskflow#8198: cursor bounces at the edge when the clipboard limit is hit.
- deskflow#9869: a top-down DIB with **negative `biHeight`** throws `std::length_error` and
  crashes the Windows server on switch.
- barrier#1085: sync silently stops after hours; restart restores it *without re-copying*,
  so the data was fine — the ownership handshake died.

**Wayland clipboard was architecturally blocked** and has only just been solved: the portal
clipboard interface covers **RemoteDesktop** sessions, while these apps use
**InputCapture** (inputleap#1698). Deskflow's answer is a V1/V2 split — clipboard requires
**GNOME 50+ / Plasma 6.7.1+**, and even then there is **no HTML and no files**.

> **This is seam's clearest differentiator.** Files and folders is the largest unmet demand
> in the entire ecosystem, and no open-source competitor has it.

## lan-mouse — the closest Rust prior art

DTLS 1.2 over UDP (`webrtc-dtls 0.12`), port 4242. Fixed 21-byte events, no length prefix
(DTLS gives record boundaries). Self-signed certs with **SHA-256 fingerprint TOFU**.
`connect_any` races all known addresses in a `JoinSet` and takes the first success — a neat
answer to multi-homed hosts.

**Event model is the middle path** between rkvm's raw-HID purity and Synergy's semantic
KeyIDs: **Linux evdev keycodes + xkb modifier state** on the wire, translated per backend.
Consequence: the client's own layout resolves the key, so non-Latin layouts, dead keys and
compose all work natively — **the AltGr/dead-key class simply doesn't exist**. The cost:
**no absolute positioning at all** (relative-only), so there is no coordinate/DPI problem
*and* no way to place the cursor.

Its `input-capture` / `input-emulation` / `input-event` crates are the only published
cross-platform Rust abstraction for this problem — but note **GPL-3.0** and a stale
`reis ^0.5` pin (reis is at 0.7).

Its own bugs worth learning from: #205 keys not released on transition · #450 four traced
macOS bugs, including `modifier_event` building a bare `CGEvent` whose keycode is
**0 = `kVK_ANSI_A`** (so holding Ctrl registers as Ctrl+A), modifiers routed through a
**single** key-repeat slot so each new keypress posts the previous modifier's keyUp while
still held, and Mod5/AltGr silently dropped · #307 **falls over permanently on lossy
Wi-Fi**, no robust reconnect · #314 a single mouse event **leaks into chained clients**.

## Commercial

- **Synergy** — 🔴 *"local TLS encryption is available in higher editions"*. **TLS is a
  paid-tier feature**; the basic tier sends keystrokes in plaintext over the LAN.
- **ShareMouse** — the most feature-complete: drag & drop of **files *and folders***,
  clipboard covering formatted text, bitmaps and multi-file folders, targetable at a
  *specific* computer; Windows Fast User Switching and UAC support; can send Ctrl+Alt+Del.
- **Mouse Without Borders** (PowerToys) — TCP 15100/15101, full mesh, max 4 machines.
  🔴 **AES-256-CBC with `PaddingMode.Zeros` and no MAC — unauthenticated and malleable.**
  PowerToys#49147 is the best bug report in the survey: a failed clipboard transfer hits
  `catch (InvalidOperationException) { break; }`, **exits the accept loop permanently with
  no watchdog**, and subsequent copies feed unparseable packets in until `errCount > 5`
  tears down the whole link — clipboard corruption escalating into total input loss.
- **Apple Universal Control** — max 3 devices, same Apple Account + 2FA, within 10 m, over
  BLE discovery + AWDL. Full file drag-and-drop. The two things Apple gets right that
  nobody else does: **zero configuration** (no IPs, no layout editor — identity comes from
  the account) and the **push-through affordance**, where the pointer visibly pushes into
  the other screen before committing, making accidental switching nearly impossible.
- **Logitech Flow** — the most under-appreciated design in the field. **The input never
  crosses the network**: the mouse is paired to each computer on a different channel and
  *the mouse itself* switches channels at the edge. Only the switch signal and clipboard go
  over the LAN. Essentially zero added input latency, immune to packet loss. Only possible
  with proprietary hardware — but it argues strongly for **decoupling the input path from
  the bulk-data path**, which is exactly what seam's datagram/stream split does in software.

## Lessons — adopted

1. **Release by physical key, not by symbol.** Deskflow's `KeyButton` + `m_serverKeys[]`
   map exists because *"the KeyID on release may not be the KeyID of the press"* with dead
   keys or mismatched layouts. Every project that skipped this has an open stuck-modifier
   bug. → seam's `KeyState` is keyed on `PhysicalKey`.
2. **Sync toggle modifiers on enter.** `CINN`'s 4th field carries Caps/Num/Scroll Lock.
   → seam's `Enter` frame carries the full authoritative `KeyState`.
3. **Enter/Ack handshake with input suppression until acknowledged** (lan-mouse's
   `WaitingForAck`) — drop input and re-send `Enter` rather than firing keystrokes into an
   unconfirmed link.
4. **Explicit exclusive send/receive states** make feedback loops structurally impossible.
5. **Keep-alive with a hard client-side deadline** — 3 s / 9 s is a proven pair, and the
   *client* must act on it.
6. **Mutual fingerprint pinning with out-of-band confirmation.** Deskflow (`PeerAuth`),
   lan-mouse (`authorized_keys`) and nikau (SSH-style approve-once) converged here
   independently. → seam's 6-digit SAS is the better UX for the same guarantee.
7. **Bound every length-prefixed field before allocating** — CVE-2021-42076.
8. **Race all candidate addresses, take the first to connect** — solves multi-homed hosts.
9. **25 years of switching UX defaults:** `switchDelay` (250 ms), `switchDoubleTap`,
   `switchCorners` + size, `lockCursorToScreen`, `relativeMouseMoves` for games, half-duplex
   lock-key flags. Apple's push-through affordance is the modern version of this.

## Lessons — rejected

1. 🔴 **Never let clipboard or file payloads share a channel with input.** The #1
   architectural mistake in the field, failing identically everywhere: barrier#775,
   deskflow#8198, PowerToys#49147. → seam: datagrams for input, a *separate* stream per
   clipboard transfer, so a failed transfer is contained.
2. 🔴 **Never send characters and re-derive keycodes.** The AltGr hack, the `ToUnicodeEx`
   pollution, and 161 comments on deskflow#4411 all descend from this one decision.
3. 🔴 **Never use 16-bit coordinates.**
4. 🔴 **Never accept a peer on the strength of a self-declared name** — CVE-2021-42073.
   Identity must be the pinned key, full stop.
5. 🔴 **Never ship encryption off by default or as a paid tier.** Barrier's auth fix shipped
   disabled; Synergy gates TLS behind an edition; the Barrier instance on this machine was
   running `--disable-crypto`.
6. **Don't build DPI/scaling as an afterthought** — cursor-trapped-at-edge and DPI are
   literally the same bug.
7. **Don't do encryption naively on the hot path** — "disable SSL to fix stuttering" is the
   confirmed workaround across all three C++ projects.
8. **Don't route modifier keys through key-repeat machinery** (lan-mouse#450).
9. **Don't fail silently at OS security boundaries** (inputleap#2143 — the password leak).
10. **Don't gate reconnection on a clean shutdown** (lan-mouse#307). Assume the link fails
    constantly; make reconnect the default state, and release all keys on every transition.
11. **Don't promise Wayland with a bounty and no plan.**

## Open opportunity: Barrier protocol compatibility

The Barrier/Deskflow protocol is fully documented above, and Deskflow negotiates down to
1.6. Speaking it behind a compatibility shim would make **every existing Barrier and
InputLeap install a seam client on day one** — estimated ~1500 lines. Worth considering as
an adoption lever once the native protocol is solid. Tracked, not scheduled.
