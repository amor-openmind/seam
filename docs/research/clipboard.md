# Clipboard and file transfer — 2026 findings

Research pass completed 2026-07-25. Verified against Apple's documentation JSON API and
SDK headers, learn.microsoft.com, X.Org ICCCM, wayland.app, and real source on GitHub.
Unverified items are flagged and collected at the end.

## The three things that matter

1. **No existing software-KVM does clipboard file copy/paste well.** Synergy 3 is text +
   images only. Deskflow **removed** file drag-and-drop and is text + images only. Mouse
   Without Borders is single-file, eager, 100 MB cap, no folders. **This is a genuinely
   open niche** — and `barrier#855` (236 reactions, the most-reacted issue in that repo)
   is the demand for it.
2. **Only Windows has a real OS-level promise for file *bytes*** —
   `CFSTR_FILEDESCRIPTORW` + `CFSTR_FILECONTENTS` over an `IDataObject`. macOS's
   `NSFilePromiseProvider` is **drag-and-drop only**: Finder never calls
   `writePromiseToURL:` on ⌘V. Linux has no promise at the selection layer at all.
3. **Therefore the portable design is a virtual filesystem, not a clipboard promise.**
   FreeRDP and RustDesk both converged on exactly this: a FUSE mount whose `read()` maps
   to a network range request. Put real paths (into our VFS) on the macOS/Linux clipboard;
   use the native `IDataObject` promise on Windows.

## macOS

**Change detection is polling only.** There is no notification or KVO API for pasteboard
changes — `NSPasteboard.changeCount` is the only supported mechanism. 500 ms is the
de-facto standard interval; we can afford 100–200 ms while focused and back off when not.

### ⚠️ The macOS 15.4+ paste privacy gate — the biggest risk to this feature

`NSPasteboard.accessBehavior` (macOS 15.4+):

| Case | Behaviour |
|---|---|
| `.default` | Ask on programmatic access to the General pasteboard. App is not listed in System Settings until it first triggers an alert, then flips to `.ask`. |
| `.ask` | Notify and ask — **except** access that is both *user-originated and paste-related*, which is always allowed silently. |
| `.alwaysAllow` | Allow silently. |

- Reading `changeCount` is metadata and **almost certainly** exempt — this is why the
  polling pattern is universally recommended. ⚠️ **Apple never states this explicitly.
  Verify empirically before shipping.**
- Reading actual data **is** programmatic access and **will** trigger the alert, after
  which the app appears under **Privacy & Security → Paste from Other Apps**.
- → **Ship a real `.app` with a stable bundle ID, and make "Always Allow" an explicit
  onboarding step.** A daemon that silently gets denied looks like a broken product.

Also: `NSPasteboard.general` automatically participates in Universal Clipboard and *"there
is no macOS API for interacting with this feature"* — we cannot suppress Handoff
interference, and may see double-sync.

### File URLs

`NSPasteboardTypeFileURL` (`public.file-url`) is current. `NSFilenamesPboardType` is
deprecated (10.14) but still worth writing for old apps. `NSPasteboardTypeFileContents` is
legacy.

- **N files ⇒ N `NSPasteboardItem`s.** `data(forType:)` on the pasteboard only sees the
  **first** item — a classic bug. Always iterate `pasteboardItems`.
- **There is no distinct pasteboard type for a folder.** Both are `public.file-url`;
  discriminate by resolving the URL (`isDirectoryKey`).
- ⚠️ The exact UTI list Finder writes on ⌘C could not be authoritatively confirmed.
  **Enumerate `types` at runtime; key off `public.file-url` with `NSFilenamesPboardType`
  as fallback.**

### Lazy data: two mechanisms, only one usable

- ✅ **`NSPasteboardItemDataProvider` / `NSPasteboardTypeOwner`** — genuine lazy *data*
  fulfilment, works on the general pasteboard. The owner object (and our process) must
  stay alive. ⚠️ Whether AppKit force-renders at termination is undocumented — assume not.
