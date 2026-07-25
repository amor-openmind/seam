# /goal — SEAM

> **seam** *(n.)* — the line where two screens meet, and the thing you should never notice crossing.

A software KVM: share one mouse, keyboard and clipboard (including files and folders)
across two or more machines on a local network. Inspired by Barrier / Synergy /
Input Leap / Deskflow / lan-mouse, written in Rust, aiming to be the most accurate,
lowest-latency and least-interrupting application of its kind.

---

## 1. Objective

Build a production-quality, cross-platform software KVM whose defining properties are
**correctness under layout/language differences** and **never interrupting the user**.

Three properties are the product. Everything else is table stakes:

1. **Layout/language correctness.** Typing across machines with different keyboard
   layouts and scripts (US / German / Persian / Cyrillic / dead keys / AltGr) produces
   the *intended* characters *and* keeps shortcuts working. This is the single most
   common complaint about every tool in this category.
2. **Never interrupt.** No stuck modifier keys. No cursor trapped on a dead machine.
   No silent disconnect. No crash. Recovery from sleep/wake, IP change and peer death
   is automatic and invisible.
3. **Near-zero configuration.** Reported directly by the user of this repo: getting two
   machines to talk for the first time in Barrier takes many attempts and a lot of trial
   and error. That is a design failure, not a user error — see §4a for the causes and
   §5 D8 for the fix.

   The standing rule, applied to every feature from here on:

   > **Every setting is a bug until proven otherwise.** If seam can detect it, seam
   > detects it. If it can be derived, it is derived. If a value must be chosen, seam
   > chooses a good default and the option only exists to override it. A setting is added
   > only when a *correct* automatic answer genuinely does not exist — never because it
   > was easier to ask the user than to work it out.

   This is not just onboarding polish. Configuration is where this software category
   fails: a screen name that must match a hostname exactly, a hand-edited layout file, an
   IP address, a port, a server/client role, a fingerprint dialog, a DPI-scaling
   workaround. **Each one is both an effort and an opportunity to get it wrong**, and
   several of them produce silent failures rather than error messages.

## 2. Scope

### In scope
| Area | Deliverable |
|---|---|
| Input | Mouse motion, buttons, high-resolution + discrete scroll, keyboard, modifiers |
| Layouts | Multi-layout / multi-language / multi-script typing with a per-peer policy |
| Clipboard | Text, HTML, RTF, images, **file and folder copy/paste** with real byte transfer |
| Topology | 2..N machines, arbitrary screen-edge graph (not just left/right) |
| Transport | Encrypted, authenticated, low-latency, self-healing |
| Discovery | Zero-config peer discovery + explicit pairing |
| Platforms | macOS, Windows, Linux (X11 + Wayland). ChromeOS — see §7 |
| Ops | Daemon + CLI, structured logs, metrics, latency instrumentation |

### Out of scope (this phase)
- Video/screen sharing (this is a KVM for input, not a remote desktop)
- Audio forwarding
- WAN / internet relay (LAN only; NAT traversal is a later phase)
- **GUI.** Per the machine-wide rules, any user-facing UI must be authored in
  Claude Design first and injected — it is deliberately excluded from this phase so
  no UI is ever hand-written in frontend code. Phase 1 ships daemon + CLI only.

## 3. Success criteria

### Functional
- [ ] F1 Cursor crosses a screen edge and control transfers, on an arbitrary N-screen layout
- [ ] F2 Keyboard input arrives with correct characters under *mismatched* layouts
- [ ] F3 Shortcuts (Cmd/Ctrl/Alt combos) keep working across mismatched layouts
- [ ] F4 Non-Latin scripts (Persian, Cyrillic, CJK-via-IME) type correctly
- [ ] F5 Dead keys / AltGr / compose produce correct composed characters
- [ ] F6 Clipboard text syncs both directions
- [ ] F7 Clipboard **files and folders** copy on machine A, paste on machine B, bytes intact
- [ ] F8 Discovery finds peers with zero configuration; pairing is explicit and authenticated

### Display topology (called out by the user as very important)
- [ ] F9 Each peer reports **every** display: pixel size, logical/point size, backing
      scale, refresh rate, position in that machine's virtual desktop, and which is primary
- [ ] F10 A peer's screen rect is **re-detected live** on monitor plug/unplug, resolution
      change, rotation, display sleep and fast user switching — never cached at startup
- [ ] F11 Edge crossing is correct with **mismatched resolutions and DPI**: leaving one
      screen at 43% of its height arrives at 43% of the neighbour's height, not at a raw
      pixel row that lands off-screen
- [ ] F12 Mixed-scale peers work (2560x1080 @1x here, HiDPI elsewhere) — coordinates are
      exchanged in a scale-independent form, never in raw device pixels
- [ ] F13 Multi-monitor peers expose their **true non-rectangular** desktop shape, so the
      pointer cannot be sent to a coordinate that exists on no physical display
- [ ] F14 The layout is a real 2-D edge graph — not a left/right chain. This fleet is
      L-shaped (iMac and Mac-mini side by side, the laptop below the iMac), so Mac-mini's left
      edge borders the iMac along part of its length and **nothing** along the rest.
      Partial edge adjacency, diagonal corners and dead zones must all behave.

### Onboarding (first-connection, elevated to a hard requirement)
- [ ] O1 **Two fresh machines connect on the first attempt**, with no config file edited,
      no IP typed, and no port chosen — measured as a timed, scripted first-run test
