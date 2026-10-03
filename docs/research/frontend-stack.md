# The frontend stack, chosen by measurement

A reference frontend is planned for whirl: a tray item plus a settings window,
macOS first, Windows and Linux after. This document picks its runtime with
numbers rather than taste, and it is written so a reader can rerun the two
commands the ordering rests on and get the same ordering.

It decides one thing: **which stack the reference frontend is built on.** It does
not decide the frontend's UI, its repository layout, or the protocol; the
protocol is `docs/architecture.md` section 2 and the frontend contract is section
8, and neither changes here.

Everything below was measured on 2026-10-02 on this machine:

```
$ sw_vers
ProductName:		macOS
ProductVersion:		26.7
BuildVersion:		25G229
$ system_profiler SPHardwareDataType | grep -E "Chip|Memory"
      Chip: Apple M3
      Memory: 24 GB
$ rustc --version
rustc 1.98.1 (48a229cea 2026-09-01)
$ swiftc --version
swift-driver version: 1.148.6 Apple Swift version 6.3.3 (clang-2100.1.1.101)
```

The three prototypes live in a scratch directory outside this repository and are
not committed, packaged or published. Its path is written as `<scratch>` below.

## 1. The candidates, and why these three

| Candidate | In one line |
|---|---|
| **Tauri** (Rust host, system WebView) | The smallest Rust host per screen, and the only candidate whose UI is not compiled into the process. |
| **egui/eframe with the `tray-icon` crate** (Rust, no WebView) | Rust end to end, one process, no WebView, and it can link this repository's own crate (`whirl-core`). |
| **Swift with AppKit** (the control) | The platform's own toolkit, and the only candidate that cannot link `whirl-core` directly: it reaches the crate only through a C-ABI bridge project of its own. |

`iced` was not built. `egui` was chosen over it for the Rust-no-WebView slot
because `eframe` plus `tray-icon` is the pair already used together by Tauri (the
same crate, one minor version apart: Tauri pins `tray-icon` 0.25, this spike uses
0.26), so the tray plumbing under test is the same code in both Rust candidates
and the comparison isolates the toolkit rather than the menu library.

## 2. What the prototypes are, and how the numbers were taken

Each prototype is the smallest honest version of the same three things:

1. a tray/menu bar item with `Next`, `Pause`/`Resume` and `Quit`;
2. one window with one form field (the control socket path);
3. a client that connects to the control socket, reads the greeting, sends
   `status`, and renders three keys out of the response.

Each also performs the platform-store check of section 6.3 in its own stack.

**The client was exercised against a stub daemon, not a real one.** No daemon was
running on this machine, and section 8's own rule is that a frontend never starts
one:

```
$ pgrep -fl whirl
(no output)
$ ls -la ~/Library/Application\ Support/whirl/whirl.sock
srw-------@ 1 <user> staff 0 Sep 27 02:03 .../whirl.sock
```

The socket is a leftover from 2026-09-27 and has no listener, so the stub stands
in. It implements section 2's framing exactly (one line per message, greeting
first, `key: value` data lines, one `OK`/`ERR ...` terminator) and the section
2.10 key set, writes no state, and owns no wallpaper. It is a test double for the
transport, not a second implementation of the daemon: it answers `hello`, `ping`,
`version`, `status`, `pause`, `resume`, `next` and `close` and refuses everything
else.

The numbers come from one harness, `<scratch>/measure/launch_measure.py`, which
for each run:

- records `t0`, execs the binary in its own process group, and reads its stdout
  until the prototype prints `READY` (printed the moment its tray item and window
  exist), so **cold start is wall time from exec to a live tray item**, measured
  outside the app;
- waits 60 s;
- records `ps -o rss= -p <pid>` and `ps -o %cpu= -p <pid>`;
- waits 5 s, records both again;
- records the sum over the process tree, and separately the WebKit XPC processes
  that appeared between `t0` and rest, then kills the whole process group.

Three runs each. The first run of each stack is cold in the page-cache sense; the
later runs are not, and the cold-start table gives the minimum and the median.

