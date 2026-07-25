# Transport, discovery and pairing — 2026 findings

Research pass completed 2026-07-25. quinn defaults verified by reading `quinn-proto`
source, not docs. Crate versions live from crates.io the same day.

## Verdict

**QUIC (quinn) with unreliable datagrams for motion + one reliable stream for state.**
Not for handshake speed, and not primarily for head-of-line blocking — the two decisive
reasons are **connection migration** and **getting a correct reliable channel adjacent to
the unreliable one over a single socket, port, firewall rule and identity**.

## What the prior art actually does

| Project | Transport | Reliability for input |
|---|---|---|
| **lan-mouse** v0.11.0 (2026-06) | **DTLS 1.2 over UDP**, port 4242 | none for input; app-level Ack for Enter/Leave only |
| **Deskflow** v1.26.0 | TCP 24800, `TCP_NODELAY` | TCP for everything |
| **input-leap** v3.0.3 | same TCP protocol | TCP |
| **rkvm** 0.6.1 (2024-07) | TCP + rustls | **dead**, Linux-only |
| **Logitech Flow** | UDP discovery + TCP 59869 | TCP |

> Correction to a common assumption: **lan-mouse is no longer raw UDP.** v0.11.0 moved to
> DTLS 1.2 (`webrtc-dtls 0.12`).

**Both existing designs are broken in complementary ways, and this is the core insight:**

- lan-mouse sends **relative deltas** with no sequence number and no retransmission → a
  dropped packet is **permanent, uncorrectable drift**. Its own source carries a comment
  explaining that it must synthesize key-ups before `Leave` because *"Leave can be lost
  over UDP/DTLS"*, otherwise the peer runs every later keystroke through phantom
  modifiers until a 1 s watchdog fires.