- [ ] O2 Time-to-first-connection **under 60 seconds**, including granting OS permissions
- [ ] O3 No machine has to be designated "server" or "client" — peers are symmetric
- [ ] O4 A peer's *name* is never load-bearing; identity is a `PeerId`, so a renamed or
      mistyped screen name can never cause a silent rejection
- [ ] O5 Every failure to connect produces a **specific, actionable** message naming the
      cause (not paired / version mismatch / permission missing / peer unreachable) —
      never a silent drop, and never a generic "connection failed"
- [ ] O6 `seam doctor` diagnoses a broken setup end to end and says what to fix

### Zero-configuration (standing requirement, applies to every feature)
- [ ] Z1 **A working two-machine setup requires zero config values from the user** — no
      IP, no port, no role, no screen name, no config file. The only input is confirming
      a 6-digit pairing code.
- [ ] Z2 **Everything detectable is detected**, never asked: screen count, resolution,
      DPI/scale, refresh rate, monitor arrangement, keyboard layout(s), OS, architecture,
      hostname, network interfaces. Re-detected on change, never cached at startup.
- [ ] Z3 **The screen layout is inferred, not drawn.** The first edge crossing a user
      performs teaches seam the arrangement; it is confirmed, not authored. A layout
      editor may exist to *correct* the inference — it is never required to reach a
      working state.
- [ ] Z4 **No setting may be required to make an advertised feature work.** Any feature
      that only functions after tuning is unfinished, not configurable.
- [ ] Z5 **Every remaining setting has a correct default and is documented with why it
      exists.** New settings need a written justification of why no automatic answer is
      possible. The count of required settings is tracked as a metric and must stay at 0.
- [ ] Z6 **Misconfiguration must be impossible or self-correcting**, not merely
      diagnosable. Preferred order: make the value automatic → make an invalid value
      unrepresentable → detect and self-heal → fail loudly with the fix. Silent
      degradation (Barrier's "unknown client", Windows UIPI's invisible `SendInput`
      failure) is never acceptable.
- [ ] Z7 **Installing seam is the whole install.** No separate driver, no daemon to
      register by hand, no firewall rule to add, no PATH edit, no elevation prompt on the
      happy path.

### Reliability (the hard requirements)
- [ ] R1 **Zero stuck modifiers** — verified by a fuzz test that kills the link mid-chord
- [ ] R2 **Zero trapped cursor** — killing any peer returns local control within 2 s
- [ ] R3 Survives sleep/wake and IP change without user action
- [ ] R4 Automatic reconnect with bounded backoff; no manual restart, ever
- [ ] R5 No panics in the daemon; `panic = "abort"` never fires in the soak test
- [ ] R6 24-hour soak test across 3 machines with zero interventions

### Performance
- [ ] P1 Added input latency (capture→inject, excluding network) **p99 < 1 ms**
- [ ] P2 End-to-end latency on wired gigabit **p99 < 5 ms**
- [ ] P3 Zero heap allocation in the motion hot path (verified by test)
- [ ] P4 Motion loss self-heals with **zero cumulative drift** (property test)
- [ ] P5 Idle CPU < 0.5 % per peer

### Engineering
- [ ] E1 Wire protocol is a written spec (`docs/PROTOCOL.md`), versioned and negotiated
- [ ] E2 Protocol codec has round-trip property tests + fuzz target
- [ ] E3 Core state machine is platform-independent and tested without any OS
- [ ] E4 CI builds and tests on macOS, Windows, Linux
- [ ] E5 `#![forbid(unsafe_code)]` everywhere except the platform backends, where every
      `unsafe` block carries a safety comment

## 4. Investigation (patterns to reuse, alternatives weighed)

Prior art studied: Barrier, Input Leap, Deskflow, Synergy 1/3, lan-mouse,
Mouse Without Borders, ShareMouse, Apple Universal Control, Logitech Flow.
Full findings in `docs/research/`.

**Direct local evidence gathered before writing any code:**
- `/Applications/Barrier.app` on this machine is crash-looping — 8 crash reports on
  2026-07-25 alone. Faulting frame:
  `ClientListener::handleUnknownClient` → `EventQueue::dispatchEvent` → `SIGSEGV`
  (`EXC_BAD_ACCESS` on a wild pointer). A use-after-free of the `void*` user-data
  carried through Barrier's hand-rolled event queue.
  → **Lesson:** no raw pointers across an event queue. Rust ownership removes this
    entire bug class; it is a large part of why this project is in Rust.
- That binary is **x86-64 running under Rosetta** on an Apple Silicon M4
  (`"translated": true`, `modelCode: Mac16,11`). Emulated code in the input hot path.
  → **Lesson:** ship native binaries for every target arch.
- Measured RTT to the Windows peer on this "local network" is **78 ms average** —
  Wi-Fi power-save, not distance.
  → **Lesson:** latency work must include countering NIC power-save, not just wire
    format. A protocol tuned for 1 ms is pointless behind a 78 ms radio nap.
- The running instance was invoked with `--disable-crypto
  --disable-client-cert-checking`, i.e. keystrokes in plaintext on the LAN.
  → **Lesson:** if the secure path is hard enough to set up that users disable it, the
    security design has failed. seam has no unencrypted mode to fall back to.

### 4a. Why the first connection is so painful today

The user's report — *"first time connection between machines takes a long time with many
trials and errors"* — matches the observed configuration exactly. Each cause below has a
corresponding fix:

