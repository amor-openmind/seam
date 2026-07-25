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
3. **First connection works the first time.** Reported directly by the user of this
   repo: getting two machines to talk for the first time in Barrier takes many
   attempts and a lot of trial and error. That is a design failure, not a user error —
   see §4a for the specific causes and §5 D8 for the fix.

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
- [ ] F14 The layout is a real 2-D edge graph (this fleet is L-shaped: iMac above amor,
      Mac-mini right of both) — not a left/right chain, and corners behave

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
| `Mac-mini` | 192.168.2.69 | macOS 26.5.2, arm64 (M4) | right | ✅ available |
| `AMorIMac-2` ("iMac") | 192.168.2.111 | **Windows** (ports 135/445 confirm it) | left, upper | ✅ reachable, 78 ms RTT |
| `om-amorabbi23-2` ("amor") | 192.168.2.193 | **Windows** | left, below the iMac | 💤 asleep at scan time |
| `linux.local` | 192.168.2.109 | Linux | — | 💤 asleep at scan time |

Note the layout is **not** a horizontal row: the iMac sits *above* amor, and Mac-mini is to
the right of both. Barrier's config models this as a per-edge graph and gets it wrong at
corners. seam must handle a genuine 2-D edge graph, not a left/right chain.

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