- ❌ **`NSFilePromiseProvider`** — conforms to `NSPasteboardWriting`, so it *can* be
  written to the pasteboard, but **Finder does not resolve it on ⌘V**. Verified in
  [Apple Developer Forums 683081](https://developer.apple.com/forums/thread/683081):
  `fileNameForType:` is called, but *"`writePromiseToURL:` is never called — Finder can't
  paste"*. **No Apple/DTS answer since 2021**, independently confirmed by a second
  developer. **macOS has no working clipboard file promise. Plan around it.**

### Cut vs copy

**There is no cut for files on macOS and no pasteboard flag.** Finder has no Edit▸Cut for
files; ⌘⌥V ("Move Item Here") is a paste-time decision made entirely inside Finder.
Nothing is recorded at copy time.

→ **Model macOS as copy-only for files.** For a cut arriving from Windows/Linux, paste as
copy and offer an explicit confirmation, and carry a private UTI
(`dev.seam.dropeffect`) so seam↔seam cut works even though Finder↔seam cut cannot.

### Permissions

No TCC permission for the pasteboard itself beyond the paste alert, but reading the *files*
those URLs point at hits TCC per-location (Desktop, Documents, Downloads, iCloud,
removable volumes). → **Request Full Disk Access during onboarding** — piecemeal prompts
are a terrible UX. **Do not sandbox this daemon.**

## Windows

### `CF_HDROP` + `DROPFILES`

`sizeof(DROPFILES)` = 20 on both x86 and x64, but write `sizeof(DROPFILES)` into `pFiles`
and honour whatever offset you read. Wide mode ends with **four zero bytes**.
`DragQueryFileW(hDrop, 0xFFFFFFFF, ...)` returns the count.

**A copied folder is just `CF_HDROP` with the folder's path.** No separate format, no
recursion, no content — the receiver walks the tree itself.

> For a KVM only `CF_HDROP` + `Preferred DropEffect` are needed. **Never marshal
> `CFSTR_SHELLIDLIST` PIDLs across machines** — they are machine-local and meaningless
> remotely.

### Cut vs copy — the full move handshake

Explorer signals Cut with `Preferred DropEffect = DROPEFFECT_MOVE`. **Ignore this and
every cut silently becomes a copy.**

| Constant | String | Direction |
|---|---|---|
| `CFSTR_PREFERREDDROPEFFECT` | `"Preferred DropEffect"` | source → target |
| `CFSTR_PERFORMEDDROPEFFECT` | `"Performed DropEffect"` | target → source |
| `CFSTR_LOGICALPERFORMEDDROPEFFECT` | `"Logical Performed DropEffect"` | source ← object |
| `CFSTR_PASTESUCCEEDED` | `"Paste Succeeded"` | target → source |

The source deletes originals only on receiving **both** `PASTESUCCEEDED=MOVE` **and**
`PERFORMEDDROPEFFECT=MOVE`.

> **seam rule: never implement an optimized move across the link.** Act as a target that
> performs a plain copy, then relay the two signals back so the *origin* machine's agent
> deletes the originals. The destructive step stays on the machine that owns the files,
> and a failed transfer is trivially non-destructive.

### Virtual files — the only true OS-level promise

`CFSTR_FILEDESCRIPTORW` (`"FileGroupDescriptorW"` — note the constant/string mismatch;
the ANSI name is a *different* format id). `sizeof(FILEDESCRIPTORW)` = **592**.

**✅ Directory trees ARE expressible** via relative `cFileName` with backslashes, plus
`FILE_ATTRIBUTE_DIRECTORY`. This is **not** on the MSDN page — the docs are incomplete —
but is corroborated by Chromium (`os_exchange_data_provider_win.cc`), FreeRDP
(`winpr/libwinpr/clipboard/posix.c`) and VirtualBox.
- `cFileName` **must be relative**; `"Folder\sub\file.txt"` builds the hierarchy.
- A directory entry needs **both** `FD_ATTRIBUTES` in `dwFlags` **and**
  `FILE_ATTRIBUTE_DIRECTORY` in `dwFileAttributes`.
- Intermediate dirs are created implicitly — explicit entries only needed for **empty** dirs.
- `FD_PROGRESSUI` is read **only from element 0**.
- ⚠️ The conjunction rule rests on leaked Windows XP `shell32` source. **Verify on Windows 11.**

`CFSTR_FILECONTENTS` — one per descriptor, selected by `FORMATETC.lindex`. **Always offer
`TYMED_ISTREAM`**: the `TYMED_HGLOBAL` path reads **`nFileSizeLow` only and never
consults `nFileSizeHigh`** — a hard **4 GB ceiling**, with Raymond Chen's own unresolved
TODO still in the source.

This is exactly how Outlook, 7-Zip and WinSCP work, and exactly the seam architecture:
cheap descriptor from a remote directory listing, bytes on demand.

`IDataObjectAsyncCapability` (formerly `IAsyncOperation`, **same IID**) is the key to good
progress UX — return from `Drop` immediately, extract off-thread, then `SetData` the
outcome. Without it a multi-GB cross-machine paste blocks both message pumps.

### OLE lifecycle traps

- With OLE, **everything is delay-rendered always**; you never write `WM_RENDERFORMAT`
  handlers — OLE's internal window delegates to `IDataObject`.
- `OleSetClipboard` **fails with `CLIPBRD_E_CANT_OPEN`** if another app has the clipboard
  open. Clipboard managers do this constantly. **Retry with backoff.**
- ⚠️ **`OleFlushClipboard` is fundamentally incompatible with virtual files** — it forces
  the *entire* remote payload to download at exit. **Never flush a virtual-file data
  object.** Keep the agent alive while `OleIsCurrentClipboard()` says we own it.
- There is a **~30 second system timeout** on delay-rendered data, and rendering runs
  inside a window message so the app is visibly unresponsive meanwhile. `TYMED_ISTREAM` +
  `IDataObjectAsyncCapability` is what keeps us under it.

### Change notification

**`AddClipboardFormatListener` + `WM_CLIPBOARDUPDATE`** (Vista+). Do **not** use
`SetClipboardViewer` — Microsoft explicitly says it exists only for backward
compatibility. `GetClipboardSequenceNumber()` is a complementary idempotence guard
(`WM_CLIPBOARDUPDATE` can fire more than once per logical change).

Clipboard History carries no `CF_HDROP`, so our promises are invisible to it. If we place
paths as text, set `CanUploadToCloudClipboard = 0` — remote paths are meaningless on a
phone and a mild leak.

### 2026 status

**No clipboard API changes; nothing deprecated.** 25H2 is an enablement package on the
24H2 branch. **MSIX AppContainer is a problem** — not for the clipboard API but for the
*paths*. **Ship the file agent unpackaged or packaged-full-trust.**

## Linux / X11

Change detection: **XFIXES** (`XFixesSelectSelectionInput` → `XFixesSelectionNotify`).
Never poll.

### File targets — the fragmented reality

**`text/uri-list`** (RFC 2483, CRLF, percent-encoded `file://`) is the lowest common
denominator every file manager understands for *copy*. Folders look identical to files.

**`x-special/gnome-copied-files`** carries cut-vs-copy on GNOME. Exact format verified from
current Nautilus source (`src/nautilus-clipboard.c`):

```
first line "cut" or "copy", then \n-separated file:// URIs, NO trailing newline
```

Nautilus's own comment: *"While it is not a public API and the format is not documented,
some apps have come to use this atom/mime type to integrate with our clipboard."*

**KDE does NOT write `x-special/gnome-copied-files`.** Verified from KIO
`src/widgets/paste.cpp`: it uses **`application/x-kde-cutselection`** with body `"1"` or
`"0"`.

⚠️ `x-special/nautilus-clipboard` was the historical bug — Nautilus ~3.30 served that body
under `text/plain`, which is why pasting a copied file into a terminal produced literal
garbage (GNOME/nautilus#634). Current GNOME no longer does this; probe defensively.

→ **As a source, offer all of:** `text/uri-list`, `x-special/gnome-copied-files`,
`x-special/mate-copied-files`, `application/x-kde-cutselection`, `text/plain`.

### INCR

Kicks in above ~256 KB. Owner replies with type `INCR`, then appends chunks, **waiting for
a `PropertyNotify(Deleted)` between each**, terminating with a zero-length write. GTK, Qt,
`x11-clipboard` and arboard's X11 backend all implement it.

## Linux / Wayland

**Core Wayland cannot work for a KVM daemon.** `wl_data_device.selection` is delivered
**only to the keyboard-focused client**, and `set_selection` requires a **valid serial from
a recent input event**. A surfaceless daemon has neither. This is *the* Wayland clipboard
problem.

### `ext_data_control_v1` — compositor support (wayland.app, 2026-07-25)

| ✅ Supported | ❌ Not supported |
|---|---|
| COSMIC, GameScope, Hyprland 0.52.1, Jay, **KWin (Plasma) 6.6**, Labwc, Louvre, Muffin (Cinnamon), niri, **Sway 1.11** | **Mutter (GNOME) 49.2**, Cage, Mir, phoc, river, Treeland, Wayfire, Weston |

Cross-checked in source: Sway registers **both** wlr- and ext- variants; Hyprland registers
ext-. **GNOME/Mutter has neither** — the refusal ([mutter#524](https://gitlab.gnome.org/GNOME/mutter/-/work_items/524))
is on privacy grounds and still stands.

⚠️ wayland.app's *wlr*-data-control table contradicted its *ext* table for Mutter. **Treat
"Mutter supports wlr-data-control" as false; probe at runtime, trust no table.**

### The GNOME path: `org.freedesktop.portal.Clipboard`

*"This portal does NOT create its own session"* — it attaches to a **RemoteDesktop** or
**InputCapture** session. **A software KVM is exactly a RemoteDesktop session; this is the
intended use case.** API: `RequestClipboard` (must precede `Start`), `SetSelection`,
`SelectionWrite`/`SelectionWriteDone`, `SelectionRead`, signals `SelectionOwnerChanged`,
`SelectionTransfer`.

⚠️ **Backend availability is the open question.** A [$5,000 Algora bounty on
Deskflow](https://algora.io/claims/kwaRn4fEP9aF3hrZ) (submitted 2025-09-30, still pending)
is explicitly to *implement* these backends in `xdg-desktop-portal-gnome` and `-kde`.
**Verify at runtime; do not assume.**

Wayland data transfer is a **pipe fd** — no INCR. ⚠️ **Deadlock hazard**: the receiver must
read promptly and the sender must be ready to block. Use a dedicated thread or non-blocking I/O.

## Moving the actual bytes

### What the field does

| Tool | Files? | Eager/lazy | Folders |
|---|---|---|---|
| Synergy 3 | ❌ | — | — |
| **Deskflow** | ❌ *"Drag and drop was removed and is no longer supported"* | — | — |
| Barrier / Input Leap | ❌ (DnD only) | Eager, 32 KB chunks | ❌ **no directory traversal at all** |
| Mouse Without Borders | ✅ crippled — `data = files[0]`, **only the first file** | Eager | ❌ ("zip them first") |
| **MS-RDPECLIP** | ✅ | **True lazy** | ✅ |
| **FreeRDP** | ✅ | **Lazy via FUSE** | ✅ |
| **RustDesk** | ✅ | Lazy — modified FreeRDP on Windows, **FUSE on Unix** | ✅ |
| VirtualBox | ⚠️ experimental | — | ❌ one file per transfer, no symlinks |

### MS-RDPECLIP is the reference design — copy it

Format List → Format Data Request/Response (fetch the descriptor) → **File Contents
Request/Response per file, per range**. `FILECONTENTS_SIZE` returns a `UINT64`;
`FILECONTENTS_RANGE` returns bytes with 64-bit offsets. `CB_LOCK_CLIPDATA`/`UNLOCK` with a
`clipDataId` pin the remote clipboard so a concurrent copy cannot invalidate an in-flight
transfer. Battle-tested; handles ranges, sizes, trees and locking.

### FreeRDP's FUSE clipboard — the portable answer

`client/common/client_cliprdr_file.c`, deliberately split out of the X11 client because
*"it is itself not X11 related, and can be reused"*.

Mounts a per-process temp dir, parses the incoming descriptor list into a tree, **then puts
those local paths on the native clipboard as ordinary file URIs** — so GTK/Qt/Nautilus see
plain files. `read()` → `FILECONTENTS_RANGE` → `fuse_reply_buf()`. Chunk cap **8 MB**.

⚠️ **Known bug to avoid** — [FreeRDP#12355](https://github.com/FreeRDP/FreeRDP/issues/12355):
`clear_selection()` aborts in-flight reads with `EIO` when the clipboard changes, so
**copying anything else during a large transfer kills it**. → **Scope VFS entries by a
clipboard-generation id and keep an old generation alive while any handle is open.**

### Can macOS and Linux do a true promise? Direct answers

| | True clipboard promise for file bytes? |
|---|---|
| **Windows** | ✅ Yes — `FILEDESCRIPTORW` + `FILECONTENTS` on `TYMED_ISTREAM` |
| **macOS** | ❌ No — `NSFilePromiseProvider` is drag-and-drop only |
| **Linux** | ❌ No — selections transfer *bytes for a MIME type*; there is no "content later" concept |

**So the fallback IS the design.** Materialise a filesystem, put real paths on the clipboard.

- **Linux:** FUSE via `fuser` 0.18.0 (updated 2026-07-22).
- **macOS:** **NFS loopback** — macOS ships an NFS *client* in the box, so serve the VFS
  over NFSv3 on `127.0.0.1` and `mount_nfs` it. **Zero installation, zero kext, zero user
  friction.** Rust: `nfsserve` 0.11.0 (HuggingFace fork, production-used for virtual
  filesystems). Preferred over FSKit (no Rust bindings) and far preferred over macFUSE
  (requires enabling third-party kexts in Recovery on Apple Silicon — unacceptable UX).
- **Windows:** don't bother — use the native `IDataObject` promise.

### Details that will bite

- **Symlinks.** Default to following them; opt-in preservation for Unix↔Unix. **Refuse or
  flatten absolute symlinks pointing outside the copied root** — that is a
  directory-traversal / exfiltration vector.
- **Permissions.** No clipboard format carries POSIX modes. Carry them in our own manifest;
  **mask off setuid/setgid/sticky unconditionally on receive** and never blindly honour a
  remote-supplied mode.
- **Quarantine.** A KVM link from another machine is arguably an untrusted source —
  consider deliberately setting `com.apple.quarantine` on macOS and `ZoneIdentifier` on
  Windows.
- **Progress.** Windows gives it nearly free (`FD_PROGRESSUI` + accurate `FD_FILESIZE` +
  async capability ⇒ a real Explorer dialog with Cancel). macOS/Linux via VFS have **no
  OS-provided progress** — we must show our own HUD, triggered by the first VFS read.
- **Source goes offline.** Show "the source machine disconnected", not a filesystem error.
  Cache completed files so a retry is incremental.

## Rust crates — and the gaps we must fill ourselves

| Crate | Ver | Files? | Verdict |
|---|---|---|---|
| `arboard` | 3.6.1 | ✅ get/set | Best baseline — but see gaps |
| `clipboard-rs` | **0.3.5** (2026-06-30) | ✅ get/set | **Best Linux file interop** — only crate that knows `x-special/gnome-copied-files` |
| `copypasta` | 0.10.2 | ❌ | **Text only**, panics on non-UTF-8. Not usable |
| `wl-clipboard-rs` | 0.9.3 | byte-level | **Excellent, use directly.** 0.9.2 added `ext-data-control` |
| `clipboard-win` | 5.4.1 | ✅ | Solid, but **no `IDataObject`, no delayed rendering** |
| `objc2-app-kit` | 0.3.2 | raw | Everything exposed, incl. `NSPasteboardItemDataProvider` |
| `windows` | 0.62.2 | raw | Everything exposed, incl. `IDataObjectAsyncCapability_Impl` |
| `ashpd` | 0.13.13 | portal | `desktop::clipboard`, *"mostly meant to be used along with RemoteDesktop"* |
| `fuser` | 0.18.0 | — | Linux VFS |
| `nfsserve` | 0.11.0 | — | **The kext-free macOS VFS route** |

**The gaps, verified by reading the crates' source rather than their READMEs:**

1. ❌ **arboard on Linux never emits `x-special/gnome-copied-files`** — a repo-wide search
   returns zero hits. ⇒ **files set by arboard will not paste correctly into
   Nautilus/Dolphin, and cut is impossible.** `clipboard-rs` has it, but **only in its X11
   backend**; its Wayland `set_files` normalises to `text/uri-list` only.
2. ❌ **Neither crate emits `application/x-kde-cutselection`.**
3. ❌ **arboard on Windows writes `CF_HDROP` only — no `CFSTR_PREFERREDDROPEFFECT`.**
   ⇒ every paste is a copy; cut is unrepresentable.
4. ❌ **No crate implements Windows virtual files.** A GitHub-wide code search for Rust
   using `FILEGROUPDESCRIPTORW` + `IDataObject_Impl` returns **one** hit, in an unrelated
   repo. This is the single biggest chunk of platform work in the project.
5. ❌ **No crate does macOS lazy pasteboard provision**, handles cut-vs-copy as a concept,
   or handles the macOS 15.4+ paste-permission flow.

→ **Do not take a single cross-platform clipboard crate.** Write a thin per-platform layer
on `windows` / `objc2-app-kit` / `x11rb` + `wl-clipboard-rs` + `ashpd`.

## Recommended build order

1. **Text + HTML + image**, all platforms, eager. This alone matches Synergy 3.
2. **File lists** — read native formats, **eager staging** to a temp dir on the target,
   write real paths back. Handles cut/copy correctly. **Already beats every OSS competitor.**
3. **Windows native promise** (`IDataObject` + `FileContents`). Instant win on the most
   common desktop.
4. **VFS** (`fuser` on Linux, `nfsserve` on macOS) to make macOS/Linux lazy too.
5. **Wayland**: data-control first, portal second.

**Loop prevention:** tag every snapshot we publish with `(origin, generation)` and ignore
change events whose content hashes to something we just wrote. RustDesk calls this
"ownership tracking"; without it the clipboard ping-pongs forever.

## Could not verify

1. Whether reading macOS `changeCount`/`types` triggers the 15.4+ paste alert. **Test on
   real hardware before designing onboarding.**
2. The exact UTI list Finder writes on ⌘C. Enumerate at runtime.
3. Whether AppKit force-renders `NSPasteboardItemDataProvider` types at termination.
4. Windows: the `FD_ATTRIBUTES && FILE_ATTRIBUTE_DIRECTORY` conjunction (leaked XP source
   + three implementations agreeing). **Verify on Windows 11.**
5. The `wayland-protocols` release that introduced `ext-data-control-v1` (reported as 1.39).
6. Whether `xdg-desktop-portal-gnome`/`-kde` ship a Clipboard backend today — a $5k bounty
   to implement them was still pending.
7. ShareMouse's mechanism — commercial, marketing claims only.
8. **Nothing here was executed on a live machine.** Every "you can do X" is derived from
   docs or source, not from a working prototype.