The three prototypes were run with the settings window open, so the three numbers
are the same shape: tray item plus one visible window.

## 3. The table

| | **Tauri 2.12.1** | **egui 0.36.2 + tray-icon 0.26** | **Swift 6.3.3 + AppKit** |
|---|---|---|---|
| Toolkits linked | `WebKit.framework` | `AppKit`, `OpenGL` (glow) | `AppKit` |
| Idle RSS, one process | 99,072 kB (96.8 MiB) | 113,856 kB (111.2 MiB) | **95,856 kB (93.6 MiB)** |
| Idle RSS, whole app | 180,528 kB (176.3 MiB) | 113,856 kB (111.2 MiB) | **95,856 kB (93.6 MiB)** |
| Binary / bundle on disk | 7,197,664 B (release, no `lto`/`strip`) | 6,584,160 B (`lto = "thin"`, `strip = true`) | **124 KiB, executable 116,688 B (`swiftc -O`)** |
| Cold start, min / median | 972 / 1031 ms | 543 / 961 ms | **230 / 291 ms** |
| Idle CPU at rest | 0.0 % | 0.0 % | 0.0 % |
| Build time, first release build | 442 s | 375 s | **25.5 s** |
| Unique crates in the tree | 292 | 157 | **0 third-party** |
| `LC_BUILD_VERSION minos` | 11.0 | 11.0 | 13.0 (builds at 11.0) |
| Can link `whirl-core` | In the Rust host only | **Yes, directly** | **No, not without a C-ABI bridge project** |
| Lines it must otherwise re-derive | protocol in JS, or a bridge | **0** | protocol + section 4.3 bounds, or none behind a C-ABI bridge |
| Hand-written lines, this spike | 205 over 4 files, 2 languages | 262 over 2 files | 216 over 2 files |

### 3.1 Idle resident memory

The command, pasted next to the value, for each of three runs:

```
$ python3 <scratch>/measure/launch_measure.py --label swift --settle 60 --runs 3 \
      --env WHIRL_SOCKET=<scratch>/prototypes/stub/whirl.sock -- \
      <scratch>/prototypes/swift/build/WhirlSpike.app/Contents/MacOS/WhirlSpike
run 1: cold=1391.4ms rss=95872kB total=95872kB cpu=0.0/0.0
run 2: cold= 230.0ms rss=95824kB total=95824kB cpu=0.0/0.0
run 3: cold= 290.9ms rss=95856kB total=95856kB cpu=0.0/6.7

$ ... --label egui ... -- <scratch>/prototypes/egui-tray/target/release/whirl-spike-egui
run 1: cold= 961.1ms rss=113856kB total=113856kB cpu=0.0/0.0
run 2: cold= 543.2ms rss=114864kB total=114864kB cpu=0.0/0.0
run 3: cold=1150.0ms rss=113392kB total=113392kB cpu=0.0/0.0

$ ... --label tauri ... -- <scratch>/prototypes/tauri/src-tauri/target/release/whirl-spike-tauri
run 1: cold=1030.5ms rss=99056kB total=180592kB cpu=0.0/0.0 wk_delta=81536kB
run 2: cold=1514.5ms rss=99072kB total=180112kB cpu=0.0/0.0 wk_delta=81040kB
run 3: cold= 972.0ms rss=99088kB total=180528kB cpu=0.0/0.0 wk_delta=81440kB
```

`total` is the named process plus its process-tree descendants plus the WebKit
XPC processes that appeared while it ran. The distinction is not cosmetic: a
`WKWebView` costs three processes that are **not** in the app's process tree,
because launchd owns them (`ppid` 1), so a tree walk alone reports 96.8 MiB for
Tauri and misses 79 MiB:

```
$ ps -Ao pid=,ppid=,rss=,comm= | grep -i webkit
88183     1  31888 .../com.apple.WebKit.GPU
88185     1  15968 .../com.apple.WebKit.Networking
88188     1  34976 .../com.apple.WebKit.WebContent
$ ps -o rss= -p 87787
   99168
```