- Deskflow sends **absolute pixel coordinates** over TCP → state transitions are reliable,
  but motion is stuck behind the same head-of-line-blocked pipe, and pixel coordinates
  cause its long-standing DPI bug family (issues #1025, #1918, #741, #94).

**Each got exactly half the problem right. seam's design is the synthesis, not a novelty.**

## Numbers that settle the argument

**Congestion control does not bite at 1000 Hz — by ~800×.**
One 32-byte event ≈ 60 B UDP payload. 1000 events/s ≈ 60 KB/s ≈ 816 kbit/s. Bytes in
flight at 0.3 ms LAN RTT ≈ **18 bytes**, against quinn's Cubic initial window of
**14,720 bytes** (verified in `quinn-proto/src/congestion/cubic.rs`). Pacing gives
~61 MB/s against our 60 KB/s.
→ **Caveat: this holds only if input has its own connection.** Sharing it with bulk
transfer *will* block input datagrams (quinn issue #2156, still open).

**TCP tail-loss stalls are the thing we are eliminating.** Fast retransmit needs 3
duplicate ACKs ≈ 4 ms stall. But if the user stops moving right after a loss, no further
packets arrive, fast retransmit can never fire, and you fall to RTO:

| OS | Minimum RTO | Source |
|---|---|---|
| RFC 6298 | 1 s | spec |
| Linux | **200 ms** | `TCP_RTO_MIN` in `include/net/tcp.h` |
| macOS/XNU | **~230 ms** | `TCPTV_REXMTMIN` 30 ms + `TCPTV_REXMTSLOP` 200 ms |
| Windows | not verified from a primary source | commonly cited ~300 ms |

**TCP guarantees delivery of data we no longer want.** A retransmitted 200 ms-old cursor
position has *negative* value — it must be delivered, in order, ahead of the current
position, then discarded. A lost datagram is simply superseded 1 ms later.

**quinn's datagram drop policy is already correct.** `send_datagram()`: *"Previously
queued datagrams which are still unsent may be discarded to make space for this datagram,
in order of oldest to newest."* That is drop-oldest — exactly right for superseding
events. **Never use `send_datagram_wait()`**, which prioritises old datagrams over new.
(quiche and s2n-quic both reject the *newest* instead — wrong policy for us.)

**Connection migration is the killer feature.** `ServerConfig::migration` is **enabled by
default**; QUIC keys on Connection ID, not the 4-tuple.
- TCP **dies** on IP change → teardown, rediscovery, re-handshake.
- DTLS 1.2 (lan-mouse's stack) **also dies** — connection ID is a DTLS *1.3* feature.
- QUIC **survives**, with PATH_CHALLENGE validation.

Limitation: quinn's *deliberate* client migration is `Endpoint::rebind(socket)`, which is
coarse. Passive migration works out of the box.

## Latency engineering

Ranked by magnitude — note the top item is on the **receiver**, which is counter-intuitive:

| Source | Cost |
|---|---|
| **Wi-Fi power save on the receiver** | **100 ms+** (DTIM 100–300 ms) |
| TCP RTO on tail loss | 200–230 ms |
| Nagle + delayed ACK | 40–200 ms |
| Windows low-level hook timeout | up to 1000 ms before unhooking |
| macOS `CGEventTap` timeout | tap silently disabled until re-armed |
| Timer-based batching | = the batch interval (self-inflicted — **don't**) |
| NIC interrupt coalescing | ~50–100 µs |
| Heap allocation in hot path | 20–100 ns (jitter, not mean) |

**This explains the 78 ms RTT measured to the Windows peer on this LAN.** It is radio
power-save, not distance, and no wire-format work can fix it.

Countermeasures: `iw dev wlan0 set power_save off` (Linux); power plan → Maximum
Performance (Windows). **macOS exposes no application-accessible API to disable Wi-Fi
power save** — a real, unfixable-in-userspace gap. Universal mitigation: a heartbeat at
50–100 Hz while a session is active keeps the radio out of deep sleep.

DSCP/EF marking is a **Linux/macOS bonus only** — on Windows `IP_TOS` is disabled by
default and needs group policy. Do not design around it.

### Realistic budget (engineering targets, not measurements)

Click-to-photon on a local machine is 20–60 ms. What we control is *added* latency, so
measure **differentially**.

| Segment | Wired gigabit | Wi-Fi 6 |
|---|---|---|
| Added one-way, p50 | 0.3–1.0 ms | 1–3 ms |
| Target p99 | < 2 ms | < 10 ms |
| Target p99.9 | < 5 ms | < 25 ms |
| Hard ceiling | 10 ms | 50 ms |

Optimising p50 below ~1 ms is wasted — invisible against a 20–60 ms display pipeline.
**Max is the headline number**: one 200 ms freeze per session is what makes a KVM feel
broken, and it is invisible in a mean.

### How to measure

**Tier 1 — hardware, two-machine differential.** An MCU enumerates as USB HID into
machine A and injects a click at t₀; a photodiode on machine B's screen detects the change
at t₁. Same MCU clock both ends ⇒ **no clock-sync problem at all**. Then repeat with the
photodiode on machine A to get `L_local`; **added latency = L_remote − L_local**, which
cancels the display pipeline. Open designs to build from: OSLTT, Open-Source-LDAT.

**Tier 2 — in-protocol echo.** Cheap, continuous, shippable as a diagnostic. Good for
trends, not absolute truth (path asymmetry, timestamping point, scheduler delay).

**Statistics.** `hdrhistogram 7.6.0`; report p50/p99/p99.9/max, never the mean. **Correct
for coordinated omission** via `record_correct(value, expected_interval)` — a naive send
loop stops generating samples during a stall, hiding exactly the events that matter. Drive
load from a fixed-rate scheduler, not a closed loop.

## Clock synchronization

**In-protocol NTP-style four-timestamp estimator. Not PTP, not a dependency on NTP.**
PTP needs NIC hardware support and privileges that consumer laptops do not have. NTP is
*"usually no better than 1 ms"* on a LAN, and both machines' independent offsets *add*.

```
delay  = (t4 - t1) - (t3 - t2)
offset = ((t2 - t1) + (t3 - t4)) / 2
```
1. **Minimum-RTT filtering** over ~64 samples — accept the offset from the smallest
   `delay`. This single filter buys most of the accuracy on Wi-Fi.
2. **Skew estimation** by regression: consumer crystals drift 10–50 ppm = 0.6–3 ms/min, so
   remove the *rate*, not just the offset.
3. **Clamp and slew, never step.**

Expected: ±50–200 µs wired, ±0.5–1 ms Wi-Fi. Our `Ping`/`Pong` frames already carry the
needed timestamps.

**Clock source — easy to get wrong:**

| Purpose | Linux | macOS | Windows |
|---|---|---|---|
| Hot path / intervals | `CLOCK_MONOTONIC` | `mach_absolute_time` | `QueryPerformanceCounter` |
| Elapsed **across suspend** | `CLOCK_BOOTTIME` | `mach_continuous_time` | `QueryUnbiasedInterruptTime` |

`CLOCK_MONOTONIC` does **not** advance during suspend, so an hour of sleep looks like
being busy. **Detect suspend via the boottime-clock gap**, then discard clock-sync state
entirely — the offset is meaningless after a resume.

## Pointer accuracy — physical units, not pixels

Confirms and extends seam's D1. Two additions:

**Send a cumulative sub-pixel odometer** (already D1). Loss self-heals, reordering
self-heals, coalescing is free and lossless, and it composes perfectly with quinn's
drop-oldest buffer. *"This one decision eliminates the entire drift/jitter/loss-accuracy
problem class."*

**Work in physical units (millimetres / DIPs), not pixels** — this is the root fix for
Deskflow's whole DPI bug family, and directly serves goal criteria F11–F13:
1. Each host reports its monitors in **mm** (from EDID physical size) plus pixel size and
   scale factor.
2. The shared virtual desktop is arranged in **physical space**, so a 27" 4K and a 13"
   laptop line up the way the user's hand expects.
3. Non-rectangular layouts: model each machine as a **union of rectangles**; edge crossing
   is segment intersection against the union boundary. Carry the crossing point as a
   **physical offset along the edge, not a fraction** — fractions are exactly why
   Deskflow's cursor jumps between different-resolution monitors.
4. **Dead zones must not trap the cursor** — clamp locally, do not hand off.
5. Convert to target pixels only at the final injection step.

**Bypass pointer acceleration on both ends:**

| Platform | Capture unaccelerated | Inject unaccelerated |
|---|---|---|
| Windows | **Raw Input** | `SendInput` + `ABSOLUTE\|VIRTUALDESK`. Relative `MOUSEEVENTF_MOVE` is *"subject to mouse speed and the two-mouse threshold"* — never use |
| macOS | `CGEventTap` delta fields; no documented raw path (platform limitation) | `CGWarpMouseCursorPosition`, or `CGEventPost` with explicit delta fields |
| Linux/X11 | evdev `REL_X`/`REL_Y` | uinput / XTEST |
| Linux/Wayland | InputCapture portal + libei | RemoteDesktop portal + libei |

At 8000 Hz polling the *local* cost is 2–10 % of a core before our code runs. With the
odometer design we can safely cap the send rate at ~1000 Hz — lossless in the only sense
the user perceives.

## Discovery

**`mdns-sd` 0.20.2** (2026-07-17) — pure safe Rust, all three OSes, no system daemon, runs
its own thread. Self-describes as *"still beta"*; acceptable as the best-maintained option.
Rejected: `zeroconf` / `astro-dnssd` need Bonjour/Avahi daemons; `searchlight` stale.

**mDNS fails in real environments** — AP client isolation, enterprise Wi-Fi blocking,
multicast rate-limiting, VPN/Docker interface chaos, and port 5353 contention with macOS's
`mDNSResponder`. Precedent: **Syncthing deliberately does not use mDNS** (UDP broadcast
`255.255.255.255:21027` with a 32-bit magic); KDE Connect uses UDP broadcast 1716.
**lan-mouse has no discovery at all** — just `lookup_host` on a user-supplied name.

→ **Three tiers: mDNS → UDP broadcast beacon every 2 s → manual host:port, always
available.** Detect network changes with `if-watch 3.2.2`, but note it **falls back to
10-second polling on macOS** — use `NWPathMonitor` there.

## Pairing — 6-digit SAS bound to the TLS exporter

This is the answer to the user's "first connection takes many tries" complaint (goal O1–O6).

1. Both sides hold a long-lived self-signed cert (`rcgen 0.14.8`) or RFC 7250 raw public key.
2. First connection uses a permissive rustls verifier that *records* the peer SPKI without
   trusting it. Mutual TLS both ways.
3. Both sides independently compute, via RFC 5705 exporter:
   ```rust
   let mut sas = [0u8; 8];
   conn.export_keying_material(&mut sas, b"seam-pair-v1", None)?;
   let code = u64::from_be_bytes(sas) % 1_000_000;   // 6 digits
   ```
4. Both machines show the same 6 digits; the user confirms on one side.
5. Each pins the other's SPKI SHA-256 permanently. Silent thereafter.

**Why this beats the alternatives:**
- **vs. Barrier/Deskflow fingerprint dialogs:** a 64-hex-char fingerprint is unverifiable
  by humans, so people click accept reflexively — or pass `--disable-crypto`, which is
  exactly what the Barrier instance on this machine was doing. Six digits that must
  *match* is a comparison task humans actually perform.
- **vs. SPAKE2:** the `spake2` crate warns *"never received an independent third party
  audit… USE AT YOUR OWN RISK!"* Channel-bound SAS gets the same MITM resistance from
  rustls primitives only.
- **MITM resistance is structural:** a man-in-the-middle necessarily terminates two
  *different* TLS sessions → two different exporter outputs → the codes visibly differ.
  Same construction as ZRTP and Bluetooth numeric comparison.

## Reliability

**Periodic full-state reconciliation** (already seam's D5, independently confirmed):
every 250 ms and on every change, send an authoritative digest of held keys, buttons,
modifiers and cumulative position on the **reliable** stream. The receiver diffs against
its own emulated state and corrects. A lost key-up repairs itself within 250 ms with no
flush logic and no special cases. *"Both lan-mouse and Deskflow would be fixed by this one
mechanism."*

**Three independent cursor safeties, all required:**
1. Sender-side liveness watchdog (~300 ms, adaptive via `Connection::rtt()`) → un-grab.
2. Receiver-side watchdog (~1 s) → release all held keys and buttons.
3. **Unconditional local release chord**, evaluated *in the capture layer* before any
   network involvement, so it works when the peer is dead, the network is gone, or the
   async runtime is wedged. **This must never depend on the transport being alive.**

**On disconnect, in order:** button-up for every held button *first* (a stuck mouse button
mid-drag is worse than a stuck key — it selects and moves things), then key-up for every
held key, then zero the modifier mask, then tear down the emulated device.

**Reconnect** with exponential backoff + jitter capped at ~5 s, but **reset the backoff
immediately on a network-change event** — the common case is "Wi-Fi came back", and making
the user wait 5 s for it is the interruption we are trying to eliminate.

**Wayland caveat:** the InputCapture portal is asynchronous and `Deactivated` may arrive
after our `Release` call — there is **no guarantee of immediate release**, so safety (3)
must not assume the portal responded.

## Serialization — `zerocopy` for the hot path

| Crate | Ver | Verdict for a fixed 32-byte event |
|---|---|---|
| **zerocopy** | **0.8.55** | ✅ encoding is a pointer cast, not a serializer |
| postcard | 1.1.3 | ❌ varint → *data-dependent* event size |
| bincode | 3.0.0 | 🟡 a serialization pass we don't need |
| rkyv | 0.8.17 | ❌ built for large object graphs |

Derive `IntoBytes, FromBytes, Immutable, KnownLayout, Unaligned`; decoding is a
*validated* reference cast, which matters because this data is from the network.

> **Note vs. our current implementation.** `seam-proto` hand-rolls a big-endian codec with
> a reusable buffer, which already meets the zero-allocation requirement and gives an
> exact written spec plus a fuzzable surface. `zerocopy` would remove the remaining
> per-field copy on the motion datagram specifically. **Decision: keep the hand-rolled
> codec for the spec and the control plane; benchmark a `zerocopy` fast path for `Motion`
> before adopting it.** Do not adopt on principle without a measurement.

Use `postcard 1.1.3` for the control plane (pairing, config, clipboard metadata) where
schema evolution matters. Two formats for two jobs is correct here, not inconsistent.

## Crate versions (2026-07-25)

| Crate | Version | Note |
|---|---|---|
| **quinn** | 0.11.11 | MSRV 1.85 |
| **rustls** | 0.23.42 | (0.24.0-dev exists — don't) |
| **rcgen** | 0.14.8 | self-signed certs |
| **tokio** | 1.53.1 | |
| **socket2** | 0.6.5 | `IP_TOS`, buffer sizing |
| **bytes** | 1.12.1 | required by quinn's datagram API |
| **zerocopy** | 0.8.55 | hot-path candidate |
| **hdrhistogram** | 7.6.0 | has `record_correct()` |
| **rtrb** | 0.3.4 | wait-free SPSC, realtime-safe |
| **mdns-sd** | 0.20.2 | discovery |
| **if-watch** | 3.2.2 | 10 s polling on macOS — use `NWPathMonitor` |
| **tracing** | 0.1.44 | |
| ~~rdev~~ | 0.5.3 | ❌ abandoned 2023-06 |
| ~~spake2~~ | 0.4.0 | ❌ explicitly unaudited |

## quinn transport config — every line fixes a wrong-for-us default

```rust
let mut tc = TransportConfig::default();
tc.initial_rtt(Duration::from_millis(1));             // default 333 ms — biggest single win
tc.datagram_send_buffer_size(4 * 1024);               // default 1 MiB ≈ 17 s of stale input
tc.datagram_receive_buffer_size(Some(16 * 1024));     // default 1.25 MB
tc.keep_alive_interval(Some(Duration::from_secs(1))); // default None — NAT + radio wake
tc.max_idle_timeout(Some(Duration::from_secs(5).try_into()?));  // default 30 s
tc.initial_mtu(1400);                                 // default 1200; skip LAN discovery ramp
tc.ack_frequency_config(Some(Default::default()));    // thin the ~500 ACK/s backflow
// keep Cubic — BBR is marked "Experimental! Use at your own risk"
// ServerConfig::migration is already true by default — leave it
```

**Threading:** OS capture thread (elevated priority, no alloc, no locks, no async) →
`rtrb` SPSC ring → tokio/quinn network thread. **Never coalesce on a timer** — timer
batching converts jitter into *guaranteed* added latency. **Don't chase GSO/GRO**: they
are Linux/Android only in `quinn-udp`, and we have one small packet per event with nothing
to batch without adding delay.

## Honest limitations

- Windows minimum RTO not verified from a primary Microsoft source.
- quinn's per-datagram CPU cost vs a raw UDP socket is unpublished — measure it.
- macOS Wi-Fi power save: no application-accessible API found; the keepalive mitigation is
  empirical, not guaranteed.
- The §"Realistic budget" figures are **targets derived from component measurements**, not
  end-to-end results. Validate with the differential hardware method before committing.
- Apple Universal Control's transport is undocumented; BLE→AWDL is structural inference.

---

## Addendum: a cross-platform clock trap (verified 2026-07-25)

**Windows `QueryPerformanceCounter` keeps counting while the machine is asleep.** MS Learn,
verbatim: it returns ticks since boot *"including the time when the machine was in a sleep
state such as standby, hibernate, or connected standby."*

So QPC behaves like `CLOCK_BOOTTIME` / `mach_continuous_time`, **not** like
`CLOCK_MONOTONIC` / `mach_absolute_time`. "Use the monotonic clock everywhere" therefore
gives *different suspend semantics per platform* — and a laptop peer sleeping mid-session
is exactly the case seam must survive (R3).

| Purpose | Linux | macOS | Windows |
|---|---|---|---|
| Latency measurement (short spans, never crosses sleep) | `CLOCK_MONOTONIC` | `mach_absolute_time` | `QueryPerformanceCounter` |
| Offset tracking (must survive suspend) | `CLOCK_BOOTTIME` | `mach_continuous_time` | `QueryPerformanceCounter` |

Windows' `CLOCK_MONOTONIC` analogue is `QueryUnbiasedInterruptTime`, but its resolution is
affected by `timeBeginPeriod`, so it is unsuitable for measurement.

**Suspend detection without an OS notification API:** track `BOOTTIME − MONOTONIC` (Linux)
or `continuous − absolute` (macOS). A jump in the difference *is* a suspend.

**On resume, discard the entire clock-sync state and re-converge.** A skew estimate from
before a suspend is worse than none: the crystal was at a different temperature and the
peer's clock may not have advanced at all.

Two more findings that change platform code:

- **`timeBeginPeriod` no longer has a global effect** (Windows 10 2004+), and on **Windows
  11 a process that is occluded or minimised loses its high timer resolution**. A KVM
  forwarding input is almost always occluded. Budget for the timer silently reverting to
  ~15.6 ms.
- **Windows Wi-Fi: `wlan_intf_opcode_power_setting` does not exist.** The settable opcodes
  are `autoconf_enabled`, `background_scan_enabled`, `radio_state`, `bss_type`,
  `media_streaming_mode`, `current_operation_mode`. The two worth using are
  **`media_streaming_mode = TRUE`** and **`background_scan_enabled = FALSE`** (off-channel
  background scans are a large, under-appreciated source of 100 ms+ jitter). Both reset on
  disconnect — **re-apply on every reconnect**.
- **macOS has no radio power-save API.** `kIOPMAssertNetworkClientActive` is about *system*
  sleep, not the radio. Confirmed from Apple's `IOPMLib.h`.
- ⚠️ **The keepalive-keeps-the-radio-awake hypothesis is unmeasured.** Mechanically
  plausible (PSM/U-APSD use inactivity timers) but no measurement was found. Treat as a
  hypothesis to settle with our own differential rig, not an established fact.