| Cause in Barrier/Synergy | Fix in seam |
|---|---|
| You must decide which machine is *server* and which is *client*, and configure them differently | Peers are **symmetric**; there is no role to choose (D8) |
| The client must be told the server's **IP address** by hand | mDNS discovery — peers find each other (D8) |
| A screen name in `barrier.conf` must **exactly match** the client's `--name`. Mismatch → the client is rejected as "unknown"… which is the very code path (`handleUnknownClient`) that segfaults on this machine | Identity is a random `PeerId`. Names are cosmetic and **cannot** cause a rejection (O4) |
| Failures are silent or generic — you retry blindly | Every rejection carries a specific reason in `HelloAck.message` (O5) |
| The TLS fingerprint prompt is easy to miss, so people pass `--disable-crypto` instead | Pairing is a deliberate, visible step with a short code; there is no insecure mode (D8) |
| Config is a hand-edited text file with its own syntax | Layout is set through the CLI; no file to hand-edit for first connection |

## 5. Key design decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | **Cumulative-position motion, not deltas** | A lost delta = permanent drift forever. Sending a cumulative accumulator makes packet loss self-healing: the next packet fully corrects. Lets motion ride unreliable datagrams safely. |
| D2 | **QUIC (quinn): datagrams for motion, reliable streams for state** | One TCP connection head-of-line-blocks pointer motion behind a clipboard transfer — a known Synergy/Barrier stutter. QUIC gives independent streams, TLS 1.3, and connection migration for sleep/wake and IP change, for free. |
| D3 | **Key events carry physical *and* logical identity** | `physical` = USB HID usage code (layout-independent), `logical` = the Unicode the source layout actually produced. Sending both is what makes mismatched layouts solvable at all. |
| D4 | **`Auto` layout policy** | Modifier-bearing chords → replay *physical* key (Cmd+C stays Cmd+C). Plain text entry → replay *logical* character (you get the glyph you typed). This is the core differentiator; no competitor does it. |
| D5 | **Authoritative key-state + digest heartbeat** | Receiver continuously reconciles its held-key set against the sender's. Makes "stuck modifier" structurally impossible rather than best-effort. |
| D6 | **Receiver-side dead-man watchdog** | The receiver releases capture on silence, without being told. Never trusts a possibly-dead peer to free the cursor. |
| D7 | **Hand-rolled wire codec** | The message set is small and fixed. Owning the bytes gives an exact spec, zero-alloc hot path, no serde churn, and a fuzzable surface. |
| D8 | **Symmetric peers + mDNS discovery + short-code pairing** | Removes every manual step that makes the first connection fail: no role to pick, no IP to type, no name to match, no config file. Pairing is one visible code confirmation, which is also what lets encryption be mandatory instead of the thing users switch off. |
| D9 | **The shared desktop is arranged in physical units (mm), not pixels** | This is the root fix for the whole DPI/resolution bug family (Deskflow #1025, #1918, #741, #94). A 27" 4K and a 13" laptop line up the way the user's hand expects, and the crossing point is carried as a physical offset along the edge rather than a fraction — fractions are exactly why cursors jump between different-resolution screens. Each machine is a **union of rectangles**, so non-rectangular multi-monitor desktops work and the pointer can never be sent to a coordinate that exists on no display. |
| D10 | **Capture fails *open*, never closed** | On macOS, an active head-inserted suppressing `CGEventTap` whose Accessibility permission is revoked mid-session can **freeze all local input, recoverable only by a hard reboot** (deskflow #9562), and a leaked tap can wedge WindowServer on Tahoe. Every error path in the capture layer must pass events through rather than swallow them, permission health must be polled (`AXIsProcessTrusted` is unreliable — probe with a throwaway `CGEventTapCreate`), and a watchdog must tear the tap down if the loop stalls. **Bricking the user's session is a worse failure than not forwarding input.** |
| D11 | **Pairing SAS is bound to the TLS exporter (RFC 5705), not a PAKE** | 6 digits from `export_keying_material(b"seam-pair-v1")`, compared on both screens. MITM resistance is structural: an attacker terminates two different TLS sessions, so the two codes visibly differ. Avoids depending on the `spake2` crate, which states it has never been independently audited. |

## 6. Verification plan

| Level | What | How |
|---|---|---|
| Unit | Codec round-trip, state machine, edge crossing | `cargo test`, `proptest` |
| Fuzz | Codec decode on arbitrary bytes | `cargo-fuzz` |
| Property | Motion drift under simulated loss/reorder | `proptest` over a lossy virtual link |
| Loopback | Full daemon↔daemon over real QUIC on one host | integration test |
| E2E | 3 real machines on this LAN | see §7 |
| Soak | 24 h, 3 machines, injected faults | harness + log assertions |

## 7. E2E test fleet

| Host | Address | Platform | Physical position | Status |
|---|---|---|---|---|
| `Mac-mini` | 192.0.2.10 | macOS 26.5.2, arm64 (M4), LG 2560x1080 | right | ✅ up |
| `windows-desktop` ("iMac") | 192.0.2.11 | **Windows** (135/139/445/5040/7680 open) | left, **same level as Mac-mini** | ✅ up |
| `windows-laptop` (laptop) | 192.0.2.12 | **Windows**, Dell laptop (7680 open) | below the iMac | ✅ up |
| `linux.local` | 192.0.2.13 | Linux | — | ✅ present (live ARP) |

```
 [ iMac / Windows ]  [ Mac-mini / macOS ]
 [ the laptop  / Windows ]
```

The layout is a genuine **2-D edge graph, not a row**. Note the corner: the laptop's right edge
and Mac-mini's bottom-left corner touch only diagonally, so part of Mac-mini's left edge
borders the iMac and part borders nothing at all. Barrier models this as per-edge links
and gets corners wrong; seam must handle partial edge adjacency and dead zones (F13/F14).

### Correction — ICMP is not a liveness test

An earlier pass reported `.193` and `.109` as "asleep" because they did not answer ping.
Both were in fact powered on and in use: **Windows Firewall drops ICMP echo by default.**
The reliable signals were a live ARP entry, an open TCP port, and mDNS resolution — all
three confirmed every machine.

This is a bug seam must not ship: **discovery and liveness must never depend on ICMP.**
It also validates mDNS-first discovery (D8), which resolved all three names correctly.

Note the two Windows machines have *different* firewall profiles — `.111` exposes SMB/RPC,
`.193` exposes only 7680. seam must therefore work without any inbound port being opened
by hand (Z7), and must not assume a peer is unreachable because a probe was refused.

**E2E must work with no SSH and no credentials** — confirmed as a constraint by the user.
That rules out driving the Windows peers from a remote shell, and it is the right
constraint: it forces verification to happen over seam's own authenticated link rather
than through a side channel that real users would never have.

- **V1** The daemon exposes its own diagnostics *in-protocol* — screen geometry, held-key
  state, permission status, latency histograms, reconnect counts — retrievable from any
  paired peer. `seam doctor` reads them for **every** peer, not just the local one.
- **V2** E2E assertions run against those in-protocol reports, so a three-machine test is
  driven entirely from this Mac-mini with no shell on the other two.
- **V3** The only manual step on a Windows peer is installing and launching seam once.
  Anything beyond that is an onboarding failure (criteria O1–O6).

This also means the diagnostics have to be good enough to debug a failure remotely, which
is exactly the property missing from every tool surveyed: Barrier's failures are silent
(`barrier#1559`), and its "unknown client" rejection is the code path that segfaults.

### Live incident — the trapped-cursor failure, observed on 2026-07-25

While stopping Barrier during this session, its server process was killed while pointer
focus was on a remote screen. Result: **the pointer was stranded and the local touchpad
became unusable.** On the macOS peer the cause was verified precisely — Barrier had left
the OS in `CGAssociateMouseAndMouseCursorPosition(false)` (mouse hardware decoupled from
the on-screen cursor) and did not restore it on death. Recovery required manually calling
`CGAssociateMouseAndMouseCursorPosition(true)`, draining the `CGDisplayShowCursor`
refcount, and warping the pointer back.

This is criterion **R2** happening in production, and it makes the requirement concrete:

- **R2.1** Capture-side state (cursor association, cursor visibility refcount, pointer
  confinement, key grabs) must be restored on **every** exit path — including `SIGKILL`,
  panic and power loss. Since `SIGKILL` cannot be handled in-process, restoration cannot
  live only in a cleanup handler: a **separate supervisor process** must observe the
  daemon's death and restore OS state, and the daemon must re-assert sane state on start.
- **R2.2** The receiving peer must release on silence via its own watchdog, never waiting
  to be told (already stated in D6).
- **R2.3** A **panic hotkey** must unconditionally return input to the local machine, and
  must work when the pointer is already stranded.

**ChromeOS — stated honestly up front.** ChromeOS does not expose global input
capture or injection to any app sandbox. Full peer parity is *not* achievable there.
The achievable target is a degraded Crostini/Linux-container peer (receive input into
the container's Wayland surface + clipboard sync). This will be confirmed by the
platform research and documented as a hard limit rather than quietly dropped.

## 8. Definition of done

Phase 1 is done when: F1–F8, R1–R6, P1–P5 and E1–E5 are demonstrated with committed
evidence (test output, latency histograms, soak logs) across **three real machines**,
and the limits above are documented rather than hidden.

## 9. Assumptions

Stated because they change the work if wrong:
1. "No firewall, antivirus, network lag, interruption" is read as *the environment is
   clean, so there are no excuses* — not as *skip security*. Traffic is still
   encrypted and authenticated by default; there is no unauthenticated mode.
2. macOS peers require Accessibility + Input Monitoring permission. This is an OS
   requirement no implementation can avoid; it will be detected and reported clearly.
3. The project name `seam` is provisional and cheap to change.

---

## 10. Next: the input path (decided, not yet built)

Everything up to the link is done and proven on three machines. What remains is the part
that actually moves the pointer. The design below is settled so implementation can start
immediately.

### Step 1 — mirror mode, and why it comes first

**macOS capture uses `kCGEventTapOptionListenOnly` initially.** A listen-only tap's return
value is ignored by the OS, so it *physically cannot* suppress input — which means it
cannot produce the failure that makes this work dangerous (deskflow#9562: an active
suppressing tap whose permission is revoked mid-session freezes all local input until a
hard reboot).

The trade-off is that the local pointer keeps moving too. That is not the final UX, but it
is honest, visible, end-to-end input forwarding, and it de-risks everything underneath:
event tap plumbing, tap-disable recovery, the motion encoding, the wire path, and Windows
injection all get proven before any code is allowed to suppress anything.

| Piece | Detail |
|---|---|
| macOS capture | `CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionListenOnly, …)` on a dedicated thread with its own `CFRunLoop` in `kCFRunLoopCommonModes` |
| Tap health | Handle `kCGEventTapDisabledByTimeout` **first and unconditionally**, re-arm with `CGEventTapEnable`, and poll `CGEventTapIsEnabled` every ~5 s. This is the bug behind "it works again when I open the app window" |
| Hot path | Callback does no allocation, no I/O, no locks: it writes a fixed-size event into an SPSC ring and returns |
| Wire | `Frame::Motion` on a QUIC datagram, already implemented and tested |
| Windows injection | `SendInput` with `MOUSEEVENTF_ABSOLUTE \| MOUSEEVENTF_VIRTUALDESK`, coordinates normalised 0–65535 across the **virtual desktop**, never the primary monitor |

### Step 2 — real crossing

Only once step 1 works end to end: switch to an active tap, suppress locally, and add
`CGAssociateMouseAndMouseCursorPosition(false)` via the existing `CursorGuard`. Edge
detection uses the geometry already implemented in `seam-input::screen`.

**Preconditions before any suppressing tap ships:** a supervisor process that restores OS
cursor state if the daemon dies (R2.1, because `SIGKILL` cannot be handled in-process), a
receiver-side dead-man watchdog (D6), and a local release chord evaluated in the capture
layer before any network involvement.

### Step 3 — keyboard

`CGEventKeyboardSetUnicodeString` plus a `UCKeyTranslate` reverse map on the receiver, so
the `LayoutPolicy::Auto` logic already implemented in `seam-proto::keys` is finally
exercised against the real German Apple keyboard on this fleet.

### Known limits to fix, not hide

- **Z7 is currently violated on Windows**: inbound mDNS needs a firewall rule, so discovery
  fails silently and only `seam pair --at` works. The fix is an installer that registers
  the rule; until then `doctor` should *detect* and report it rather than leaving the user
  to guess.
- `seam peers` shows a peer's id where its name should be.

---

### Resolved: elevated windows no longer kill injection (v0.2.2)

Focusing an elevated window on Windows — an admin PowerShell, Task Manager — made the
shared pointer and keyboard freeze there: UIPI silently discards injected input from a
lower-integrity process, with no error anywhere. seam now detects a non-elevated start
and relaunches itself once through UAC (`--no-elevate` opts out and the elevated copy
passes it to itself so it cannot loop); `doctor` states the elevation and its exact
consequence. Declining the prompt logs precisely what will freeze and when. Out of scope
and said so: the secure desktop (the UAC prompt itself, Ctrl+Alt+Del) needs a signed
UIAccess binary and stays local.

This also retires the reliability goal's weakest point: it is now written down as R7 —
**injection must survive elevated-window focus**.

## 11. Known gaps, precisely stated

Recorded so the next session starts from evidence rather than rediscovery.

### Media and volume keys are not forwarded

Volume, brightness, play/pause and the rest are **not keyboard events** on macOS. They
arrive as `NX_SYSDEFINED` (`CGEventType` 14), and which key it was lives in the event's
`data1` field rather than in a keycode. seam's tap masks only key-down, key-up and
flags-changed, so these never reach the wire — and the Mac keeps acting on them, which is
why they work locally while the pointer is on another machine.

**What it needs**
1. Add `NX_SYSDEFINED` to the event mask, and suppress it like any other key while a peer
   holds focus.
2. Decode it. `CGEvent` exposes no accessor for `data1`; the practical route is
   `NSEvent.eventWithCGEvent:` and then `subtype` (8 = `NX_SUBTYPE_AUX_CONTROL_BUTTONS`),
   `data1 >> 16` for the key, and `(data1 & 0xFF00) >> 8` for the direction. That means an
   AppKit dependency — `objc2-app-kit` — in the macOS backend.
3. Carry it. USB HID puts these on the **Consumer** page (`0x0C`), not the Keyboard page
   (`0x07`), so `PhysicalKey` needs a page as well as a usage, or a separate frame.
4. Inject on Windows: `VK_VOLUME_UP` `0xAF`, `VK_VOLUME_DOWN` `0xAE`, `VK_VOLUME_MUTE`
   `0xAD`, `VK_MEDIA_PLAY_PAUSE` `0xB3`, `VK_MEDIA_NEXT_TRACK` `0xB0`,
   `VK_MEDIA_PREV_TRACK` `0xB1`.

**Honest caveat from the research**: these keys can be *observed*, but the local machine
often cannot be stopped from acting on them as well — brightness in particular has
lower-level handling. Expect volume to forward cleanly and brightness to remain local.

### Clipboard: images and files

Text works in all directions. Images are a contained addition (`arboard` handles them; one
more frame type). Files are not: no Rust crate implements the Windows file promise
(`CFSTR_FILEDESCRIPTORW` + `CFSTR_FILECONTENTS` over a custom `IDataObject`), and macOS has
no working clipboard file promise at all, so it needs a virtual filesystem — NFS loopback
rather than macFUSE, to avoid a kext. See `docs/research/clipboard.md`.

### The local cursor still tracks the mouse — needs a foreground agent

**Status: understood, not fixed. This is architectural, not a bug in the logic.**

Suppression is enabled correctly: `set_suppress_local(true)` runs on every remote
handover and is cleared only when focus returns local. `CursorGuard::detach` is called,
and `CGAssociateMouseAndMouseCursorPosition(0)` returns success. Six separate attempts
to fix this by changing the suppression logic failed because the logic was never wrong.

The mistake was reading that success return as proof of effect. It is not. On macOS,
cursor visibility and cursor-to-mouse coupling belong to the **foreground application**.
A background daemon may call `CGAssociateMouseAndMouseCursorPosition` and
`CGDisplayHideCursor`, receive success from both, and have neither take effect.

This is why Barrier and deskflow ship a GUI application on macOS rather than a headless
daemon. It is not a stylistic choice: owning the cursor requires foreground status.

**Read Barrier before touching this again.** `OSXScreen.mm` uses two APIs seam does not:

```cpp
CGSetLocalEventsSuppressionInterval(0.0);              // setZeroSuppressionInterval()
CGSetLocalEventsFilterDuringSupressionState(           // avoidSupression()
    kCGEventFilterMaskPermitAllEvents, kCGEventSupressionStateSupressionInterval);
CGSetLocalEventsFilterDuringSupressionState(
    kCGEventFilterMaskPermitLocalKeyboardEvents | kCGEventFilterMaskPermitSystemDefinedEvents,
    kCGEventSupressionStateRemoteMouseDrag);
```

After any `CGWarpMouseCursorPosition`, macOS suppresses local hardware events for about
0.25 s by default. Barrier zeroes that interval on entering the primary screen and permits
all events during the suppression state. seam warps on every return-home and sets neither,
so for a quarter second after each crossing macOS is filtering local input in a way seam
does not account for. Barrier calls `setZeroSuppressionInterval()` from `enter()` on the
primary screen and `avoidSupression()` from `enter()` on a secondary one.

Barrier's `hideCursor()` is otherwise the same pair seam already uses
(`CGDisplayHideCursor` + `CGAssociateMouseAndMouseCursorPosition(false)`), so the
difference is the suppression state, not the cursor call. Try this **before** building an
agent — it is three lines against a new crate.

**If that is not enough**: a minimal Cocoa agent — `NSApplicationActivationPolicyAccessory`, no dock
icon, no window — that holds the cursor state while a peer has the pointer. The daemon
keeps the transport, the graph and the tap; the agent owns only the cursor. Do **not**
attempt another workaround from the daemon: two have already failed and one locked the
machine (see the removed `park_cursor` note in `crates/seam-cli/src/main.rs`).

### Cosmetic: the cursor position at the edge

With the tap at the HID location the cursor should stop moving. It still sits where it was
left. Hiding it needs foreground status, which a daemon does not have — two attempts to
work around that failed and one locked the machine, so the real fix is a UI agent rather
than another cursor warp.

---

## 12c. Settings, startup, and the role question — answered honestly

**Settings now persist** (v0.3.7) in a `settings` file beside the identity, so a portable
install carries them: which machines are switched off, and which clipboard kinds this
machine shares. Nothing detectable is stored there — geometry, layout, speed and identity
stay detected (goal Z2). Disabling a peer stops input to it immediately, mid-session,
without unpairing.

**Start at login** is per-user and needs no installer or admin rights: a `LaunchAgent`
plist on macOS, an `HKCU\...\Run` value on Windows. `RunAtLoad` only — deliberately no
`KeepAlive`, because a crash loop that relaunches itself forever is worse than seam being
off, and the input watchdog already covers the failure that matters.

**"Make this machine the server" cannot be built yet, and the reason is not a setting.**
seam captures input only on macOS; the Windows backend replays and never captures
(`this machine receives input only; capture is not built for it yet`). So a Windows
machine cannot share its keyboard and mouse regardless of any toggle, and adding one
would be a control that lies. What is missing is **Windows input capture** — a low-level
keyboard/mouse hook plus its own suppression story, comparable in size to the whole macOS
capture path. Until that exists, a machine's role is reported as observed capability
(`shares input` / `receives input`), never as a choice.

**Claude Design surfaces: all authored** as of v0.3.8. Nine pages, every one served by
the binary. What remains is not design but *binding*: onboarding, doctor, update,
settings, pairing, chrome and notifications still show their authored states rather than
live daemon data. The fleet page is the only one bound so far. Binding each is
mechanical — a `_ds/bind.js` handler per page against endpoints that mostly exist —
and must be done without touching the visible structure.

## 12d. No mock data in the product — a rule, not a preference

Design pages carry sample content: example machine names, plausible timestamps, a folder
called `photos-june`. That is correct in a design tool and **a lie in a running
application**. seam shipped it for several versions — a doctor page listing failures the
machine did not have, an activity feed of events that never happened.

**The rule**: any repeating or data-driven element in a design is a TEMPLATE. The binding
clones it and fills it from real state, or the page shows its empty state. Sample content
must never survive to a running screen.

**How to check before shipping a page**: grep the served page for the sample values. If a
machine name, timestamp or filename from the design appears in what the daemon serves,
the page is not bound and must not claim to be.

Bound to real state as of v0.4.0: fleet (machines, focus, health, transfers, sharing,
start-at-login, activity from the real log), doctor (live checks, verdict counted from
them), transfers (what is actually moving, honest empty state), settings (persisted
values). Still carrying authored sample states, and therefore still to bind: pairing,
chrome, notifications, onboarding, update.

## 12e. Why clients cannot see each other — the star, stated plainly

Observed on the real fleet: the Mac mini lists both Windows machines; each Windows machine
lists only the Mac mini. That is not a bug in the UI — it is the topology, and it was never
made explicit.

**Pairing is pairwise.** The iMac and the laptop were each paired with the Mac mini and
never with each other, so they have no reason to trust or dial one another. Auto-discovery
only dials machines this machine already trusts, deliberately: dialling strangers found on
a network is how a KVM becomes an attack surface.

So the fleet is a **star**, and each leaf genuinely knows only the centre. Two consequences
the UI currently gets wrong:

1. A leaf cannot show the fleet, because nobody tells it. **Fix**: the centre should send
   its peer list, so leaves can *display* the fleet without connecting to it. This needs a
   frame (kind `0x03`, reserved) carrying peer id, name, edge and capability. Display only
   — never a basis for trust.
2. Status changes are not reactive on a leaf for the same reason: it learns only what
   arrives on its own link. The same frame, pushed on change, fixes both.

**The alternative — a full mesh — is a security decision, not a convenience.** It would
mean either pairing every pair by hand (n² SAS confirmations) or transitive trust ("the
Mac mini vouches for the laptop"), which quietly turns one compromised machine into a
compromised fleet. The star with pushed fleet state gives the right display without that
trade.

## 12f. Can clients be a web page instead of a binary? No — and what can

Asked directly, so answered directly: **a browser cannot inject mouse or keyboard events
into the operating system.** That sandbox is the reason browsing is safe; if a page could
move the cursor, every site could. Any machine that *receives* input must run native code.
No amount of work changes this.

What can travel over a URL is the **control surface**, which is most of what a person
opens seam to look at. Recorded as the next design-led feature:

- Serve the fleet page on the network, not only loopback, so any machine — including a
  phone — can watch the desk, switch machines off, release the pointer and quit, with
  nothing installed.
- Authenticate it with the six-digit confirmation seam already uses for pairing, shown on
  the host and entered once per device. A password would be the configuration this project
  has spent its life avoiding.
- Default to read-only. Viewing which machine holds the pointer and quitting the daemon
  are different permissions, and the page already knows which actions are which.

The full idea list, with cost and with the reasoning for the two ideas that are declined
(a full mesh, and reaching the Windows secure desktop), is authored in Claude Design as
`ideas.html` and served by the binary at `/ideas.html`.

## 12b. Review discipline — added after a self-inflicted vulnerability

A line-by-line review of the newest code found that the fleet-page server validated
nothing about who was calling it. Any website the user visited could
`fetch('http://127.0.0.1:PORT/action/quit', {method:'POST', mode:'no-cors'})` — a simple
POST needs no preflight, so it lands — and kill the daemon, disable peers or release
input; DNS rebinding could also read `/state` (machine names, ids, LAN addresses).
Fixed in v0.3.5 with Origin and Host validation, four tests pinning the attack, and a
live proof that a hostile POST is refused while the daemon keeps running.

**The lesson, recorded so it becomes a rule rather than an anecdote**: seam's threat
model was written entirely around the *network* — encrypted QUIC, certificate identity,
SAS pairing — and the moment a local HTTP surface appeared, none of that reasoning
applied to it. Every new surface gets its own threat model, not the project's.

**Standing review checklist for any new surface**
1. Who can reach it? (Loopback is not a permission — browsers reach loopback.)
2. What proves the caller is who it claims? (Origin/Host, tokens, OS credentials.)
3. What does it leak to a caller that cannot read the response? (Timing, side effects.)
4. What happens if the process dies mid-operation? (Concealed cursors, held input.)
5. Is the failure visible in the log, or silent?

## 12a. Design governance — permanent rule for every feature and config

**Claude Design is the source of truth for everything visible, for the life of this
project.** Not a phase — a standing rule:

1. **Every** feature, config surface, dialog, notification, status, empty state, error
   state and OS chrome is authored in Claude Design first — "Seam Pages"
   (e3000f2f-209b-434d-b7a8-4ca8813e66d0), consuming the "Seams Design System"
   (22ae8beb-c703-4b1c-aa6d-ae63e84ef135). New reusable pieces go into the DS itself.
2. **Ask Claude Design for ideas, new designs and redesigns** — author explorations and
   alternatives in the design project and pick there, not in code. When a surface feels
   wrong, it is redesigned in Claude Design and re-pulled; it is never patched in code.
3. The frontend **renders the produced structure 1:1**. Implementation work is binding:
   real daemon state, real events, real actions wrapped around the design's elements.
   Hand-editing the visible part of the frontend — any element, style, spacing or copy —
   is a violation, including "small fixes" and "temporary" states.
4. A design/business conflict is resolved by fixing the DESIGN first (a template must
   never override a real constraint like the zero-config principle or the pairing trust
   model), then re-injecting.

**Surface inventory** (each needs design coverage before its frontend exists —
authored ✓ / to ask Claude Design for ✗):

| Surface | State |
|---|---|
| Fleet dashboard (desk, peers, health, activity) | ✓ index.html |
| Transfers (streaming, resume, history, refusals) | ✓ transfers.html |
| Pairing flow incl. SAS verify + refusal | ✓ pairing.html |
| Settings (sharing, behaviour, notifications, diagnostics, danger) | ✓ settings.html |
| Onboarding + macOS permission walkthrough (the per-binary re-grant pain) | ✓ onboarding.html |
| Menu-bar dropdown (macOS) / tray popover (Windows) — the everyday chrome | ✓ chrome.html |
| Toasts/notifications (peer joined/dropped, transfer done/failed, update) | ✓ notifications.html |
| Doctor report as a full page (what's wrong and the exact fix) | ✓ doctor.html |
| Degraded states: tap disabled, UIPI elevated-window warning, mirrors-not-switching | ✓ in chrome.html + notifications.html |
| Empty states: no peers yet, nothing transferred yet | ✓ in index.html + transfers.html |
| Update flow (new version available → restart → re-grant reminder) | ✓ update.html |
| Compact/mobile-width variants of all of the above | ✓ responsive in each page (media queries), not separate files |

## 12. /goal — design-driven frontend + streaming clipboard (in progress)

**Objective**: a real user-facing frontend on every OS — status, pairing, layout,
notifications, settings — with every visible element authored in Claude Design and never
hand-touched; and chunked, resumable streaming for clipboard payloads over the 64 MB cap.

**Design source of truth**
- Design System: "Seams Design System" (claude.ai/design/p/22ae8beb-c703-4b1c-aa6d-ae63e84ef135)
  — already populated: warm paper/ink/indigo tokens, Archivo + IBM Plex Mono + Instrument
  Serif, core components (Button, Dialog, Toast, Badge, Table, Tabs, forms), brand
  guidelines. Its status vocabulary maps onto KVM state: approved→connected,
  in-production→holding pointer, at-risk→degraded, late→offline.
- Pages: "Seam Pages" (claude.ai/design/p/e3000f2f-209b-434d-b7a8-4ca8813e66d0).
  Authored so far, render-verified:
  - `index.html` — fleet dashboard: identity, the real desk topology (iMac left,
    laptop below iMac, live pointer focus), peers with status pills, permission
    health (the doctor as UI, including the mirrors-instead-of-switching warning),
    activity feed from the real log lines.
  - `transfers.html` — clipboard transfers: chunked progress, pause/resume,
    resumed-after-reconnect state, refused-over-cap state, history table.
  - `_ds/tokens.css` — verbatim mirror of the DS tokens (DS remains the source; edit
    there, re-mirror here).
  - `pairing.html` — the pairing flow as states: discovering (mDNS, zero-config),
    machines found, the SAS six-digit verify dialog (the moment that matters, with the
    mismatch-means-interception explanation), paired, and numbers-differ refusal.
  - `settings.html` — the few settings a person could genuinely want: identity +
    fingerprint, sharing toggles and the 64 MB direct-transfer limit, start-at-login,
    menu-bar status, panic release shortcut, notification choices, log level/doctor,
    and the danger zone (forget all, reset identity) — prefaced by the zero-config
    statement that layout/speed/geometry are detected, not configured.
  Still to author: notifications/toast surfaces, per-OS chrome variants, mobile layouts.

**Streaming clipboard design (frame kinds 0x60–0x6F, reserved since v0.1.0)**
1. `TransferOffer {id, generation, kind: files|image, manifest: [(path, size, sha256)]}` —
   sent instead of ClipboardFiles/Image when total > 64 MB.
2. Receiver pastes a *promise* immediately (Windows: IDataObject with
   CFSTR_FILEDESCRIPTORW/CFSTR_FILECONTENTS; macOS: spool-on-demand), pulls with
   `TransferPull {id, index}` — lazy: chunks fetched only when the paste target reads.
3. `TransferChunk {id, index, bytes}` — 1 MB chunks over one reliable QUIC stream per
   transfer; per-chunk sha-256; receiver acks high-water mark.
4. Resume: on reconnect the receiver re-sends its high-water mark; sender continues from
   there. Chunks already landed are never resent.
5. Cancel/timeout: either side sends `TransferAbort {id, reason}`; spool partials swept.

**Clipboard feature status, precisely**
- Images: SHIPPED v0.2.0. Verified by a live round-trip through the real macOS
  pasteboard and codec fuzz tests. Cross-machine paste awaits the user's test.
- Files/folders ≤ 64 MB: SHIPPED v0.2.1. Same verification level; Windows side
  compiles and speaks CF_HDROP but has not been exercised end-to-end.
- Over 64 MB: NOT DONE. Streaming/resumable is fully specified below and is the next
  implementation item. Until then, over-cap copies are refused with a log line.

**Honest phase status (per the completion contract)**
1. Design authored + render-verified: PARTIAL (6 page groups; outstanding: onboarding/permissions walkthrough, doctor page, empty states, update flow, compact variants).
2. Sync/mapping into the app: NOT STARTED.
3. Frontend shells: V1 SHIPPED (v0.3.0) — the daemon serves the design mirror on
   loopback and `seam ui` opens it; the artifacts are embedded verbatim from the
   Claude Design pull (crates/seam-cli/ui is a mirror, never hand-edited). Native
   menu-bar/tray wrappers around the same pages: NOT STARTED.
4. Real data binding: STARTED (v0.3.0) — the fleet page binds live /state (identity,
   version, focus holder, peers, permission health) via _ds/bind.js, authored in
   Claude Design. Other pages still show authored sample data; activity feed,
   transfers and settings are not yet bound.
5. Tests/E2E for the frontend: NOT STARTED.
6. Streaming protocol: DESIGNED above, NOT IMPLEMENTED — v0.2.1 ships the 64 MB
   single-frame cap.

---

## 13. Licensing and distribution (v0.5.0)

**Licensing.** A licence is a token signed with an Ed25519 private key that only the owner
holds. Builds carry the matching **public** key, so a binary can verify a licence and can
never mint one. There is no licence server and nothing is transmitted: a licence works on a
machine that has never been online. `seam activate <licence>` verifies and stores it beside
the identity; the daemon refuses to bind, capture or connect without one; `doctor` reports
it.

Issuing is `scripts/issue-licence.sh` — owner only, needs `licence-private.pem`, which must
never be committed or copied to a machine that is not the owner's. Releases are built with
`SEAM_LICENCE_KEY=$(cat licence-public.hex)`. A build without that variable refuses every
licence, which is the safe direction to fail.

**Stated honestly**: this does not make the software uncopyable. Anyone who can rebuild or
patch the binary can remove the check — true of every client-side licence ever shipped.
What it does is make running seam require something only the owner can issue.

**Private source with public downloads needs two repositories.** A private repo's release
assets require a GitHub token to download, so "private code, public releases" cannot be one
repo:

- `seam` (private) — all source, history and issues.
- `seam-releases` (public) — no source. Releases only: binaries, `SHA256SUMS.txt`,
  `join.sh`, release notes. Publishing becomes: build with the owner key, then
  `gh release create --repo <owner>/seam-releases`.

The join script and the update flow point at the public repo, so nothing else changes.
Note the current repository is public and its history contains the full source; making it
private later does not retract what has already been fetched or forked.