That is 99,168 kB + 82,832 kB, a `ps` snapshot taken at a different moment than
the 180,528 kB (176.3 MiB) idle run in the table, and it is the number a user's
Activity Monitor shows when they sort by memory. The same measurement on the
other two candidates found no new WebKit process at all: both the egui run and
the Swift run report `webkit_new_processes: {}` and `rss_kb_tree: 0` for every
one of their three runs, so their `total` equals their single-process figure.

**This is the measurement that decides the framing of the whole comparison:** at
rest, with the settings window open, the three stacks are 93.6, 111.2 and
176.3 MiB, and the WebView is the entire reason Tauri is not the smallest.

### 3.2 Idle CPU at rest

```
$ ps -o %cpu= -p <pid>
0.0
$ ps -o %cpu= -p <pid>      # five seconds later
0.0
```

Every sample in every stack read `0.0` except one Swift sample at `+5s` (6.7),
which is one decaying-average sample after the window was ordered front, not a
steady state; the same run read `0.0` at the 60 s mark. No stack polls. The egui
spike does no periodic repaint either: the tray menu events arrive through
`muda`'s `MenuEvent::set_event_handler`, which calls
`egui::Context::request_repaint`, so an idle tray costs nothing. Section 8's
"Never poll" (item 9) is therefore not a differentiator here, but it is a
constraint the stack must be *able* to honour, and all three are.

Several rows below lean on ADR 0002, *Let a frontend write the config and own no
part of the daemon*. That ADR is **proposed and not merged**: it lives on branch
`docs/adr-0002-frontend` and this document was written against it at rev
`f0fc620`. If it is rejected or amended, section 3.8's weight and the reason
behind the recommendation both change with it.

One caveat, measured and not hidden: on the first sweep, one `egui` run had
already exited by the 60 s mark (`rss_kb_ps: 0`, `exit_code_before_kill: null`
because the kill found nothing to send to). The sweep was re-run and 3/3 runs
survived, and a separate 5-run soak with a 20 s settle had 5/5 survive. The
one-off is unexplained; it is recorded here rather than discarded.

### 3.3 Binary and bundle size

```
$ du -sk <scratch>/prototypes/swift/build/WhirlSpike.app
124	<scratch>/prototypes/swift/build/WhirlSpike.app
$ stat -f "%z" <scratch>/prototypes/swift/build/WhirlSpike.app/Contents/MacOS/WhirlSpike \
              <scratch>/prototypes/egui-tray/target/release/whirl-spike-egui \
              <scratch>/prototypes/tauri/src-tauri/target/release/whirl-spike-tauri
116688
6584160
7197664
```

A macOS frontend ships as a `.app` regardless of stack, so the Swift figure is
the whole bundle (executable plus `Info.plist`) and the two Rust figures are
executables that would gain a few hundred KiB of `Info.plist` and icons. The
Tauri binary does not carry a WebView: `otool -L` shows
`/System/Library/Frameworks/WebKit.framework/Versions/A/WebKit` as a system
framework, which is the point of the stack and also the reason its footprint is
paid at runtime rather than on disk.

The build directory is the less flattering half of the same fact:

```
$ du -sh <scratch>/prototypes/egui-tray/target <scratch>/prototypes/tauri/src-tauri/target
492M	<scratch>/prototypes/egui-tray/target
827M	<scratch>/prototypes/tauri/src-tauri/target
```

### 3.4 How each stack gets a menu bar item, and living without a Dock icon

| | Tauri | egui + tray-icon | Swift + AppKit |
|---|---|---|---|
| Tray API | `tauri::tray::TrayIconBuilder` + `tauri::menu::{Menu, MenuItem}` | `tray_icon::TrayIconBuilder` + `tray_icon::menu` (the `muda` crate re-exported) | `NSStatusBar.system.statusItem(withLength:)` + `NSMenu` |
| Event delivery | `on_menu_event` callback | `MenuEvent::set_event_handler` callback | `NSMenuItem` target/action |
| No Dock icon | `app.set_activation_policy(ActivationPolicy::Accessory)`, or `LSUIElement` in the bundle | **No API.** The spike needed one `objc2` message send: `NSApplication.setActivationPolicy:1`; `eframe` does not surface winit's macOS activation policy | `NSApp.setActivationPolicy(.accessory)` plus `LSUIElement` in `Info.plist` |

