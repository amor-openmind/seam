# seam

> **seam** *(n.)* — the line where two screens meet, and the thing you should never
> notice crossing.

One mouse, one keyboard, one clipboard, across every machine on your desk.
A software KVM in Rust, for macOS, Windows and Linux.

Inspired by [Barrier], [Input Leap], [Deskflow], [Synergy] and [lan-mouse] — and built
around the three things all of them get wrong.

[Barrier]: https://github.com/debauchee/barrier
[Input Leap]: https://github.com/input-leap/input-leap
[Deskflow]: https://github.com/deskflow/deskflow
[Synergy]: https://symless.com/synergy
[lan-mouse]: https://github.com/feschber/lan-mouse

> **Status: early.** The protocol core is implemented and tested. Platform backends,
> transport and clipboard are next. See [`docs/GOAL.md`](docs/GOAL.md) for the full
> contract and honest progress against it.

---

## Why another one

Three problems define this software category. seam is designed around fixing them
structurally rather than patching the symptoms.

### 1. Keyboard layouts and languages

Every existing tool puts **one** key identity on the wire and makes the receiver guess.

- Send only the **physical** key: a US sender pressing `;` types `ن` on a Persian
  machine. Text is destroyed; shortcuts survive.
- Send only the **character**: `Cmd+C` on a German keyboard becomes "whatever key
  currently produces `c`". Text survives; shortcuts are destroyed.

Neither is right, because the user means different things in the two cases. seam sends
**both** identities and decides per event:

| You press | seam replays | Result |
|---|---|---|
| `Cmd`+`C` | the **physical key** | Copy still works on any layout |
| `ی` | the **character** | You get `ی`, not `d` |
| `AltGr`+`Q` (German `@`) | the **character** | You get `@`, not `q` |
| `F5`, arrows, modifiers | the **physical key** | No text to reproduce |

AltGr is deliberately **not** treated as a command modifier — treating it as one is a
real bug in this class of software, and it makes `@` untypeable across machines.

### 2. It interrupts you

Stuck modifier keys. A cursor trapped on a machine that went to sleep. Silent
disconnects. In seam these are structural, not best-effort:

- **Key state is an explicit, comparable value.** A heartbeat carries a digest of the
  pressed-key set; any divergence — from *any* cause, including ones nobody anticipated —
  is detected within one heartbeat and repaired. Correctness stops depending on every
  code path being right.
- **Motion is a cumulative odometer, not deltas.** A lost delta packet offsets your
  pointer forever; a lost cumulative packet is fully corrected by the next one. Zero
  drift, no resync jumps, and safe to send unreliably.
- **The receiver releases on silence**, using its own watchdog. It never waits to be told
  by a peer that may be dead.
- **Capture fails open.** Never freeze the user's machine, even at the cost of not
  forwarding input.

### 3. The first connection

Getting two machines talking for the first time should not take many attempts.

| Everyone else | seam |
|---|---|
| Pick which machine is "server" | Peers are symmetric |
| Type the other machine's IP | mDNS discovery |
| Screen name must exactly match the hostname | Identity is a random `PeerId`; names are cosmetic |
| A 64-hex-character fingerprint to eyeball | A 6-digit code that must match |
| …so people pass `--disable-crypto` | There is no unencrypted mode |
| Hand-edit a config file | No file to edit |

---

## Design at a glance

| | |
|---|---|
| **Transport** | QUIC ([quinn]) — unreliable datagrams for motion, reliable streams for state |
| **Security** | TLS 1.3, mandatory, with pinned SPKI after a 6-digit pairing confirmation |
| **Layout** | Both physical (USB HID usage) and logical (Unicode) key identity on the wire |
| **Geometry** | Screens arranged in **physical units**, so mixed DPI and resolution line up |
| **Codec** | Hand-rolled, zero-dependency, zero-allocation on the motion path |
| **Clipboard** | Text, HTML, images, and **files and folders** |

[quinn]: https://github.com/quinn-rs/quinn

Why QUIC and not TCP: on one TCP connection a retransmitted clipboard chunk
head-of-line-blocks your pointer, and a tail-loss stall costs 200–230 ms — a visible
freeze. Why not raw UDP: state transitions like key-up *must* be reliable, and rolling
your own retransmission is more work than using quinn, not less. QUIC also survives IP
changes and sleep/wake without reconnecting, which neither TCP nor DTLS 1.2 can do.

Full reasoning, with sources and measurements, in [`docs/research/`](docs/research/).

---

## Repository layout

```
crates/
  seam-proto/     wire protocol: types, codec, key state, motion model  [implemented]
docs/
  GOAL.md         the contract: scope, success criteria, decisions, progress
  research/       platform APIs, transport, prior art — with sources
```

## Building

Requires Rust 1.90+ (edition 2024).

```bash
cargo test
```

## Platform support

| Platform | Send input | Receive input | Notes |
|---|---|---|---|
| macOS 13+ | planned | planned | Needs Accessibility + Input Monitoring |
| Windows 10/11 | planned | planned | |
| Linux (Wayland) | planned | planned | Capture needs GNOME or KDE; injection works everywhere |
| Linux (X11) | planned | planned | Compatibility tier — GNOME 49 removed its X11 session |
| ChromeOS | **not possible** | degraded | See below |

**ChromeOS, stated honestly:** Google documents global input capture as a deliberate
security boundary, naming Synergy specifically as something that "will not work". A
Chromebook can only ever be a **receive-only client** via a fullscreen PWA, plus clipboard
sync. It can never capture input or inject into the ChromeOS shell. This is a hard limit,
documented rather than quietly dropped.

## Licence

MIT OR Apache-2.0.