All three built a status item and a three-item menu without an error on this
machine. The one real difference is the middle row: the Rust-no-WebView stack is
the only one where the "no Dock icon" answer is not in the library's API and
needs an escape hatch into the ObjC runtime. It is one message send, and it is
not a reason to reject the stack, but it is the kind of seam that grows.

Whether the absence of a Dock tile is *visually* true could not be checked: this
session cannot capture the screen (section 6).

### 3.5 Reading the token from the platform store, and never showing it back

Every prototype performs the same check, and it is the check ADR 0002 decision 2
requires: **ask for attributes, never for data.**

| | Mechanism |
|---|---|
| Swift | `SecItemCopyMatching` with `kSecReturnAttributes: true` and no `kSecReturnData` |
| egui | `security_framework::item::ItemSearchOptions::load_attributes(true)` |
| Tauri | the same Security-framework call, behind a `#[tauri::command]` the WebView invokes |

`load_attributes` is the whole trick: the Security framework returns the item's
attributes and the process never receives the secret bytes, so a UI built this way
*cannot* print the value even by accident. All three queries ran and returned
"absent" on this machine; the Swift window render below shows the rendered line.
The value itself was never read, never echoed and never needed:

```
$ security help find-generic-password | head -4
Usage: find-generic-password [-h] [-a account] [-s service] [options...] [-g] [keychain...]
    -g  Display the password for the item found
    -w  Display only the password on stdout
$ security find-generic-password -s whirl-wallhaven
(no output, exit 44)
```

`-g` and `-w` are the only two ways that command prints a value, and 6.3 forbids
the write side of `-w` because it puts the token in `argv`. A frontend that
writes the item through the Security framework (`SecItemAdd`) and reads back only
attributes satisfies decision 2 in all three stacks.

**No key was written.** There is no `whirl-wallhaven` item on this machine, the
probes only ever searched read-only, and the "present" path is therefore code
that compiled and ran but was not exercised against a real item.

### 3.6 Registering a login item

| | macOS | Windows | Linux |
|---|---|---|---|
| Swift | `SMAppService.mainApp.register()` (macOS 13+) | not applicable, no Windows build exists | not applicable |
| egui (Rust) | `SMAppService` through `objc2`, or the `auto-launch` crate, which writes a `LaunchAgent` plist | `auto-launch` writes a `Run` key under `HKCU\...\CurrentVersion\Run` | `auto-launch` writes a `~/.config/autostart/*.desktop` |
| Tauri | `tauri-plugin-autostart`, which uses `SMAppService`/`LaunchAgent` on macOS | the same plugin, `Run` key | the same plugin, XDG `.desktop` |

The Rust and Tauri rows are documentation, not measurement: no login item was
installed and nothing was registered with the OS, so this table is the only claim
in this document that rests on documentation alone. Tauri's advantage here
is real and is the plugin: one API, three platforms, maintained alongside the
framework. The egui stack gets the same three mechanisms from `auto-launch`,
which is a smaller and less-tested crate.

Note the split ADR 0002 decision 3 draws: a frontend's *own* login item is its own
business, and the daemon's supervisor unit is the product's and is reported, not
owned. Nothing in this document changes that; the table is about the frontend's
own switch.

### 3.7 Minimum OS, and what it costs a user to install

```
$ vtool -show-build <binary>     # for each of the three
      platform MACOS
        minos 13.0               # Swift, because a login item needs SMAppService
        sdk 26.5
      platform MACOS
        minos 11.0               # egui/eframe + tray-icon
        sdk 26.5
      platform MACOS
        minos 11.0               # Tauri 2.12.1
        sdk 26.5
```

The Swift row is a choice, not a floor: the same source builds with
`-target arm64-apple-macos11.0` (verified, `built .../WhirlSpike.app`), and the
deployment target was raised to 13.0 only because `SMAppService` is 13+. The two
Rust rows are the SDK's own default floor.

Install cost per platform, stated as documentation and labelled as such, because
only macOS was measured here:

- **macOS:** every stack ships a bundle that has to be signed and notarised for a
  user to open it without the quarantine dialog. This cost is identical across the
  three and is not a differentiator. It is also why v0.1.0 ships unsigned
  (`docs/decisions/0001-macos-artifacts-ship-unsigned-in-v0.1.0.md`).
- **Windows:** Tauri needs WebView2, which is present on Windows 11 and on patched
  Windows 10 and is a redistributable otherwise; the two Rust-no-WebView stacks and
  the Swift stack depend on nothing. This is the per-platform cost of the WebView
  that Tauri's small binary hides.
- **Linux:** Tauri needs WebKitGTK 4.1 and libsoup 3 (`webkit2gtk-4.1`), which is a
  packaging dependency on every distribution the frontend wants to support; the
  egui stack needs X11 or Wayland client libraries and ships one static binary.
- **Windows and Linux for Swift:** there is no Swift build, so the Swift path is two
  additional implementations in two other languages, not a port.

### 3.8 Lines of code, and the second axis: `whirl-core`

Lines of hand-written code each spike needed for the same three marks, counted
with `wc -l`:

```
$ wc -l <scratch>/prototypes/swift/WhirlSpike/main.swift <scratch>/prototypes/swift/build.sh
     179 main.swift
      37 build.sh
$ wc -l <scratch>/prototypes/egui-tray/src/main.rs <scratch>/prototypes/egui-tray/Cargo.toml
     242 main.rs
      20 Cargo.toml
$ wc -l <scratch>/prototypes/tauri/src-tauri/src/main.rs <scratch>/prototypes/tauri/dist/index.html \
         <scratch>/prototypes/tauri/src-tauri/{build.rs,Cargo.toml,tauri.conf.json} \
         <scratch>/prototypes/tauri/src-tauri/capabilities/default.json
     104 src/main.rs
      48 dist/index.html
      53 build.rs + Cargo.toml + tauri.conf.json + capabilities/default.json
```

205, 262 and 216 lines, which at this size says more about the two languages than
about maintenance cost: the Tauri spike spreads the same three marks over four
files and two languages because the form lives in HTML and the tray lives in
Rust, and that split is the whole point of the stack. The number that does
discriminate is the next one.

`whirl-core` is the crate the daemon, the worker and the CLI all link, and it
holds the two parsers a frontend needs under ADR 0002 decision 1:

```
$ wc -l crates/whirl-core/src/protocol.rs crates/whirl-core/src/config.rs
    1644 crates/whirl-core/src/protocol.rs
    2714 crates/whirl-core/src/config.rs
```

A probe outside this repository, `<scratch>/prototypes/whirl-core-probe`, took
`whirl-core` as a git dependency and used it for both jobs:

```
$ cargo tree --manifest-path <scratch>/prototypes/whirl-core-probe/Cargo.toml | head -2
whirl-core-probe v0.1.0 (<scratch>/prototypes/whirl-core-probe)
└── whirl-core v0.1.0 (https://github.com/guruor/whirl.git?branch=development#6455a9c)

$ <scratch>/prototypes/whirl-core-probe/target/release/whirl-core-probe <scratch>/prototypes/stub/whirl.sock
protocol::parse_response saw 47 line(s), terminator Ok
  daemon_version: whirl 0.1.0
  protocol: 2
  platform: macos
config::parse refused interval_seconds=30: schedule.interval_seconds (line 1): 30 is less than 60
config::parse accepted interval_seconds=900
```

The git dependency resolved, the daemon's own `parse_response` read the socket's
response, and the daemon's own `Config::parse` enforced 4.3's bound
(`config.rs:1341`, "less than 60") with the field name and the rule.

That is the column that decides more than the memory numbers do:

| | Can consume `whirl-core` | Lines it must otherwise reimplement |
|---|---|---|
| **egui/eframe + tray-icon** | Yes, directly: `whirl-core = { git = "...", branch = "..." }` | **0**: protocol grammar and config bounds come from the crate the daemon links |
| **Tauri** | In the Rust host, yes; the settings form is JavaScript, so every write crosses a `#[tauri::command]` bridge or re-derives the rules in JS | the 4.3 bounds (and the framing, if the JS talks to the socket) |
| **Swift** | Not directly. A C-ABI bridge project (a `staticlib` wrapper around `whirl-core`) can call both parsers; without one it cannot link the crate at all | none through that bridge; without it, the 4,358 lines above are an upper bound, the full size of the two parsers `whirl-core` holds |

Mirrored rules drift. ADR 0002's own consequence section says so ("the UI mirror
4.3's ordering rules locally, and mirrored rules drift"), and the 4,358 lines
above are the upper bound of that mirror: the full size of the two parsers
`whirl-core` holds, not a measured re-derivation cost, because a frontend needs
the framing and 4.3's bounds and not all 4,358 of them. A Rust frontend links
those parsers instead of mirroring them, and a Tauri frontend can half-link them
and still has a JavaScript settings form whose values must be validated somewhere.
A Swift frontend has two honest paths: own a C-ABI bridge project, which reaches
both parsers and re-derives neither, at the cost of marshaling each value type
across the boundary; or take no link at all and carry the full mirror.

Note also what the git dependency did *not* need: `whirl-core` has no
dependencies (`crates/whirl-core/Cargo.toml` is `[dependencies]` and nothing
else), so linking it adds zero crates to a frontend's graph and pulls in no
toolkit. It compiled under this machine's rustc 1.98.1 without a change.

The two Rust spikes are deliberately different binaries, so that neither claim
borrows the other's evidence: the probe links `whirl-core` and is where this
section's numbers come from, while the `egui` spike behind section 3's memory and
start-up rows reaches the socket through about 25 hand-rolled lines and does not
link it. Nothing above measures one binary that does both. Linking `whirl-core`
adds a dependency-free rlib and no toolkit, so it does not move the numbers
section 3 reports; that is an argument, not a measurement.

## 4. What I did not test, and what these measurements do not settle

- **No real daemon.** Every socket number above is against the stub of section 2.
  That settles the framing, not the daemon's behaviour under a real subscription,
  a blocked `next`, or a `busy` refusal.
- **No synthetic click on a tray menu.** The status item was created and its menu
  built without error in all three stacks, and each stack's event path is wired,
  but no run drove a menu item end to end. Whether the selected item actually
  performs its verb is code that compiled and not behaviour that was observed.
- **No screenshot of a tray menu, and no screenshot at all from the screen.** This
  session has no display-capture permission:
  `screencapture -x <out>` returns `could not create image from display` and writes
  no file, on the only display. The one image that exists was rendered by the Swift
  prototype itself, in process, from its own view
  (`bitmapImageRepForCachingDisplay` / `cacheDisplay`), and shows the form field,
  the three `status` keys read off the socket, and the keychain presence line. The
  equivalent was not built for the other two stacks, and the tray menu cannot be
  rendered in process at all. The tray menu contents are therefore transcribed, not
  pictured: `Next`, `Pause`/`Resume`, a separator, `Quit` (`Settings…` is a fourth
  entry in the Swift spike).
- **Retina behaviour.** The window renders at 2x on this machine (the Swift render
  is a 840x360 bitmap of a 420x180 window), and nothing above tests how each stack
  scales on a mixed-DPI multi-display setup.
- **Accessibility.** No stack was driven by VoiceOver, and no accessibility-tree
  inspection was done. A WebView's accessibility is the platform's and is usually
  better than an immediate-mode GUI's; that is a hypothesis, not a measurement here.
- **Notarisation cost.** Not run. It is a per-release fee in time, not in bytes, and
  it is identical across the three stacks, so it does not enter the ordering.
- **Windows and Linux.** Nothing was run there. Every Windows and Linux row above
  is documentation, and the platform setters are stubs in v0.1.0 anyway
  (`docs/releases/v0.1.0.md`).
- **A real API key.** None was used, and no keychain item was written.
- **Long-run memory.** 60 s at rest is not a leak test. A tray app that grows while
  idle over eight hours would not show up in any row of the table.
- **The `egui` one-off exit** in section 3.2 is unexplained and is the one
  observation that argues for a longer soak before this stack is committed to.

## 5. Recommendation

**Build the reference frontend on Rust, one process, no WebView: `eframe`/`egui`
with the `tray-icon` crate, macOS first, with `whirl-core` as a git dependency.**

The numbers behind it: 111.2 MiB at rest against Tauri's 176.3 MiB and Swift's
93.6 MiB; a 6.3 MiB single binary; 961 ms median cold start against Tauri's
1031 ms and Swift's 291 ms; no WebKit XPC processes at rest at all; and, on the
axis this document did not start with, **0 lines of protocol or config code to
re-derive** because the frontend links the same `whirl-core` the daemon links,
measured by the probe above. On that axis Swift is neither at 0 nor simply at
4,358: it reaches both parsers through a C-ABI bridge project, which re-derives
nothing but is a project of its own with a marshaling layer per value type, and
without that bridge it faces the 4,358 lines, the upper bound of the two parsers
`whirl-core` holds. It loses to Swift on footprint (93.6 MiB against 111.2,
291 ms against 961) and that is the honest price of not being macOS-only: Swift's
93.6 MiB buys a second and a third implementation in two other languages, which
ADR 0002 decision 1 makes into a second and third copy of 4.3's bounds. The
bridge does not remove that cost: it is a macOS path, and there is no Swift build
at all for Windows or Linux (section 3.7), so those two platforms re-derive the
framing and the bounds whoever owns the macOS bridge.
Tauri is rejected by its own measurement: it is *not* the lightweight option the
WebView story implies, because the WebView is charged to the user at runtime, not
to the download, and 79 MiB of the 176.3 is three launchd-owned processes that
would not appear in the frontend's own accounting.

The sentence that would overturn it: **if a 60 s soak at rest does not hold
`egui`'s idle CPU at 0 % and its RSS flat on repeated runs, the one-off exit in
section 3.2 becomes a stability cost and Swift with AppKit, accepting two more
implementations, wins on the one axis this document was asked to measure first
(lightweight, low memory).** Two smaller sentences would move it the same way: if
Linux packaging of the Rust stack turns out to need a WebKitGTK that the egui
stack does not actually avoid, Tauri's bridge cost is no longer paid for anything;
and if `eframe` gains no activation-policy API and the `objc2` seam grows past the
one message send, the tray section's middle row stops being a footnote.

## 6. Reproducing

The measurement rests on two commands, and both are in section 3.1:

```
$ ps -o rss= -p <pid>          # after 60 s at rest
$ ps -o %cpu= -p <pid>         # after 60 s at rest
```

To get the same ordering, rebuild the three spikes with the versions in the
table header and run `<scratch>/measure/launch_measure.py` against each with
`--settle 60 --runs 3`. The dependency sets that matter:

- Swift: `swiftc -O -target arm64-apple-macos13.0 -framework AppKit -framework Security`, no third-party code;
- egui: `eframe 0.36` (default features off, `default_fonts`, `glow`, `wayland`, `x11`), `tray-icon 0.26`, `security-framework 3.7`, `objc2 0.6`, `[profile.release] lto = "thin"`, `strip = true`;
- Tauri: `tauri 2.12` with the `tray-icon` feature, `tauri-build 2.7`, `security-framework 3.7`, `frontendDist` a static `dist/index.html` with `withGlobalTauri`, `bundle.active: false`.

The raw harness output for every run cited above is the per-stack JSONL under
`<scratch>/results/`, and the probe in section 3.8 is
`<scratch>/prototypes/whirl-core-probe`.
