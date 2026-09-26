# whirl architecture: processes, protocol, and what each platform can actually do

Status: architecture of record for v0.1. Written 2026-09-25 on macOS 26.5.2 (build 25F84,
arm64) at `main` = `1261786`. Documentation only; no production code was written or changed.
Revision: where this document touches the `excl_file` lock it now names the second refusal as well
(1.5 step 1, 1.7.2, `lock_mode` in 2.10, failure mode 16), deferring to `[D 6 §8.8]` for the rule,
which `docs/spec/state-and-cache.md` owns.

This document answers three questions and leaves nothing open:

1. **What processes exist, which one is resident, and what does it cost?** Section 1.
2. **What exactly do they say to each other?** Section 2, complete enough to write a client
   in another language without asking a question, with a worked transcript.
3. **Is a lightweight daemon plus an optional lightweight UI possible everywhere, and does the
   OS scheduler suffice?** Sections 3 and 5.

It also fixes the config file (section 4), the security model (6), every failure mode (7), the
frontend contract (8) and the rules that keep the resident process small (9). Where this
document disagrees with `docs/spec/features.md` or `docs/spec/state-and-cache.md`, the
disagreement is stated in section 10, with the sentence it disagrees with.

## What this supersedes, and what it does not

- **Supersedes** the prototype's protocol (`prototype/whd/whd.rs`, `prototype/whd/whctl.rs`):
  same shape, versioned grammar, four deliberate differences (section 2.12).
- **Supersedes** `prototype/README.md`'s process model where the two disagree. Its measurements
  stand and are cited as `[M n]`; two of its claims about macOS are already corrected by
  `docs/research/macos.md`, and one number turns out to be un-reproducible from the committed
  tree (section 1.4, `[L 5]`).
- **Defers to** `docs/spec/state-and-cache.md` for every path, file format, eviction rule and
  lock: this document does not restate them, it names which process acts on them.
- **Defers to** `docs/spec/features.md` for the feature set and the config schema.
- **Defers to** `docs/research/{macos,windows,linux,scheduling}.md` for everything about the
  platforms. Every platform claim below is theirs; where I needed a number they do not have, it
  is marked as unverified here rather than asserted.

## Reading conventions

The same four labels the sibling documents use, plus one:

- `[M n]` a measurement or a read constant from the prototype, listed in section 11 with its
  value and its file:line. Where a measurement came from the prototype's own README and the code
  that produced it is not committed, section 11 says so.
- `[D n §s]` a finding in a sibling document: `[D 1]` macOS, `[D 2]` Windows, `[D 3]` Linux,
  `[D 4]` scheduling, `[D 5]` features spec, `[D 6]` state-and-cache spec. `[D 1 Vn]` points at
  a row of that document's own on-machine table.
- `[L n]` something run on this machine while writing this document, listed in section 11.
- `decision:` a call this document owns, with its basis named in the same row or sentence.
- `unverified:` a claim that comes from documentation only, or from a platform with no host
  here. Every one of these appears in section 3.7 with what would settle it.

## Decisions at a glance

Every row is a decision and its basis. A row whose basis column is empty would be a defect.

| # | Decision | Basis |
|---|---|---|
| P1 | Three processes: a resident daemon, a per-rotation worker, a thin CLI. The daemon never decodes, never links a platform backend, never links a GUI toolkit. | `[M 1]` 1.8 -> 2.3 MB delegated vs `[M 2]` 12 -> 140 MB in-process; `[M 3]` is the README's own "under 3 MB if it delegates the image work", and `[L 3]` built the daemon at 727,072 bytes with no dependencies while the worker is the crate built with `CGO_ENABLED=1` (`prototype/README.md:97`), which is where the platform linkage lands |
| P2 | The worker is spawned per rotation and exits; its peak is 21-25 MB for 1.2-3.4 s. | `[M 4]` |
| P3 | A hung worker is killed at 300 s (TERM, 5 s, KILL), never waited on forever. | Derived from `[M 4]` and the worker's own 2-minute HTTP timeout, `[M 5]` |
| P4 | The daemon does not block the control plane on the worker; a second rotation is refused, not queued. | `[D 6 §7.3]`; and `[L 4]` shows what unbounded per-connection growth costs |
| P5 | The daemon is started by the OS supervisor, never by the CLI and never by the worker. | `[D 5 §1.1]` (`daemon start`, `daemon stop` and `daemon restart` are non-goals), `[D 4 §Part 3]` |
| Pr1 | The CLI's language over the socket is a line protocol, one request per line, `key: value` data lines, one `OK` or one `ERR <code>` terminator. | The prototype's shape, kept; `[L 3]` is the captured exchange |
| Pr2 | Failures are `ERR <code> <message>` with a closed code set, not the prototype's free-text `ACK`. | `[L 3]`: the prototype answers an unknown verb with `ACK unknown command 'frobnicate'`, which a client cannot branch on |
| Pr3 | Every data line is prefixed, so an empty list is distinguishable from a missing key. | `[L 3]`: the prototype's empty `history` and `favorites` both return a bare `OK`; `last: ` is an empty value with a trailing space |
| Pr4 | Transport is a unix socket, mode `0600`, bound in a directory the daemon creates `0700`; a named pipe on Windows whose DACL grants the owning user only. | `[M 6]` the prototype's socket is `0600` as intended; `[L 3]` a socket bound without a `chmod` is `0755` at umask `0022`, so the window between `bind` and `chmod` is real |
| Pr5 | Protocol version 2, negotiated by the greeting and an optional `hello`. | The grammar below differs from the prototype's in four ways (2.12), so numbering it 1 would claim a compatibility that does not exist |
| Pr6 | `subscribe` is a streaming mode with a monotonic `seq` and a 30-second heartbeat. | `[D 6 §7.3]` one notification per state transition; the prototype wakes twice per rotation; the heartbeat derives from the prototype's own 60 s idle wait, `[L 3]` |
| Pr7 | 16 concurrent connections, then `ERR busy`. | `[L 4]`: 128 unbounded connections moved the prototype's RSS from 1.8 to 5.1 MB |
| C1 | Config stays JSON, with `_`-prefixed keys as the comment convention. | `[M 7]` the prototype's config was parsed by two std-only parsers; the daemon's parser stays dependency-free, and Windows paths make the full JSON escape set mandatory, `[D 2 §2]` |
| C2 | Paths per platform are the ones `[D 6 §1]` fixes; no new roots are invented. | `[D 6 §1.1]` to `[D 6 §1.3]` |
| S1 | The OS scheduler owns the process; the daemon owns the clock. Identical on all three platforms. No OS timer is given the rotation schedule. | `[D 4 §Part 3]`, adopted verbatim |
| S2 | The daemon's deadline is a wall-clock comparison against a persisted `next_at`, advanced by whole intervals. | `[D 4 §Part 2]` (launchd `StartInterval` loses firings across sleep), and the prototype's re-anchoring bug, `[D 4 §Part 2]` drift row |
| X1 | On macOS the agent must be a per-user `LaunchAgent` in the Aqua session; a daemon is impossible. | `[D 1 §9]` measured; TN2083 quoted in `[D 1 §9]` |
| X2 | On Windows it is a per-user interactive process, never a service. | `[D 2 §4]`: session 0 isolation, error 1459 |
| X3 | On Linux, GNOME/KDE/sway/X11 need no resident helper at all; Hyprland requires `hyprpaper`, which whirl does not own and does not supervise. | `[D 3 §The decisive column]`, `[D 3 §The resident helper]` |
| X4 | Per-Space wallpaper on macOS, per-virtual-desktop wallpaper on Windows, and per-monitor wallpaper on GNOME are platform limitations, stated as such, not worked around. | `[D 1 §2]`, `[D 2 §3]`, `[D 3 §GNOME 2]` |
| X5 | `display.mode = per-display` is honoured only where the platform documents a per-display setter that a one-rotation worker can reach; elsewhere it is refused with a named reason. | `[D 5 §1.3]` is the fallback rule this implements; `[D 2 §1]` documented, `[D 1 §2]` unverified, `[D 3 §GNOME 2]` impossible, `[D 3 §KDE 2]` out of scope |
| R1 | The resident process owns state and never pixels. | `[M 1]` vs `[M 2]` |
| R2 | It never embeds a GUI toolkit. | `[M 8]` 883 MB for a Go/Fyne app with one immortal process owning toolkit, index and pipeline |
| R3 | It holds no unbounded index: history 50 entries, cache capped on bytes and count, 500 entries = 170,607 bytes. | `[D 6 §2.1 L 5]`, `[D 6 §5.1]` |
| R4 | It grows state only under a cap, including threads and connections. | `[L 4]` 128 connections = +3.3 MB; `[D 6 §5.1]` the count cap does not bound bytes (60x spread at best) |
| R5 | One writer per file, and every state write is temp-file + fsync + rename. | `[D 6 §6.3 L 5]`: 3068 of 3508 concurrent reads saw a truncated file with an in-place rewrite |

## 1. Process model

### 1.1 The three processes

| Process | Resident? | Lifetime | Owns | Started by |
|---|---|---|---|---|
| `whirld` (daemon) | **yes, one per user session** | login to logout (or to an explicit stop) | the config's scalars, the schedule and its persisted deadline, `state/current.json`, `state/history.json`, `state/favorites.json`, `cache/index.json`, the control socket, the log, `state/locks/daemon.lock` and (only for a sweep) `state/locks/rotate.lock`, and the worker's process handle | the OS supervisor only: a `LaunchAgent` on macOS, a per-user logon task on Windows, a `systemd --user` service on Linux (section 5) |
| `whirl-worker` | **no**, one per rotation | one rotation: measured 1.2-3.4 s `[M 4]` | the network fetch, the header sniff, its part file under `cache/tmp/`, the `rename` into `cache/sha256/`, the platform setter call, and `rotate.lock` for its own lifetime | the daemon, one `fork`/`exec`-equivalent with a fixed argv and a scrubbed environment (1.6) |
| `whirl` (CLI) | **no** | one verb | nothing. It is a protocol client and a formatter | the user or a script |
| a frontend (TUI, menu bar item, script) | no, optionally | as long as the user keeps it | nothing but its own view. It is the same kind of process as `whirl` (section 8) | the user |

There is no fourth process on any platform. In particular whirl never runs a compositor helper,
never runs `swaybg`, never starts or restarts `hyprpaper`, and never runs a privileged helper:
`[D 3 §The resident helper]` is the source of the first two, and `[D 2 §4]` (session 0, error 1459)
plus `[D 1 §9]` (a pre-login daemon cannot touch the wallpaper) are why a privileged helper would
buy nothing anyway.

### 1.2 Repo layout implied by this document

`docs/development.md` owns the final tree; this is the part the architecture fixes, because the
process split is what the crates are:

```
crates/
  whirl-core/     protocol types and their codec, config schema and validation, state records,
                  the source trait, the error-code enum. No I/O, no platform code.
  whirld/         the daemon binary: socket, scheduler, state, index, sweep, worker supervision.
  whirl-worker/   the worker binary: sources, filter pipeline, cache writes, platform setters.
                  This is the only crate that links a platform backend.
  whirl-cli/      the CLI binary: a protocol client, verb parsing, output formatting.
```

Binaries: `whirld`, `whirl-worker`, `whirl`. `[L 8]` confirms no binary with those names exists
in `PATH` on this machine. The prototype's `whd`/`whctl`/`wh-rotate` names are retired with the
prototype. The reverse-DNS form is used where a platform demands one (`com.guruor.whirl` as the
launchd label, `[D 4 §Part 3]`), and the plain product name is used for paths, per
`[D 6 §1.1]`.

The platform backend lives only in `whirl-worker`: macOS AppKit via `cgo` (`[D 1 §3]`), Windows
`IDesktopWallpaper` COM (`[D 2 §1]`), Linux per-environment adapters (`[D 3 §The decisive
column]`). That is not tidiness, it is the measurement `[M 3]`: the prototype's worker binary is
8.4 MB and the daemon is 0.69 MB, and the difference is the platform linkage. The daemon must
never link it.

### 1.3 What the resident process costs

| | resident | source |
|---|---|---|
| `whirld`, idle | 1.8 MB | `[M 1]`, reproduced as 1.7-1.8 MB in `[L 3]` |
| `whirld`, after 7 rotations | 2.3 MB, flat | `[M 1]` |
| `whirld`, 128 concurrent clients | 5.1 MB, and rising with the count | `[L 4]`, the unbounded case this design forbids |
| `whirl-worker`, one rotation | 21-25 MB peak, 1.2-3.4 s, then gone | `[M 4]` |
| the same work in-process, for contrast | 12 MB -> 140 MB, never returns | `[M 2]`, `[M 9]` |
| `whirl` (CLI) | 0 MB between invocations | `[M 3]` |

The daemon's own state is bounded by construction: a 50-entry history ring `[D 6 §6.2]`, a
favorites list bounded by the user's own clicks `[D 5 §1.2]`, and a cache index of at most 500
entries at 341 bytes each = 170,607 bytes `[D 6 §2.1 L 5]`. The prototype's ring was 64 entries
`[M 11]`; the difference is the spec's number, not a new one.

### 1.4 The rule, and the measurement behind it

> **Rule D1. The resident process never owns pixels. Every operation that touches image bytes
> runs in a process that exits when the operation ends.**

Basis, both rows from the same daemon, one flag apart `[M 2]`:

```
delegated,  12 requests:  idle 1.8 MB -> 2.3 MB after 7 rotations, flat
in-process, 12 requests:  idle 12.0 MB -> 24.7 -> 46.1 -> 46.2 -> 140.1 -> 140.3 MB, permanently
```

`[M 9]` is the mechanism and it is the reason a "free it after use" fix does not exist: the floor
is set by the largest image ever decoded, not by the average, and the in-process variant called
`runtime.GC()` and `debug.FreeOSMemory()` after every decode and still ratcheted. A 4672x7008
image is 124.9 MB of RGBA `[M 9]`, so the floor moves to 140 MB and stays there for rotations of
10 MB images. The same shape at the product level is the 883 MB app `[M 8]`: one immortal process
owning the toolkit, the index, the scheduler and the pipeline, so every feature became a
permanent memory floor.

**Provenance caveat, stated because it is checkable and a maintainer will hit it.** The
in-process row `[M 2]` and the trace `[M 9]` are recorded in `prototype/README.md`, but the
artifact that produced them is not in this tree: `prototype/whd/measure.py` drives an HTTP daemon
at `http://127.0.0.1:8791` and reads `inproc` and `goroutines` fields (`measure.py:12,43,52`),
while the committed `whd.rs` speaks the unix-socket line protocol and has no decode path and no
such fields (`[L 5]`). So the delegated half is reproducible and was reproduced here (1.7-1.8 MB
idle, `[L 3]`), and the in-process half is a recorded measurement of a variant that is not
committed. What would settle it: re-add the in-process mode behind a debug flag on the Rust
daemon and re-run `measure.py`-equivalent sampling. I use the number as the card requires and I
do not claim to have re-run it.

### 1.5 Lifecycle

**Daemon.** Started at login by the supervisor and kept alive by it (macOS `KeepAlive`, Windows
`RestartOnFailure`, Linux `Restart=always`; section 5). It never starts itself, and no client can
start it: `whirl` reports an unreachable daemon and exits 2 `[M 13]` rather than spawning one, and
`daemon start|stop|restart` stays out of the verb set for the reason `[D 5 §1.1]` gives, that a
second supervisor competes with the first.

**Startup order inside the daemon** (order matters, and it is the sequence
`[D 6 §5.5]`, `[D 6 §7.2]` and `[D 6 §8.5]` add up to):

1. Take `state/locks/daemon.lock` exclusively and non-blocking. Held, exit with the holder's pid
   if not. A lock file the daemon did not create is refused as well, never taken over: under the
   `excl_file` fallback the message classifies the recorded holder (pid plus the platform's start
   time for it) and names the one action that clears it `[D 6 §8.8]`. `[D 6 §7.2]`.
2. Open the state directory and validate that it is writable; refuse to start if it is not, with
   the directory and the `errno` in the message `[D 6 §8.5]`.
3. Parse and validate the config. A config that fails validation is a refusal to start, naming
   the key `[D 6 §8.7]`.
4. Load `favorites.json`, `history.json`, `current.json`; quarantine and degrade per
   `[D 6 §6.4]` if one is unreadable.
5. Bind the control socket (2.1). A second daemon is already excluded by step 1, so a live
   socket at this path is either ours (impossible) or stale; unlink it only after a connect probe
   returns `ECONNREFUSED`/`ENOENT`, never unconditionally, which is the prototype's bug `[M 15]`.
6. Read back the platform's current image where the platform can report it and reconcile the
   anchor (1.7.3). Do not set anything yet.
7. Run the startup sweep and the startup rotation, both off the accept loop, so the socket is
   answering while they run.

**Worker.** Spawned when a rotation is due, when a client asks for `next`/`prev`/`set path`/
`set id`, or when a client asks for `config check`. One worker at a time, enforced by
`rotate.lock` `[D 6 §7.3]`. It is never spawned by a client, and never by another worker.

**CLI and frontends.** One process, one connection, one or more verbs, exit. `[M 3]` measures the
CLI at 0 MB between invocations because it does exactly that.

### 1.6 The worker contract

The daemon spawns the worker as a child process with a fixed shape, so that a maintainer reading
either side can see the whole interface:

- **argv:** `whirl-worker --config <abs path> --verb <rotate|set|check> [--target <path|id>]`
  --run <rotation id>`. The rotation id is the daemon's monotonic slot counter, used to build the
  part-file name `tmp/<run>-<rand>.part` `[D 6 §3]`.
- **environment: scrubbed, then explicitly set.** The prototype does the opposite: `whd.rs:114`
  calls `env()` on `Command` without `env_clear()`, so the worker inherits the daemon's entire
  environment `[M 16]`. `decision:` the worker gets `env_clear()` plus exactly this list, because
  a per-rotation process should not carry whatever the session had, and because the Linux
  adapters need named session variables rather than the whole environment:
  `PATH`, `HOME`, `WHIRL_CONFIG`, `WHIRL_BACKEND`, `WHIRL_WALLHAVEN_API_KEY` (only when set in the
  daemon's own environment, which is one of the three places a key may come from
  `[D 5 §2.4]`), `WHIRL_CACHE_DIR` and `WHIRL_STATE_DIR` (always, set to the directories this daemon
  resolved whether or not its own environment named them), and, on Linux only, nine variables: the
  four signals
  `[D 3 §Detecting the environment]` names as decisive (`XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE`,
  `SWAYSOCK` with `I3SOCK`, `HYPRLAND_INSTANCE_SIGNATURE` - five variables, because sway and i3
  share a row), plus four more that a session bus or a display connection needs and that the same
  section does not call decisive (`XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`, `WAYLAND_DISPLAY`,
  `DISPLAY`). The four are the ones the section calls the signals "a compositor sets for its own
  children"; the other four are what talking to that compositor requires, and the document cites
  them as such rather than as detection signals. Without them the worker cannot tell a GNOME
  session from a KDE one, and `[D 3 §Detecting the environment]` is explicit that `gsettings` being
  on `PATH` is not a GNOME signal.
- **The two path knobs reach the worker as this daemon's resolved directories.** `WHIRL_CACHE_DIR`
  and `WHIRL_STATE_DIR` are on the list because 4.3 puts the environment ahead of the file for the
  cache root and the state directory, and the worker is the process that writes `sha256/**` under
  the cache root and builds the recent window of 4.1 out of the state directory. A scrub
  that dropped them would leave the two processes resolving one knob two ways - the daemon reporting
  the directory the environment named, because it read it, and the worker writing into the compiled
  default, because it never saw it - while `status` reported the daemon's answer as the effective
  one. The daemon therefore sets both names to the directories it resolved whether or not its own
  environment named them, because the platform default is itself chosen by variables this list does
  not carry (`$XDG_STATE_HOME` and `$XDG_CACHE_HOME` on Linux, `%LOCALAPPDATA%` on Windows;
  `[D 6 §1.2]`, `[D 6 §1.3]`), and a child left to re-derive them reads a different `history.json`
  than the daemon wrote and gets a 4.1 window that is silently empty.
- **stdout:** at most two lines. `downloaded: <digest> <abs path>` after the rename, and
  `set: <digest> <origin_key> <abs path>` after the setter returned success. The daemon parses the
  last non-empty line as the result and keeps the whole capture for the log
  `[D 6 §7.2]` (the worker does not open the log file). Two lines rather than one because of
  1.7.3: a worker that dies between the setter and its exit has still told the daemon what is on
  screen.
- **exit code:** 0 when the verb completed and the wallpaper was set (or, for `check`, when the
  plan was produced); non-zero with the failing stage on stderr otherwise. The `ERR` code the
  client sees is derived from the failing stage, and the mapping is in 2.7.

### 1.7 What happens when the worker hangs, crashes, or is killed mid-rotation

Three cases, one mechanism each. None of them can leave the daemon blocked, and none of them can
leave the wallpaper in a state whirl does not know about.

**1.7.1 It hangs.** The daemon waits for the worker with a deadline of
`schedule.worker_deadline_seconds` (default **300**), then sends `SIGTERM`, waits **5 s**, then
`SIGKILL`. Basis for 300: the worker's own HTTP client timeout is 2 minutes `[M 5]`, a healthy
rotation is 1.2-3.4 s `[M 4]`, and the cache prune is bounded by the cache caps, so 300 s is
about 90 times the measured worst case and still covers a fully-expired download plus a setter
retry. Basis for 5 s: a worker that has not exited 5 s after `SIGTERM` has already exceeded its
own measured p100 by 47% `[M 4]`. Windows has no `SIGTERM`; the worker is created inside a Job
Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, which also guarantees no orphan worker survives
a daemon crash (unverified: no Windows host, section 3.7). The prototype has no deadline at all:
`whd.rs:120` blocks on `c.output()` forever, so a hung worker leaves `rotating = true` and the
scheduler never rotates again `[M 14]`. That is the bug this deadline exists for.

After the kill the daemon writes `last_error: worker_timeout` (surfaced as `ERR timeout` to the
client that asked), arms the next slot normally, and does not retry inside the slot.

**1.7.2 It crashes or is killed mid-rotation.** Nothing was written to state, because the worker
writes no state `[D 6 §7.1]`. What is left is a part file under `cache/tmp/` and, if the crash
happened after the rename, a cache file with no index entry. Both are reclaimed by the next sweep
(`[D 6 §5.5]` steps 3 and 4) after `cache.orphan_grace_seconds` (300) and
`cache.grace_seconds` (600) respectively. `rotate.lock` needs no recovery logic where the kernel
owns the lock: the kernel releases an `flock` when the holder exits, including on `SIGKILL`
`[D 6 §7.2]`. Where the `excl_file` fallback of `[D 6 §8.7]` is in force there is no kernel
release, and the daemon removes the lock file of the worker it has just reaped `[D 6 §8.8]`. The
daemon logs one line, sets `last_error: worker_failed`, and the slot is consumed.

**1.7.3 It dies between a successful setter call and its exit.** This is the interesting one and
the reason for the two-line stdout contract. The wallpaper has changed and the daemon was not
told. `decision:` on any worker failure that produced a `downloaded:` line but no `set:` line, the
daemon **adopts the display from the platform rather than from its own record**:

1. Read the current image back where the platform can report it: `desktopImageURLForScreen:` on
   macOS `[D 1 §3]`, `GetWallpaper` per monitor on Windows `[D 2 §1]`, the desktop environment's
   own key on Linux where the adapter can read one `[D 3 §GNOME 1]`.
2. If it matches the file the worker reported downloading, that file becomes the anchor and the
   rotation counts as a success with `via: recovered`.
3. If it does not match, or the platform cannot report it (true for parts of Linux
   `[D 5 §1.5]`), the daemon sets `anchor_verified: 0` and the next sweep protects **every** file
   under `sha256/` created inside `cache.grace_seconds`, reporting `anchor_unverified: 1`, until a
   completed rotation re-establishes the anchor.

Why this is not optional: `[D 6 §5.3]` protects the anchor because a wallpaper whose file
disappears is a broken wallpaper, measured on macOS as a write to a deleted path returning `NO`
with `The file doesn't exist.` `[D 1 V5c]`. Without step 1 or 3, the one file that is on screen
and not in the index is exactly the file the sweep would reclaim as an orphan.

### 1.8 The control plane never blocks on a worker

- The accept loop never spawns a worker. Clients hand work to the daemon's single rotation
  worker thread and wait for the reply on their own connection `[L 3]` is the prototype's shape
  and it is the right one.
- A second rotation request while one is in flight is refused with `ERR busy`, not queued
  `[D 6 §7.3]`. The prototype's queue is a channel with no bound; the spec's rule is the one that
  survives a laptop waking from sleep with a dark tunnel's worth of missed slots.
- The daemon holds `rotate.lock` only for a sweep, never across a worker's lifetime
  `[D 6 §7.2]`, so a sweep and a download cannot deadlock.
- Connections are bounded: 16 concurrent, then `ERR busy` and close. `[L 4]` is the reason it is a
  number and not a comment: with no bound, the prototype's RSS went 1.8 -> 2.0 -> 2.7 -> 3.5 ->
  5.1 MB at 1, 8, 32, 64 and 128 held-open connections, and fds went 7 -> 263. Any local process
  as the same user can open connections, so an unbounded accept loop is an unbounded growth path
  in the one process the whole design exists to keep small `[R4]`.

### 1.9 What the daemon deliberately does not own

Not a list of omissions, a list of refusals, each with the measurement or finding behind it:

| Not owned | Why |
|---|---|
| Image bytes, decoders, thumbnails, colour extraction | `[M 2]` the 140 MB ratchet |
| A GUI toolkit, a window, a tray icon | `[M 8]` 883 MB; `[D 5 §Part 3]` names a tray icon as a non-goal |
| A second supervisor for itself or for a compositor helper | `[D 5 §1.1]`, `[D 3 §The resident helper]` |
| A watcher for display changes or Space switches | `[D 5 §1.3]` a watcher is resident code; macOS per-Space targeting is impossible anyway `[D 1 §2]` |
| A cache verification pass | `[D 6 §9]` no verb budget for it |
| The rotation schedule as an OS timer | `[D 4 §Part 3]`; section 5 |
| The Wallhaven API key | `[D 5 §2.4]`; section 6 |

## 2. Protocol

This section is written to be implemented from without looking at the prototype. Section 2.11 is
a worked transcript of a real session; section 2.12 lists where this deliberately differs from
`prototype/whd/whctl.rs`.

### 2.1 Transport

- **macOS and Linux: a unix domain socket, `SOCK_STREAM`, mode `0600`.** Default path per
  `[D 6 §1.1]` and `[D 6 §1.2]`: `~/Library/Application Support/whirl/whirl.sock` on macOS
  (69 bytes for this user's home, `[L 7]`), `$XDG_RUNTIME_DIR/whirl.sock` on Linux (24 bytes,
  `[L 7]`), falling back to `$XDG_STATE_HOME/whirl/whirl.sock` when `XDG_RUNTIME_DIR` is unset.
  The parent directory is created `0700` if it does not exist.
- **Windows: a named pipe**, `\\.\pipe\whirl-<user>` with the same sanitisation and 32-character
  truncation `[D 6 §1.3]` specifies, created with a security descriptor whose only ACE grants
  `GENERIC_READ | GENERIC_WRITE` to the owning user's SID. `[D 6 §1.3]` marks the ACL as an open
  item because no Windows host was available; this document fixes the intended DACL so the
  implementation has one answer, and leaves it `unverified:` for the real-Windows checklist.
- **No TCP, ever.** Remote control and multi-user access are non-goals for a reason
  `[D 5 §Part 3]`; a listener on a network interface would be the only path by which another
  account reaches the daemon, and not having one is cheaper than authenticating one.

**Permissions, and the window between `bind` and `chmod`.** `[L 3]` measured both halves of this:
the prototype's socket is `srw-------` (`0600`) as intended, and a unix socket bound with no
`chmod` at this machine's umask `0022` is `srwxr-xr-x` (`0755`). Group and other *may connect*
in the interval between `bind()` and `set_permissions()`. `decision:` whirl sets the process umask
to `0o077` around the `bind` call and then `fchmod`s the socket to `0600`, so there is no interval
in which a connection is possible without the user's identity. The Windows pipe gets its DACL in
the `CreateNamedPipe` call itself, so the equivalent interval does not exist there.

**Path length.** `sun_path` is 104 bytes on this machine `[L 1]`, and the macOS default socket
path is already 69 of them `[L 7]`, so a longer user name or home directory can exceed the limit.
`decision:` the daemon queries the platform's `sun_path` size at runtime, compares it to the
resolved path, and if the path does not fit, refuses to start with an error naming both lengths
and the two ways to change the path (`socket` in the config, or `WHIRL_SOCKET`). Hardcoding 104 or
108 is how this becomes a bug report from someone with a long home directory.

### 2.2 Framing

- **UTF-8 text, one message per line, terminated by a single `\n` (`0x0A`).** A `\r` before the
  `\n` is tolerated on input and never emitted on output, because a client written on Windows may
  be built by someone who assumes CRLF.
- **A request line is at most 8192 bytes, excluding the `\n`.** Basis: the longest legal request
  is `set path <path>`, and a path can be as long as the platform's path limit, 1024 bytes on
  this machine `[L 2]`, and 4096 on Linux (`PATH_MAX` in `linux/limits.h`, not read on a Linux
  host here: `unverified:`). 8192 covers the platform limit plus the verb, the digest and the
  `set:` prefix with room to spare, and bounds what the daemon must buffer per connection. A
  longer line is answered with `ERR too_long` and the connection is closed, without reading the
  rest of the line.
- **Byte order is not a wire concern.** Byte `0x0A` cannot appear inside a UTF-8 multi-byte
  sequence, so a line-oriented reader can never split a character.
- **A NUL byte anywhere in a request line is rejected with `ERR bad_framing`** and closes the
  connection. Nothing in the grammar contains one, and accepting it only makes logging ambiguous.
- **Field separator is a single ASCII space.** Runs of spaces are not collapsed, so an empty
  field is legal where the grammar allows one.
- **The last field of a request or a positional response record takes the rest of the line,
  spaces included.** This is what lets a path contain spaces without quoting, escape sequences or
  a second parsing layer, which the Windows and macOS paths in `[D 6 §1]` make mandatory
  (`~/Library/Application Support/whirl/...`, `%LOCALAPPDATA%\whirl\...`).
- **Identifiers contain no spaces.** `decision:` a source `id` matches `[A-Za-z0-9._:-]+` and is
  at most 64 bytes, validated in the config with a named error. Reason: `id` appears as a
  non-final field in `source:` and `entry:` records, and an id with a space would make the
  framing ambiguous. This is a constraint this document adds to `[D 5 §2.1]`, which says only
  "stable name"; it is checked at `whirl config check` and at load.

### 2.3 Connection lifetime

1. On `accept`, the daemon immediately writes the greeting (2.4) and flushes it. A client must
   read the greeting before it sends anything; this is the prototype's contract too `[L 3]`.
2. The client sends request lines. The daemon answers each one before reading the next, so
   responses are in request order. Pipelining is therefore legal: a client may write several
   request lines at once and read the responses in order.
3. A response is complete when its terminator line (`OK`, `ERR ...`) has been read. Whatever is
   on the socket after that belongs to the next request.
4. The connection ends when: the client sends `close` (answered `OK`, then closed); the client
   half-closes or dies; a framing error occurs (`too_long`, `bad_framing`); a protocol error
   occurs (`bad_protocol`); the daemon shuts down (a `shutdown` event first if the client is
   subscribed, then close); or `connection_idle_timeout` expires (2.8).
5. **`subscribe` takes over the connection.** After `subscribe` has been accepted, the only
   legal request is `close`. A subscription cannot be multiplexed with commands, because
   interleaving two response streams on one socket is exactly the ambiguity the framing rules
   exist to prevent. A client that needs both opens two connections.
6. **`ERR` does not end the connection** except for the four codes marked "closes" in 2.7. A
   client can therefore issue the next verb after a refused one, which is what makes a REPL or a
   `netcat` session usable.
7. At most 16 concurrent connections (1.8); the 17th is accepted, answered
   `ERR busy too many clients (16)`, and closed immediately. This bounds the resident process's
   stack and fd footprint `[L 4]` and is checked before the greeting only for the refusal path:
   an accepted connection always gets a greeting or the busy error, never silence.

### 2.4 Version negotiation

The greeting is the version announcement and it is mandatory:

```
<-- OK whirl <semver> protocol <n>
```

`n` is an integer. This document defines `n = 2`. `<semver>` is the **bare** version, `0.1.0`: the
product name appears exactly once, in the leading `whirl`, so the greeting is
`OK whirl 0.1.0 protocol 2`. The two-token form `whirl 0.1.0` is what the `daemon_version` status
key carries (2.10); it is never a token of the greeting, and a client can split the greeting on
spaces and read the version from the third field. A client may then send:

```
--> hello <n> [<client-name>/<client-version>]
<-- protocol: <m>
<-- OK
```

Rules:

- `m = min(n, server_max)`. With one version in existence, `n = 2` gives `m = 2`.
- A client that sends no `hello` is treated as agreeing to the version in the greeting. That is
  the whole compatibility story for a shell script that just opens the socket and sends `status`.
- `hello 0`, `hello -1` or `hello abc` is answered `ERR bad_protocol <message>` and the
  connection is closed. So is `hello 3` once `server_max` is 2: a v3 client must not be allowed to
  guess what changed.
- `decision:` the version number changes when the grammar changes in any way a v2 client could
  misparse, including adding a required field, changing a data-line prefix, or moving a
  terminator. Adding a new `key:` to `status`, a new event type to `subscribe`, or a new source
  kind is **not** a version bump, and clients must tolerate unknown keys and unknown events
  (section 8). This rule is what keeps the version number meaningful: it is a claim about
  parseability, not about feature sets.

### 2.5 Request grammar

Requests are the verbs of `[D 5 §1.1]`, plus the four protocol-level verbs the prototype
established (`ping`, `version`, `subscribe`, `close`). Case-sensitive, lower-case. The first token
is the verb; four verbs take a mandatory literal second token (`set path`, `set id`, `config
path`, `config check`).

| Request | Argument | Blocks? | Success data lines | Error codes (2.7) |
|---|---|---|---|---|
| `hello <n> [<name>/<ver>]` | version, optional client id | no | `protocol: <m>` | `bad_protocol`* |
| `ping` | none | no | none | - |
| `version` | none | no | `daemon_version:`, `protocol:`, `platform:` | - |
| `status` | none | no | the stable block (2.10) | - |
| `next` | none | yes, up to 300 s | `queued`, then `set: <digest> <origin_key> <via> <path>` | `busy`, `timeout`, `worker_failed`, `no_candidates`, `offline`, `too_large`, `not_an_image`, `set_failed`, `enospc`, `cache_readonly`, `cache_unwritable`, `internal` |
| `prev` | none | yes | as `next` | as `next`, plus `no_prev` |
| `set path <abs path>` | the path, to end of line | yes | as `next` | as `next`, plus `bad_args`, `not_found` |
| `set id <id>` | an id | yes | as `next` | as `next`, plus `bad_args`, `not_found` |
| `pause` | none | no | none | - |
| `resume` | none | no | none | - |
| `history [<n>]` | count, default 10, max 50 | no | `count: <n>`, then `entry:` lines, newest first | `bad_args` |
| `favorites` | none | no | `count: <n>`, then `entry:` lines | - |
| `favorite [<id>]` | optional id; default is the current entry | no | `favorited: <digest> <origin_key>`, plus `already: 1` if it was already pinned | `not_found`, `favorites_degraded` |
| `unfavorite <id>` | an id | no | `unfavorited: <digest>` | `not_found`, `favorites_degraded` |
| `sources` | none | no | `count: <n>`, then `source:` lines | - |
| `config path` | none | no | `config: <abs path>` | - |
| `config check` | none | yes | `queued`, then `source:` plan records, then one `plan:` line | `bad_config`, `worker_failed`, `timeout` |
| `subscribe [<seq>]` | optional resume point (2.9) | forever | `subscribed: <seq>`, then `event:` lines | `bad_args` |
| `close` | none | no | none; the daemon then closes the connection | - |

`*` codes that close the connection after the `ERR`.

The optional id argument (`favorite [<id>]`, `unfavorite <id>`, `set id <id>`) resolves in this
order, first hit wins, and the resolution is stated because a client must be able to predict it:

1. an exact `origin_key` match (`space:ab12cd`) in history, favorites or the cache index;
2. else, a 64-character lower-case hex string is a content digest;
3. else `ERR not_found <id> not in history, favorites or cache`.

`origin_key` is `<source id>:<source-scoped id>`: the site's own wallpaper id for a `wallhaven`
source, and `sha256` of the normalized absolute path for a `local` source, so a renamed file is a
new candidate rather than a silent dedupe miss `[D 6 §4.1]`. `[D 6 §4.1]`'s JSON example writes
`wallhaven:ab12cd`, with the source kind as the prefix; this document uses the source's `id`, because
two `wallhaven` sources must not collide on a candidate they both returned and because the same
section's prose calls it "the source-scoped candidate id". Recorded in 10.14.

`prev` selects by walking the ring, and it mutates nothing. History is newest first (2.6 records the
`entry:` form), and the selection rule is stated here so that a client can predict the second
`prev`, which is the question the prototype's answer leaves open:

1. find the newest history entry whose `digest` equals the current `anchor_digest`;
2. walk older from there and stop at the first entry whose `digest` differs;
3. if step 1 finds nothing (an `external` image, or a ring that has rolled past the anchor), start
   from the newest entry and apply step 2;
4. if no entry passes step 2, the answer is `ERR no_prev`.

`prev` removes nothing and appends nothing: the ring a client read with `history` is the ring the
next `prev` walks, so repeated `prev`s step one entry older each time, and `history_count` does not
change. The set itself changes the anchor, its timestamp and `last_via: prev`. Recording a `prev`
set as a history entry would put the walk inside the ring it walks, and the second `prev` would
become a function of the first that no client can predict without knowing the implementation. Any
rotation (`next`, `set path`, `set id`, a scheduled or startup rotation) appends an entry and
therefore restarts the walk from the newest entry. A `prev` whose entry has no bytes left
re-materialises it from the entry's `origin` before setting it, which is why the response carries
`queued` and a `set:` line exactly as `next` does `[D 6 §6.1]`. The prototype's `pop_back`
(`whd.rs:336`) is rejected, not adopted: 2.12.

`pause` freezes the schedule and `resume` re-arms it from now. While `paused: 1`, no rotation starts
and `next_at` is left where it is, so an hour of pause is not an hour of missed slots; `resume` sets
`next_at = now + schedule.interval_seconds`, persists it before answering, and the next rotation is
therefore one full interval away `[D 5 §F4]`, the rule the prototype already implements
(`whd.rs:377`). While `paused: 1`, `status` prints `next_in_s: -` (2.10), because there is no live
deadline to count down to. 5.5 rule 8 states both halves.

Argument counts are exact: a verb given more or fewer arguments than its row allows is
`ERR bad_args`. `set path` requires an absolute path (leading `/`, `~`, a drive letter, or a UNC
`\\`); a relative path is `ERR bad_args` and is never resolved against the daemon's cwd, which is
`/` under launchd `[D 1 §Falsified 2]`, the exact mechanism that made an AppleScript path
resolve to `/wh-commons.jpg` in the prototype's environment.

Which of `next`/`prev`/`set path`/`set id` a CLI verb maps to is the CLI's business: `whirl set`
sends `set path` when the argument contains `/` or `\` or starts with `~`, otherwise `set id`.

### 2.5.1 The CLI verbs, and which protocol verb each one is

`[D 5 §1.1]`'s rule is "every verb above is one or two protocol commands, and no protocol command
exists only to serve the CLI". The mapping, which `docs/development.md` can implement from and a
reviewer can check:

| CLI verb | protocol |
|---|---|
| `whirl next` | `next` |
| `whirl prev` | `prev` |
| `whirl set <path\|id>` | `set path <path>` or `set id <id>`, chosen by the argument's shape (2.5) |
| `whirl status` | `status` |
| `whirl pause`, `whirl resume` | `pause`, `resume` |
| `whirl history [n]` | `history [n]` |
| `whirl favorite [id]`, `whirl unfavorite <id>`, `whirl favorites` | `favorite [id]`, `unfavorite <id>`, `favorites` |
| `whirl sources` | `sources` |
| `whirl config path`, `whirl config check` | `config path`, `config check` |
| `whirl idle` | `subscribe`, then `close` after the first `event:` line |
| `whirl version` | optionally `hello`, then `version` |
| `whirl ping` | `ping` |

`whirl ping` is the liveness probe and nothing else: `ping` is one round trip that does no work and
has no success data lines (2.5), so the CLI prints nothing and its exit code is the whole answer, 0
when the daemon replied and 2 when the socket is unreachable. It adds no protocol surface, because
`ping` is a verb for any client (below); it is the CLI's own way to ask the question a user asks,
"is the daemon there", without `nc` and without reading a socket by hand.

Verbs with no CLI verb, and why they exist: `hello` is negotiation for any client, and the CLI never
sends it (`whirl version` maps to `version` alone, above); `close` is a clean shutdown of one
connection, and `whirl idle` issues it itself; `subscribe` is the frontend surface, reached through
`whirl idle`. No protocol verb exists only for the CLI, and no CLI verb needs a protocol verb of its
own.

`decision:` `whirl idle` is `subscribe` plus one event, not a server-side one-shot verb.
`[D 5 §1.1]`'s verb table wants "Block until state changes. For frontends, so none of them polls."
(the `whirl idle` row), and a stream narrowed by the client to its first event satisfies that with
one mechanism instead of two. `[D 5 §F10]` is the other half and is not this rule: F10 is `status`
and `sources` introspection, and it is cited for the stability of the `status` key set (2.10), not
for the no-polling rule. Consequences worth stating: `whirl idle` prints one `event:` line and exits
0 when a change arrives; it has no 60 s timeout of its own, because heartbeats keep the subscribed
connection alive (2.8), and a script that
wants a bound wraps it in `timeout(1)`; and the prototype's `idle` timeout line, which `whctl watch`
needed to keep its own loop alive, is gone with that loop `[M 13]`.

### 2.6 Response grammar

A response is zero or more lines followed by exactly one terminator.

```
data-line   := key ": " value          ; key matches [a-z][a-z0-9_]*
record-line := key ": " f1 " " f2 ... [" " rest]   ; positional after the key
interim     := "queued"
event-line  := "event: " seq " " type (" " fields...)
terminator  := "OK" | "ERR " code " " message
```

- **Every data line carries a `key: value` prefix, or is one of the positional `record-line`
  forms named per verb in 2.5.** Basis: `[L 3]` shows the prototype returning a bare `OK` for an
  empty `history` and an empty `favorites`, so a client cannot distinguish "no entries" from "I
  do not implement that key", and it returns `last: ` (empty value, trailing space) for an unset
  path, which makes trailing-whitespace handling load-bearing. `decision:` lists always begin with
  `count: <n>` and an unset scalar is the single character `-`, so a parser never has to guess and
  never has to trim.
- **The positional record forms** are exactly these four, and their last field takes the rest of
  the line:
  - `entry: <set_at> <via> <kind> <origin_key> <digest> <path|->` (history)
  - `entry: <added_at> <kind> <origin_key> <digest> <state> <path|->` (favorites; `state` is
    `present`, `missing` or `unrecoverable`; this is where `[D 6 §5.4 INV-CACHE-3]` becomes
    visible to a user)
  - `source: <id> <kind> weight=<n> enabled=<0|1> last=<outcome|-> [candidates=<n> admitted=<n>
    rejected_<stage>=<n> ...] reason=<text|->`, where the bracketed group appears only in a
    `config check` response and `last` is `-` there, and the stage names are the pipeline's
    (`resolution`, `ratio`, `size`, `type`, `dedupe`). `[D 5 §2.5]` requires exactly this
    per-source accounting and one record form is enough to carry both cases
  - `set: <digest> <origin_key> <via> <path|->`
  - `plan: <config key>=<effective value> ...`, where the last field of the line is the rest, so it
    uses the config's own key paths, in the order 4.2 writes them, and it is the whole rotation's
    set of them: `min_width` and `min_height` sit between `display` and `filters`, and `startup.*`,
    `filters.target_ratio` and `cache.root` appear like any other key. The two keys whose value is
    resolved rather than written keep their file positions too: `backend` after 4.3's precedence,
    `sources` as the count of enabled sources. `config_schema`, `socket` and `log_level` are absent,
    because they are the daemon's own settings and not the rotation's. A key with no value prints
    `-`, this document's rule everywhere else, which is how an unset `cache.root` or
    `filters.target_ratio` reads. For the config 4.2 writes, the line is exactly
    `plan: schedule.interval_seconds=1800 schedule.worker_deadline_seconds=300 startup.enabled=1
    startup.mode=last startup.respect_manual=1 display.mode=all display.mode_effective=all
    min_width=1600 min_height=900 filters.max_bytes=41943040 filters.ratio_tolerance=0.02
    filters.target_ratio=- state.history_entries=50 dedupe.recent_entries=50 cache.root=-
    cache.max_bytes=2147483648 cache.max_files=500 cache.grace_seconds=600
    cache.orphan_grace_seconds=300 backend=native sources=2`. A client can print it, diff it
    against a config, or ignore it; it exists because "what did the daemon actually adopt" must be
    answerable without reading the daemon's mind
- **Two closed vocabularies**, so a client never has to interpret free text:
  - `via` is `source` (the pipeline chose the candidate), `manual` (a `set path`/`set id`
    request), `prev` (a `prev` request), `startup` (the startup rotation, including a manual change
    the daemon detected), or `recovered` (1.7.3). This is the only list: a history entry written to
    `history.json` records the same five values
    (`docs/spec/state-and-cache.md` 6.2), so the file and the `entry:` record cannot disagree.
  - `kind` is `local`, `wallhaven` or `external`, and it names the origin, not the mechanism.
    For an `external` entry `origin_key` is `external:<sha256 of the absolute path>` and `digest`
    is `-` when the file could not be hashed, because an image the user set by hand has no source
    and may not be readable.
- **Timestamps are RFC 3339 UTC** (`2026-09-25T07:41:12Z`). Basis: the macOS store's timestamps
  are UTC and reading them as local time is off by the timezone offset, measured in
  `[D 1 §1]`; one convention everywhere removes the class of bug.
- **`queued` is the only interim line**, and only for the verbs that spawn a worker (`next`,
  `prev`, `set path`, `set id`, `config check`). It means "the request was accepted and a worker
  is running", and it is sent before the daemon blocks on the worker. Basis: the prototype sends
  it (`whd.rs:218`) and it is the difference between "still working" and "connection lost" for a
  client whose read timeout is 300 s `[M 13]`.
- **A `-` value means "unset", never "empty string".** A digest is 64 lower-case hex characters.
  A path in a record is absolute on POSIX and a drive-absolute path on Windows.

### 2.7 The error model

A failure is always the last line of a response, always starts with `ERR `, and always carries one
code from this closed set. Nothing else in the protocol can be mistaken for a failure, and no
failure can be mistaken for state: a data line always carries a `key:` prefix, and the success
terminator is the bare token `OK`.

| Code | Meaning | Produced by | Closes? |
|---|---|---|---|
| `unknown_verb` | the first token is not a verb | daemon | no |
| `bad_args` | wrong arity, a relative path, a non-numeric `n` | daemon | no |
| `too_long` | request line over 8192 bytes | daemon | yes |
| `bad_framing` | NUL in a request line, invalid UTF-8 | daemon | yes |
| `bad_protocol` | `hello` with an out-of-range or unparsable version | daemon | yes |
| `busy` | a rotation is in flight, or 16 clients are connected | daemon | no (rotation), yes (clients) |
| `not_found` | id does not resolve; path does not exist | daemon | no |
| `no_prev` | no earlier entry in history | daemon | no |
| `no_candidates` | every configured source yielded nothing admissible | worker | no |
| `offline` | every source that could serve this rotation needs the network and the network failed | worker | no |
| `too_large` | the download exceeded `filters.max_bytes` | worker | no |
| `not_an_image` | the header sniff failed | worker | no |
| `set_failed` | the platform setter refused the file | worker | no |
| `enospc` | disk full during the download or a state write | worker, daemon | no |
| `cache_readonly` | the cache could not be written | worker | no |
| `cache_unwritable` | the cache root could not be created | worker | no |
| `worker_failed` | the worker exited non-zero, crashed, or was killed | daemon | no |
| `timeout` | the request did not complete inside 300 s | daemon | no |
| `favorites_degraded` | `favorites.json` is quarantined, so pin state is unknown | daemon | no |
| `bad_config` | the config fails validation; the message names the key | daemon | no |
| `internal` | an unexpected daemon error; the message carries a correlation id | daemon | no |

`decision:` codes, not the prototype's free text. `[L 3]` is the evidence: the prototype answers
an unknown verb with `ACK unknown command 'frobnicate'`, and an empty history with a bare `OK`;
a client has to pattern-match English to tell a failure from a fact, and the same channel carries
both. A code is a contract a client can branch on, and it is what `whirl`'s exit code 1 maps to
(2.12).

A message may contain the offender after the code, in the same line, because the terminator is the
last line: `ERR bad_args history: n must be 1..=50, got 500`. Messages never contain the value of
a secret: `[D 5 §2.4]` forbids it, and the only key whirl ever knows about appears as the label
`api_key_ref`, never as a value.

### 2.8 Timeouts

| Name | Value | Basis |
|---|---|---|
| `connection_idle_timeout` | 300 s without a complete request line, and without any traffic on a subscribed connection | The prototype's own client read timeout is 300 s (`whctl.rs:32`, `[M 13]`), so no client that waits as long as the prototype's does is ever cut off; today the daemon has no bound at all, and `[L 4]` shows what unbounded connections cost |
| rotation request timeout | 300 s | Deliberately equal to the worker deadline: a client must not give up before the daemon does, or it will report a failure for a rotation that then succeeds |
| `schedule.worker_deadline_seconds` | 300 s | The worker's own HTTP client timeout is 2 minutes `[M 5]` and a healthy rotation is 1.2-3.4 s `[M 4]`: ~90x the measured worst case |
| `SIGTERM` -> `SIGKILL` escalation | 5 s | `[M 4]`: a worker that has not exited 5 s after `SIGTERM` has passed its own measured p100 by 47% |
| subscribe heartbeat | 30 s of quiet | Half of the prototype's own 60 s idle wait (`whd.rs:283`), so the heartbeat is strictly more frequent than the bound the prototype's client already tolerated |
| client liveness window | 90 s (three missed heartbeats) | Derived from the heartbeat |
| `cache.grace_seconds` / `cache.orphan_grace_seconds` | 600 / 300 | `[D 6 §5.3]`, `[D 6 §5.5]`; not this document's numbers |
| `schedule.interval_seconds` | 1800 | `[D 5 §F0]`-era default, kept; the prototype's `WHD_INTERVAL` default is the same 1800 s `[M 10]` |

No timeout is applied to a *running* verb other than through the worker deadline: `status`,
`history`, `favorites`, `sources`, `config path`, `pause` and `resume` are answered from
in-memory data and cannot block. `config check` can be reached only through the worker deadline.

### 2.9 Subscribe mode and the event format

A frontend needs to know when the wallpaper changed without polling, so the daemon offers a
stream. It is a mode, not a verb with an answer:

```
--> subscribe [<since>]
<-- subscribed: <current seq>
<-- gap: <n>            # only when <since> was given and <since> < <current seq>
<-- event: <seq> <type> <fields...>
<-- event: <seq> <type> <fields...>
...
```

- `subscribed:` carries the daemon's current `seq`, so a client knows the number to compare
  against. `gap:` states how many events it missed, sparing every client the subtraction.
- The daemon keeps **no event history**. A client that had a gap must call `status` on its own
  connection, because the events it missed are gone. This is why `status` must be a complete
  snapshot of everything a frontend draws (2.10), and why the events are notifications rather than
  data transfer.
- `seq` starts at 1 per daemon run and increases by exactly 1 per event. It resets on restart and
  must never be persisted by a client `[D 6 §6.1]`; a client that sees a smaller `seq` than before
  knows the daemon restarted and must re-read `status`.
- A heartbeat is an event and consumes a `seq`. Any quiet period of 30 s produces exactly one
  `heartbeat` event, so the client's liveness test is "no line at all for 90 s" (2.8).
- **One event per state transition.** `[D 6 §7.3]` step 5 requires a single notification per
  rotation, and `[M 12]` records that the prototype wakes twice per rotation. The daemon therefore
  announces a rotation at `rotate_start` and at its outcome, and nothing else, and the `rotating`
  key in `status` is the only other place the intermediate state appears.
- The stream ends with `shutdown` and a close if the daemon is going away, or with a close if the
  client sent `close`. Any other request during a subscription is answered
  `ERR bad_args subscribe takes over this connection` **inside the stream**, and the stream
  continues: an `ERR` line is distinguishable from an `event` line by its prefix, so a client
  never has to choose between reading events and handling errors.

| Event | Fields | Meaning |
|---|---|---|
| `rotate_start` | `<run>` | a rotation began; `run` is the daemon's monotonic slot counter |
| `rotate_ok` | `<digest> <origin_key> <via> <path>` | the wallpaper changed; `path` takes the rest of the line |
| `rotate_failed` | `<code> <message>` | the rotation produced nothing; `code` is from 2.7 |
| `paused` | - | the schedule is suspended, so no rotation will start, and `next_at` is frozen (5.5 rule 8) |
| `resumed` | - | the schedule is live again, with `next_at` re-armed from now (2.5, 5.5 rule 8) |
| `favorite_added` | `<digest> <origin_key>` | a pin was written |
| `favorite_removed` | `<digest>` | a pin was removed |
| `cache_swept` | `<removed> <reclaimed_bytes> <hidden>` | a sweep completed; `hidden` is how many entries were kept only because they are pinned `[D 6 §5.3]` |
| `clock_jump` | `<seconds>` | the wall clock moved more than one interval, so the deadline was recomputed `[D 4 §Part 2]` |
| `config_reloaded` | - | the daemon re-read the config and it parsed `[D 5 §F0]`; no fields, because no digest of the config exists anywhere in the protocol for one to carry |
| `anchor_unverified` | - | 1.7.3 step 3 took effect: the daemon does not know what is on screen and is protecting the whole grace window |
| `shutdown` | - | the daemon is exiting; the connection closes immediately after |
| `heartbeat` | `<unix_seconds>` | 30 s of quiet |

### 2.10 `status`: the stable key set

`status` is the complete snapshot a frontend draws from, so its keys are a contract: they may be
added, never renamed or removed, and a client must ignore keys it does not know (section 2.4).
`[D 5 §F10]` fixes that stability rule, `[D 6 §9]` names most of these keys because its invariants
are checked against them, and the third column below says where each name comes from.

| Key | Example | Name comes from | Meaning |
|---|---|---|---|
| `daemon_version` | `whirl 0.1.0` | prototype | the daemon's version as `<product> <semver>` on one line; the greeting carries the bare semver and the name once 2.4 |
| `protocol` | `2` | prototype | the protocol version, the client's only compatibility check 2.1 |
| `platform` | `macos` | here | `macos` \| `linux` \| `windows`; the client may branch on it |
| `pid` | `4711` | prototype | the daemon's pid, so a script can check it is alive |
| `seq` | `183` | prototype | the state sequence number; every state change increments it 2.9 (the prototype called it `gen`) |
| `uptime_s` | `98765` | prototype | seconds since the daemon started, not since login |
| `rss_kb` | `2312` | prototype | the daemon's own RSS, measured `[M 1]` as 1.8-2.3 MB |
| `paused` | `0` | prototype | 1 while `pause` holds the schedule 7.9 |
| `rotating` | `0` | prototype | 1 while a worker is running; this is why `next` has a `busy` code 7.8 |
| `rotation_count` | `128` | prototype | rotations completed since the state file was created |
| `interval_s` | `1800` | prototype | the effective `schedule.interval_seconds` |
| `next_at` | `2026-09-25T08:11:12Z` | prototype | the persisted deadline, wall clock, RFC 3339 UTC 5.5 |
| `next_in_s` | `1764` | `[D 6 §8.6]` | seconds until `next_at`, computed when the response is written 8.6; `-` while `paused: 1`, because a suspended schedule has no deadline to count down to 5.5 rule 8 |
| `last_digest` | `d435840ce84fbb8d...` | prototype | content digest of the displayed image |
| `last_origin_key` | `space:ab12cd` | prototype | where it came from 2.5 |
| `last_via` | `source` | prototype | the closed `via` vocabulary of 2.6 |
| `last_at` | `2026-09-25T07:41:12Z` | prototype | when it was set |
| `last_error` | `-` | prototype | the last rotation's error code, `-` if it succeeded |
| `history_entries` | `50` | `[D 6 §9]` | the ring's bound, `state.history_entries` 6.2 |
| `history_count` | `3` | prototype | entries currently in the ring (alias of the prototype's `history`) |
| `favorites_count` | `1` | prototype | pinned entries (alias of the prototype's `favorites`) |
| `display_mode` | `all` | here | the configured `display.mode` |
| `display_mode_effective` | `all` | `features 1.3` | what the platform actually gets; features.md 1.3 names this key for the `per-display` fallback |
| `display_mode_reason` | `-` | here | why, when they differ: `unverified_platform`, `impossible_on_this_desktop`, `out_of_scope_on_this_desktop`, `no_displays` 3.7 |
| `anchor_digest` | `d435840ce84fbb8d...` | `[D 6 §9]` | what whirl believes is on screen; `-` before the first verified rotation 1.7.3 |
| `anchor_path` | `sha256/d4/35/d43584...` | `[D 6 §9]` | the path the platform was given, `-` for a reference-mode set |
| `anchor_verified` | `1` | here | 1 once a rotation's readback agreed with the set; 0 while unverified 1.7.3 |
| `cache_dir` | `/Users/govind.rajpurohit/Library/Caches/whirl` | `[D 6 §9]` | which cache this daemon owns 3.1 |
| `cache_root_id` | `9d1f0c2e-5b6a-4d7e-8f11-0c2b4a6d9e01` | `[D 6 §9]` | the cache root's identity file, so `status` can tell two daemons apart 3.1 |
| `cache_files` | `312` | `[D 6 §9]` | files in the cache root |
| `cache_bytes` | `180224512` | `[D 6 §9]` | their total size |
| `cache_files_cap` | `500` | `[D 6 §9]` | `cache.max_files` (alias `keep`) |
| `cache_bytes_cap` | `2147483648` | `[D 6 §9]` | `cache.max_bytes` |
| `cache_over_cap` | `0` | `[D 6 §9]` | 1 after a sweep driven by contention rather than by a timer 5.2 |
| `cache_over_reason` | `-` | `[D 6 §9]` | `single_file`, `pinned`, `sweep_error` or `favorites_degraded`, the four causes `[D 6 §5.4]` makes exhaustive, or `-` |
| `cache_writable` | `1` | `[D 6 §9]` | 0 when the daemon has had to continue without a writable cache 8.4 |
| `sweep_deferred` | `0` | `[D 6 §9]` | 1 when the sweep could not take the rotation lock because another holder had it, which is the only trigger `[D 6 §5.5]` step 1 gives this key; a sweep that ran and failed is a different fact and reports `cache_over_reason: sweep_error` instead |
| `lock_mode` | `flock` | `[D 6 §9]` | `flock` \| `excl_file`: the primitive actually holding `state/locks/daemon.lock`, which 1.5 step 1 takes at startup and holds for the daemon's lifetime, with `excl_file` where the filesystem cannot `flock` 8.7. Windows's `LockFileEx` reports `flock`, because it is that platform's own exclusive lock rather than a weaker one. There is no third value: a daemon that cannot take the lock does not run 1.5 step 1, and a lock file left behind by a daemon that is gone is classified and refused at that same step, before there is a socket for `status` to answer on `[D 6 §8.8]` |
| `state_dir` | `/Users/govind.ra...` | `[D 6 §9]` | where the state files are 1.1 |
| `state_corrupt` | `-` | `[D 6 §9]` | the state file that failed to parse, if any 6.4 |
| `state_quarantined` | `-` | `[D 6 §9]` | the path it was moved to before defaults were written 6.4 |
| `state_schema_newer` | `0` | `[D 6 §9]` | 1 when the file's schema is newer than this binary 6.4; the file's name and both schema numbers go to the log, not to this key, because the key is a typed flag like its neighbours and no client branches on the detail |
| `history_lost` | `0` | `[D 6 §9]` | entries lost to a quarantine |
| `favorites_degraded` | `0` | `[D 6 §9]` | 1 when a pin could not be honoured, so a favorite may be evicted 5.4 |
| `clock_jump` | `0` | `[D 6 §9]` | how many clock jumps larger than two intervals have been seen 8.6 |
| `respect_manual_effective` | `1` | here | 0 where the platform cannot read the current image back, so `startup.respect_manual` cannot be honoured 1.7.3 |
| `sources` | `2` | here | enabled sources, out of all configured |
| `source:` | `source: pictures local weight=1 enabled=1 last=ok reason=-` | here | one line per source, in config order; the record form is 2.6 |
| `source:` | `source: space wallhaven weight=3 enabled=1 last=ok reason=-` | here | a source that failed validation keeps its line with `enabled=0` and the reason 4.3 |

Legend for the third column: `[D 6 §9]` and `[D 6 §8.6]` name that key exactly, which is why they are the ones the invariants are checked against; `features 1.3` names `display_mode_effective` for the fallback; `prototype` means the prototype's `status` already carried that key, renamed here to say what it means; `here` means this document added the name, and the row's own justification is in the section it cites.

### 2.11 Worked transcript

Values are consistent across all three transcripts: every digest is a real `sha256` of the string it
names (the `origin_key`, or the path for an external entry), computed when this document was built,
so the fields are the right shape and length; the local origin key is `sha256` of the absolute path
(2.5); and paths under `cache/sha256/` follow the two-level fan-out of `[D 6 §3]`:
`sha256/<first two hex characters of the digest>/<next two characters>/<digest>.<ext>`, so a file
whose digest begins `d435840c` lives at `sha256/d4/35/<digest>.<ext>`. Every `sha256/` path in the
three transcripts below recomputes that way from its own digest, and the two in transcript B start
with `26`, so they read `sha256/26/b8/`. `-->` is the client, `<--` is the daemon. The status block
in A is generated from the same list as the table in 2.10, so the two cannot drift.

**A. A normal session: version negotiation, a rotation, pins, history, plan.**

```
$ nc -U ~/Library/Application\ Support/whirl/whirl.sock
<-- OK whirl 0.1.0 protocol 2
--> hello 2 whirl-cli/0.1.0
<-- protocol: 2
<-- OK
--> version
<-- daemon_version: whirl 0.1.0
<-- protocol: 2
<-- platform: macos
<-- OK
--> ping
<-- OK
--> status
<-- daemon_version: whirl 0.1.0
<-- protocol: 2
<-- platform: macos
<-- pid: 4711
<-- seq: 183
<-- uptime_s: 98765
<-- rss_kb: 2312
<-- paused: 0
<-- rotating: 0
<-- rotation_count: 128
<-- interval_s: 1800
<-- next_at: 2026-09-25T08:11:12Z
<-- next_in_s: 1764
<-- last_digest: d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f
<-- last_origin_key: space:ab12cd
<-- last_via: source
<-- last_at: 2026-09-25T07:41:12Z
<-- last_error: -
<-- history_entries: 50
<-- history_count: 3
<-- favorites_count: 1
<-- display_mode: all
<-- display_mode_effective: all
<-- display_mode_reason: -
<-- anchor_digest: d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f
<-- anchor_path: sha256/d4/35/d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f.jpg
<-- anchor_verified: 1
<-- cache_dir: /Users/govind.rajpurohit/Library/Caches/whirl
<-- cache_root_id: 9d1f0c2e-5b6a-4d7e-8f11-0c2b4a6d9e01
<-- cache_files: 312
<-- cache_bytes: 180224512
<-- cache_files_cap: 500
<-- cache_bytes_cap: 2147483648
<-- cache_over_cap: 0
<-- cache_over_reason: -
<-- cache_writable: 1
<-- sweep_deferred: 0
<-- lock_mode: flock
<-- state_dir: /Users/govind.rajpurohit/Library/Application Support/whirl
<-- state_corrupt: -
<-- state_quarantined: -
<-- state_schema_newer: 0
<-- history_lost: 0
<-- favorites_degraded: 0
<-- clock_jump: 0
<-- respect_manual_effective: 1
<-- sources: 2
<-- source: pictures local weight=1 enabled=1 last=ok reason=-
<-- source: space wallhaven weight=3 enabled=1 last=ok reason=-
<-- OK
--> next
<-- queued
<-- set: d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f space:ab12cd source sha256/d4/35/d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f.jpg
<-- OK
--> favorite
<-- favorited: d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f space:ab12cd
<-- OK
--> favorite
<-- favorited: d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f space:ab12cd
<-- already: 1
<-- OK
--> history 3
<-- count: 3
<-- entry: 2026-09-25T07:41:12Z source wallhaven space:ab12cd d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f sha256/d4/35/d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f.jpg
<-- entry: 2026-09-25T07:11:09Z source local pictures:401df7171a5e52d88b2f7f9e9d201308f9600e7034916d7865c883f33ec64dcd 3b29b61764d0a17238f7a51d2585eccf538171d638f210e810f4e8eab970387a /Users/govind.rajpurohit/Pictures/Wallpapers/valley.jpg
<-- entry: 2026-09-25T06:58:02Z startup external external:d25a845d3a284ed719022916037cde61995e9d3c31b96253e87c0c2d3032e35d - /System/Library/Desktop Pictures/Mac Yellow.heic
<-- OK
--> favorites
<-- count: 1
<-- entry: 2026-09-25T07:42:01Z wallhaven space:ab12cd d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f present sha256/d4/35/d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f.jpg
<-- OK
--> unfavorite d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f
<-- unfavorited: d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f
<-- OK
--> favorite space:ab12cd
<-- favorited: d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f space:ab12cd
<-- OK
--> set path /Users/govind.rajpurohit/Pictures/Wallpapers/valley.jpg
<-- queued
<-- set: 3b29b61764d0a17238f7a51d2585eccf538171d638f210e810f4e8eab970387a pictures:401df7171a5e52d88b2f7f9e9d201308f9600e7034916d7865c883f33ec64dcd manual /Users/govind.rajpurohit/Pictures/Wallpapers/valley.jpg
<-- OK
--> set id 3b29b61764d0a17238f7a51d2585eccf538171d638f210e810f4e8eab970387a
<-- queued
<-- set: 3b29b61764d0a17238f7a51d2585eccf538171d638f210e810f4e8eab970387a pictures:401df7171a5e52d88b2f7f9e9d201308f9600e7034916d7865c883f33ec64dcd manual /Users/govind.rajpurohit/Pictures/Wallpapers/valley.jpg
<-- OK
--> prev
<-- queued
<-- set: d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f space:ab12cd prev sha256/d4/35/d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f.jpg
<-- OK
--> pause
<-- OK
--> resume
<-- OK
--> sources
<-- count: 2
<-- source: pictures local weight=1 enabled=1 last=ok reason=-
<-- source: space wallhaven weight=3 enabled=1 last=ok reason=-
<-- OK
--> config path
<-- config: /Users/govind.rajpurohit/Library/Application Support/whirl/config.json
<-- OK
--> config check
<-- queued
<-- source: pictures local weight=1 enabled=1 last=- candidates=412 admitted=97 rejected_resolution=203 rejected_ratio=41 rejected_size=0 rejected_type=71 rejected_dedupe=0 reason=-
<-- source: space wallhaven weight=3 enabled=1 last=- candidates=24 admitted=3 rejected_resolution=0 rejected_ratio=0 rejected_size=0 rejected_type=0 rejected_dedupe=21 reason=-
<-- plan: schedule.interval_seconds=1800 schedule.worker_deadline_seconds=300 startup.enabled=1 startup.mode=last startup.respect_manual=1 display.mode=all display.mode_effective=all min_width=1600 min_height=900 filters.max_bytes=41943040 filters.ratio_tolerance=0.02 filters.target_ratio=- state.history_entries=50 dedupe.recent_entries=50 cache.root=- cache.max_bytes=2147483648 cache.max_files=500 cache.grace_seconds=600 cache.orphan_grace_seconds=300 backend=native sources=2
<-- OK
--> close
<-- OK
(connection closed)
```

**B. A subscription, and the same daemon from a second connection.**

```
connection 1: whirl --follow (subscribe)                connection 2: whirl next, then pause, resume, close
$ nc -U ~/Library/Application\ Support/whirl/whirl.sock
<-- OK whirl 0.1.0 protocol 2
--> subscribe 182
<-- subscribed: 183
<-- gap: 1
                                                        --> next
                                                        <-- queued
<-- event: 184 rotate_start 4211
                                                        <-- set: 26b885cbb8096ddac7173f80fcd852b783949d658bb8a9e19bda874bf1c31dc6 pictures:1cc438356245b4cc61935af265cf3e8efc90425c6eb03b05ac886666e1fe9724 source sha256/26/b8/26b885cbb8096ddac7173f80fcd852b783949d658bb8a9e19bda874bf1c31dc6.jpg
                                                        <-- OK
<-- event: 185 rotate_ok 26b885cbb8096ddac7173f80fcd852b783949d658bb8a9e19bda874bf1c31dc6 pictures:1cc438356245b4cc61935af265cf3e8efc90425c6eb03b05ac886666e1fe9724 source sha256/26/b8/26b885cbb8096ddac7173f80fcd852b783949d658bb8a9e19bda874bf1c31dc6.jpg
                                                        --> pause
                                                        <-- OK
<-- event: 186 paused
                                                        --> resume
                                                        <-- OK
<-- event: 187 resumed
<-- event: 188 config_reloaded
<-- event: 189 cache_swept 12 34816000 1
<-- event: 190 clock_jump 7200
<-- event: 191 anchor_unverified
<-- event: 192 heartbeat 1790324533
                                                        --> close
                                                        <-- OK
--> close
<-- OK
(connection closed)
```

**C. Failures and refusals: the error model, and that an `ERR` does not end a connection.**

```
C1. A refused request does not end the connection, except where the table in 2.5 says it does.

--> next
<-- queued
<-- ERR offline every source that could serve this rotation needed the network
--> ping
<-- OK
--> history 500
<-- ERR bad_args history: n must be 1..=50, got 500
--> unfavorite 0123
<-- ERR not_found 0123 is not an origin_key, not a digest and not a favorite
--> frobnicate
<-- ERR unknown_verb unknown command 'frobnicate'
--> ping
<-- OK
--> close
<-- OK

Two connections, one rotation: the second is refused rather than queued.

connection 1                                            connection 2
--> next                                                --> next
<-- queued                                              <-- ERR busy a rotation is in flight (run 4212)
<-- set: d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f space:ab12cd source sha256/d4/35/d435840ce84fbb8d633f0d1f81ad0b620fff857597bca6ab238b51e164e1da9f.jpg
<-- OK

C2. A broken frame is fatal for that connection, and the `ERR` is the last line on it.

<-- OK whirl 0.1.0 protocol 2
--> <8200 bytes before the first newline>
<-- ERR too_long request line exceeds 8192 bytes
(connection closed)
--- new connection ---
<-- OK whirl 0.1.0 protocol 2
--> status\0
<-- ERR bad_framing NUL in request line
(connection closed)
--- new connection ---
<-- OK whirl 0.1.0 protocol 2
--> hello 9
<-- ERR bad_protocol server speaks 2, client asked for 9
(connection closed)

C3. A rotation that fails is visible on both planes.

--> next
<-- queued
<-- ERR set_failed the platform refused the set: The file doesn't exist.
--> status
<-- last_error: set_failed
<-- anchor_verified: 0
<-- clock_jump: 0
<-- OK
--> subscribe
<-- subscribed: 193
<-- event: 194 rotate_start 4213
<-- event: 195 rotate_failed set_failed the platform refused the set: The file doesn't exist.
--> close
<-- OK
```

Coverage, checked mechanically when this document was written: every verb in sections 2.5 and 2.9
appears in A, B or C, and every verb that appears in A, B or C is defined in 2.5 or 2.9. The check
and its output are in `[L 9]`.

### 2.12 Where this deliberately differs from the prototype's protocol

Each row is a difference, not a drift: the prototype's behaviour is on the left, and the reason it
was not carried over is on the right.

| Prototype | Here | Why |
|---|---|---|
| `ACK <free text>` is the error line | `ERR <code> <message>` | `[L 3]`: the same channel answers an unknown verb with `ACK unknown command 'frobnicate'`, so a client must match English to branch |
| data lines are bare (`last: <path>`, a list of raw paths) | `key: value` data lines and four prefixed record forms | `[L 3]`: an empty `history` returns a bare `OK`, and a path is a byte string that could itself be `OK` or start with `ERR `, which would end a frame early |
| lists have no header line | `count: <n>` first | same `[L 3]` evidence: zero entries and "unknown key" look identical |
| `OK whd 0.1.0 protocol 1` | `protocol: 2` plus `hello` | the grammar changed in four ways, so keeping the number 1 would claim a compatibility that does not exist |
| one-shot `idle`, blocks 60 s, returns on any change | `subscribe` stream, 30 s heartbeat, monotonic `seq` | a frontend needs events, not a 60 s poll loop; `[D 6 §7.3]` wants one notification per transition, not a wake per poll |
| `-set <path>` | `set path <path>`, `set id <id>` | the daemon never guesses whether an argument is a path or an id; the guess lives in the CLI, where a wrong guess costs one exit code 3, not a wall of state |
| `log` verb (last 15 lines) | dropped | `[D 5 §1.1]`'s verb set is closed and the log is a file for humans `[D 6 §7.2]`; a protocol verb no CLI verb maps to is surface with no owner |
| `prev` pops the newest entry out of the ring (`whd.rs:336`) | `prev` walks the ring and mutates nothing 2.5 | the pop destroys the entry the user just rejected, and it is the ring it walks: `[D 6 §6.2]` keeps every set as an entry, and "step back" that silently deletes cannot be undone or predicted from `history` |
| `status` reports `gen` | `status` reports `seq`, and `subscribe` carries the same number | the same counter, named for the thing a client actually compares |
| exit codes 0, 1, 2 | 0, 1, 2 and 3 | 1 keeps exactly one meaning, "the daemon refused, and the `ERR` code says why"; a usage error is not a failure of the daemon `[M 13]` |
| `remove_file(socket)` unconditionally at startup | connect-probe, unlink only on `ECONNREFUSED`/`ENOENT` | `[M 15]`: unlinking a live daemon's socket leaves the daemon running and unreachable, then lets a second daemon bind the freed path |
| worker spawned with `env()`, inheriting everything | `env_clear()` plus the named variables in 1.6 | `[M 16]`; and the Linux adapters need specific session variables, not the whole environment `[D 3 §Detecting the environment]` |
| the daemon waits on the worker forever | 300 s, then TERM, 5 s, then KILL | `[M 14]`: a hung worker leaves `rotating = true` and the scheduler never runs again |
| `next_at` re-anchored when a rotation finishes | `next_at` advanced by whole intervals from the persisted value | `[D 4 §Part 2]` drift row: every rotation that ran late pushes the whole schedule late |
| two wakeups per rotation (one from the worker, one from the scheduler) | one event per transition | `[M 12]`, `[D 6 §7.3]` step 5 |
| state files rewritten in place | temp file, fsync, rename | `[D 6 §6.3 L 5]`: 3068 of 3508 reads of a concurrently rewritten state file saw a truncated file |
| one thread per connection, unbounded | 16 connections, then `ERR busy` | `[L 4]`: 128 held connections moved RSS 1.8 -> 5.1 MB, and any process running as the user can open them |

## 3. Cross-platform verdict

### 3.0 The answer

**Yes, on all three platforms: one lightweight resident daemon plus an optional lightweight UI is
possible, and the OS scheduler is sufficient.** The rotation timer and the rotation process are
not OS timers or OS jobs; they are the daemon's clock and the daemon's child process (section 5).
Every platform can set the wallpaper from an ordinary unprivileged process, and none of the three
requires a GUI toolkit in whirl's process tree.

Three qualifications, all of them platform facts rather than design choices:

1. **The daemon has to be resident, per user session, everywhere.** There is no platform where a
   one-shot process at login could rotate a wallpaper on a timer without leaving something running.
   The cost of that something is measured: 1.8 MB idle, 2.3 MB after 7 rotations `[M 1]`.
2. **"Everywhere" means fewer places than the words suggest.** Per-Space on macOS
   `[D 1 §2]`, per-virtual-desktop on Windows `[D 2 §3]`, and per-monitor on GNOME
   `[D 3 §GNOME 2]` are impossible. Section 3.6 states each one as a limitation with the
   mechanism.
3. **One platform does force a third-party resident helper: Hyprland**, whose only wallpaper API
   needs `hyprpaper` (or `swaybg`) already running `[D 3 §The resident helper]`. whirl does not
   own it, does not start it, and does not supervise it, so that platform's memory floor is set by
   someone else's process. It is deferred from v0.1 for exactly that reason `[D 3 §The decisive
   column]`.

### 3.1 The per-platform table

| | macOS | Windows | Linux |
|---|---|---|---|
| API | `NSWorkspace.setDesktopImageURL:forScreen:options:error:`, `macos(10.6)`, no entitlement needed `[D 1 §3]` | `IDesktopWallpaper`, `Win8`/`Server2012`, desktop apps only `[D 2 §1]` | per environment: `gsettings` (GNOME), `plasma-apply-wallpaperimage` or DBus (KDE), `swaymsg output bg` (sway), `hyprctl` (Hyprland), `feh`/`xwallpaper`/`hsetroot` (generic X11) `[D 3 §The decisive column]` |
| Resident requirement | the daemon only, and it must be a per-user `LaunchAgent` in the Aqua session; a `LaunchDaemon` cannot act on behalf of a user `[D 1 §9]`, TN2083 | the daemon only, per user, interactive session; a service in session 0 fails with error 1459 `[D 2 §4]` | the daemon only, `systemd --user`; no helper on GNOME/KDE/sway/X11 `[D 3 §The resident helper]` |
| Per-monitor | modelled (`Displays`, keyed by display UUID), but the write goes to the frontmost Space and the fallback order when a UUID is missing is unverified `[D 1 §2]`: `per-display` is accepted and runs as `all` | yes, per monitor by design, and the only platform where v0.1 ships `per-display` `[D 2 §1]` | GNOME: impossible `[D 3 §GNOME 2]`; KDE: out of scope `[D 3 §KDE 2]`; sway: per output `[D 3 §sway]`; generic X11: `--output` for xwallpaper, file order / Xinerama for feh `[D 3 §Generic X11]` |
| Per-Space / per-virtual-desktop | **impossible.** The live Space node is authoritative and `allSpaces` is inert; writes land on the frontmost Space only `[D 1 §2]` | **impossible** via the documented API; `IVirtualDesktopManagerInternal::SetDesktopWallpaper` is undocumented and per-build `[D 2 §3]` | no supported setter takes a virtual desktop as a parameter `[D 3 §The decisive column]`, so there is no API to call rather than a technicality |
| Read back the current image | yes, `desktopImageURLForScreen:` `[D 1 §3]` | yes, `GetWallpaper` per monitor `[D 2 §1]` | per environment, not for generic X11 `[D 5 §1.5]` |
| Call cost | one call per screen, in-process, `NO` + `NSError` on failure `[D 1 §3]` | one out-of-process COM call per monitor, cheap at rotation frequency but not in a hot loop `[D 2 §1]` | one process spawn per call (`gsettings`, `plasma-apply-wallpaperimage`), or one IPC round trip (`swaymsg`, `hyprctl`) `[D 3 §The decisive column]` |
| Slideshow / rotation built in | no | yes (OS slideshow, and mixing with it is undefined) `[D 2 §5]` | yes on KDE (`org.kde.slideshow`), and it owns the config key `[D 3 §KDE 2]` |
| Distribution friction | Developer ID + notarisation for a distributable binary; ad-hoc signed binaries break under a sandbox entitlement `[D 1 §7]` | none beyond the `windows` crate's four Win32 feature flags, COM plus three more (`Win32_Foundation`, `Win32_System_Memory`, `Win32_UI_Shell`) `[D 2 §1]` | none: package sizes for the helpers we do not ship are `swaybg` 15.3 KB package / 33.6 KB installed, `hyprpaper` 160.1 KB / 478.8 KB `[D 3 §The resident helper]` |
| Minimum version | macOS Sonoma 14.0 `[D 1 §3]` | Windows 8 / Server 2012 for `IDesktopWallpaper` `[D 2 §1]` | no single floor; the floor is per environment `[D 3 §The decisive column]` |

### 3.2 macOS, stated precisely

What whirl does at rotation time, in one paragraph, is `[D 1 §3]`: enumerate screens, and for each
call `setDesktopImageURL:forScreen:options:error:` with the cached file. The API is not deprecated,
needs no entitlement, and reports success or an `NSError` rather than silently doing nothing. The
call goes to the frontmost Space; the other Spaces are not touched, and there is no API to touch
them `[D 1 §2]`. `allSpaces` exists in the options dictionary and is inert: two nodes changed with
it and without it `[D 1 §2, V3]`, so it is not used, and the doc says so rather than setting it
because the name looks right.

Consequences the architecture has to absorb:

- **The startup rotation is what makes "one image everywhere" mostly true in practice.** A new
  Space inherits layers the store does not document, so whirl does not claim the image will be on
  a Space it has never written to. `[D 5 §1.3]`'s rule, "a display connected later gets the image
  at the next rotation", is the same shape.
- **`respect_manual` reads the store, not a wish.** The write path replaces the store file rather
  than editing it (`inode 203393620 -> 203394346`, `[D 1 §6]`), all four layers disagree about
  which image is current, and the live Space node is the one that answers `[D 1 §1]`. Reading
  `desktopImageURLForScreen:` is cheaper and is what `status` uses for
  `anchor_path`; parsing the store is left to the worker where `[D 5 §1.5]` says it lives.
- **Do not use AppleScript, and do not use `System Events`.** The AppleScript path via
  `System Events` loses the placement and needs the Automation TCC grant; the Finder path works
  and is what `[D 1 §4]` measured, but neither is the API, and the prototype's `/wh-commons.jpg`
  incident is a cwd bug that a relative path in an AppleScript string hides `[D 1 §4]`.
- **A sandbox breaks the probe at launch, and whirl is not sandboxed.** What `[D 1 §7]` measured is
  a trap before any of the probe's code ran: a binary carrying `com.apple.security.app-sandbox` and
  signed ad-hoc dies with `Trace/BPT trap: 5` (exit 133), AMFI rejecting the ad-hoc signature
  (`amfid: ... AppleMobileFileIntegrityError Code=-423`) and `AppSandbox` in `libsystem_secinit`
  `[D 1 V9]`. The same binary without the entitlement runs and sets the wallpaper, so the trap is
  the entitlement and not ad-hoc signing as such. The binary never reached a store write, so
  "cannot write the store" is not what was measured; and the research's own caveat is the one to
  carry, listed in 3.7: whether a *correctly signed* sandboxed build can call
  `setDesktopImageURL` at all is **unverified** `[D 1 §7, §8]`. Distribution is therefore Developer
  ID plus notarisation, and that is a v1.0 concern, not a v0.1 one.
- **Focus is not integrated and will not be** `[D 1 §8]`.

Cost: the LaunchAgent adds no process of its own; the floor is the daemon's 1.8 MB `[M 1]`.

### 3.3 Windows, stated precisely

`IDesktopWallpaper` is an out-of-process shell COM object (`CLSCTX_LOCAL_SERVER`), reached from the
`windows` crate with four Win32 feature flags, COM plus three more (`Win32_Foundation`,
`Win32_System_Memory`, `Win32_UI_Shell`) `[D 2 §1]`. It sets per monitor, reports the
current image per monitor, and can enable, disable and position the wallpaper. Three traps the
architecture has to handle:

- **Enumerate by device path, never by index.** `GetMonitorDevicePathAt` can return a monitor that
  is no longer attached `[D 2 §1]`, so the monitor identity the worker uses is the device path
  string, and a monitor that vanished between enumeration and the call is a per-monitor failure,
  not a retry on a shifted index.
- **Retry the class-not-registered case at logon.** `CoCreateInstance` can return
  `REGDB_E_CLASSNOTREG` while the shell is still coming up, and `[D 2 §1]`'s workaround is a short
  retry loop `[D 2 §4]`. The startup rotation therefore retries rather than reporting a failure the
  user would see once per boot.
- **Never run as a service.** Session 0 cannot change a user's wallpaper; the API fails with error
  1459 `[D 2 §4]`. The task is per-user and interactive, which is also why a scheduled task with
  `DisallowStartIfOnBatteries` at its default would silently not run on a laptop `[D 4 §Part 2]`;
  section 5 sets that flag.

`SetPosition` is system-wide, not per monitor `[D 2 §1]`, so `display.mode = per-display` on
Windows does not imply per-monitor positioning, and the config has no per-monitor position key for
that reason. `Enable(FALSE)` fills with a solid colour and `SetWallpaper` re-enables the desktop
`[D 2 §1]`; whirl never calls `Enable(FALSE)`.

Cost: one daemon, no helper. Each rotation is one out-of-process COM call per monitor, which
`[D 2 §1]` calls cheap at rotation frequency.

### 3.4 Linux, stated precisely

There is no single Linux way; there are five, and `[D 3 §Detecting the environment]` is explicit
that the detection signals are the session variables (`XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE`,
`SWAYSOCK`, `HYPRLAND_INSTANCE_SIGNATURE`), not the presence of a binary on `PATH`. whirl's worker
detects, then dispatches to one adapter:

- **GNOME (Wayland):** one `gsettings set` of `picture-uri` and `picture-uri-dark`. No per-monitor
  support, and the dconf database at `~/.config/dconf/user` is GNOME's, so whirl does not write it
  directly `[D 3 §GNOME 1, §GNOME 2]`. GNOME on X11 is on its way out (off by default in GNOME 49,
  dropped by Fedora, targeted for removal in GNOME 50) `[D 3 §GNOME 3]`, which is why the GNOME
  adapter is written for Wayland and only tolerates X11.
- **KDE Plasma 6 (Wayland and X11):** `plasma-apply-wallpaperimage` is a one-shot wrapper that sends
  a JavaScript snippet to `org.kde.PlasmaShell.evaluateScript` over the session bus and exits;
  `writeConfig(Image)` on each containment is what persists it, into
  `~/.config/plasma-org.kde.plasma.desktop-appletsrc`, which Plasma owns and rewrites on a clean
  shutdown and which whirl therefore never edits `[D 3 §KDE 1, §KDE 3]`. Three KDE-specific rules
  follow: a non-empty DBus error reply is a hard failure and never a success `[D 3 §KDE 1]`; when
  the scripting console is disabled (`[KDE Action Restrictions] plasma-desktop/scripting_console=false`
  in `kdeglobals`) the call fails with "Administrative policies prevent script execution", which is
  `ERR set_failed` with that message, not a silent no-op `[D 3 §KDE 1]`; and if `plasmashell` is not
  running the call fails with a service-not-found error that must not be reported as a bad image
  `[D 3 §KDE 4]`. All screens get the same image; KDE's stock tool loops every `desktops()` entry and
  per-monitor would mean depending on undocumented containment ordering, so per-monitor is out of
  scope for KDE `[D 3 §KDE 2]`. `org.kde.slideshow` is the one environment with a documented native
  rotation; delegating to it would forfeit history and source mixing, so it is not used
  `[D 3 §KDE 5, §Recommendation for v0.1]`.
- **sway:** `swaymsg output <name> bg <path> fill`, per output, and sway owns the `swaybg` process
  `[D 3 §The decisive column]`. A runtime `output bg` is not persisted by sway, so the startup
  rotation is what restores it after a compositor restart. whirl never starts `swaybg`.
- **Hyprland:** `hyprctl` fails if `hyprpaper` is not already running, and Hyprland's own
  directory rotation is the alternative `[D 3 §The resident helper]`. Deferred from v0.1.
- **Generic X11:** `feh --bg-fill`, `xwallpaper`, `hsetroot`; one-shot, no persistence, and `feh`
  writes `~/.fehbg` `[D 3 §Generic X11]`. `feh` also does not work under the GNOME shell, which is
  another reason the adapter detects rather than guesses.

Cost: no resident helper on the four supported environments. Where a helper exists (`swaybg`),
it belongs to the compositor, not to whirl.

**The v0.1 target set**, adopted from `[D 3 §Recommendation for v0.1]` without change: GNOME
(Wayland), KDE Plasma 6 (Wayland), sway, and generic X11 (`feh` or `xwallpaper`, for i3/openbox/
bspwm). Deferred, with the research's reasons: **Hyprland** (the only target where a one-shot
process cannot do the job, and the only one that would force whirl to own or depend on a resident
helper whose IPC surface changed within the past year), **GNOME on X11** (being removed upstream),
**per-monitor on GNOME** (impossible), **delegating rotation to the DE's slideshow** (hands
scheduling away and forfeits history), and **GNOME `.xml` slideshows** (undocumented, and a
rejected file leaves the user with no wallpaper and no error) `[D 3 §Recommendation for v0.1]`.

### 3.5 The optional lightweight UI

The UI is optional on every platform for the same structural reason: the daemon's protocol (section
2) is the only interface, and a frontend is an ordinary client of it, exactly like `whirl`.
Concretely:

- **Nothing in any platform's wallpaper API needs a UI process.** macOS needs an Aqua session, not a
  window `[D 1 §9]`; Windows needs an interactive session, not a window `[D 2 §4]`; the Linux
  adapters need a session bus or a compositor socket, not a window `[D 3 §Detecting the
  environment]`.
- **A frontend's footprint is its own, never the daemon's.** Because the daemon links no GUI
  toolkit `[R2]`, a menu bar item or a TUI cannot drag a toolkit into the resident process; `[M 8]`
  is the measurement of what happens when a single process owns both.
- **A frontend needs no privileges beyond the user's.** It reads the socket, which is `0600`
  (2.1), so it is the user's own process or it is not connected.
- **A TUI is a protocol client, not a second daemon.** It holds one `subscribe` connection and
  one command connection (2.3 step 5) and writes no state `[D 6 §7.1]`.

So the answer to "does the design need a UI" is no, and the answer to "can it have one cheaply" is
yes, in any language that can open a socket and read a line.

### 3.6 Platform limitations, stated as limitations

These are not backlog items and not "v0.1 doesn't do it yet". They are the platforms:

1. **macOS per-Space wallpaper, and therefore "the same image on every Space".** The store has no
   node meaning "every Space", `allSpaces` changes nothing, and the write lands on the frontmost
   Space `[D 1 §2]`. Making it true would need a resident observer of Space switches, which `R2`
   and `[D 5 §1.3]` rule out. What the user gets: the image on the Space they were on when the
   rotation ran, and on every display on that Space.
2. **Windows per-virtual-desktop wallpaper.** The documented API has no virtual desktop in it, and
   per-monitor and per-VD are mutually exclusive in the one interface that exposes it, which is
   undocumented and per-build `[D 2 §3]`.
3. **GNOME per-monitor wallpaper.** `gsettings` takes one URI, and there is no other supported
   route `[D 3 §GNOME 2]`.
4. **Hyprland needs someone else's resident process** `[D 3 §The resident helper]`, so it is out of
   v0.1's supported set rather than in it with a caveat.

### 3.7 What is unverified, and what would settle it

Every claim below is documentation-only or from a platform with no host on this machine. v0.1 fails
closed on each: the feature is refused or falls back, and the refusal is visible in `status`. This
list is the input to the test checklist, not an excuse.

| Unverified | Consequence in v0.1 | What settles it |
|---|---|---|
| macOS behaviour with two or more displays, and the fallback order when a display UUID is missing from the `Displays` dictionary `[D 1 §2]` | `display.mode = per-display` is accepted and runs as `all`, with `display_mode_reason: unverified_platform` in `status`, which is `[D 5 §1.3]`'s fallback for an unanswered platform | two displays, or a second virtual display; run the readback and compare per screen |
| Whether a sandboxed, notarised build can write the store `[D 1 §7]` | irrelevant to v0.1: no sandbox, and distribution is deferrable | one notarised build with `com.apple.security.app-sandbox` |
| Windows per-monitor behaviour, the DACL on the named pipe, Job Object process containment, `REGDB_E_CLASSNOTREG` timing `[D 2 §1, §4]`, `[D 6 §1.3]` | `per-display` ships on the documented API alone because `[D 5 §1.3]` says a platform where the research found it reachable is honoured, and this is the one v0.1 feature nobody has run; the pipe DACL (2.1) and the Job Object rule (1.7.1) are specified and equally unrun | one real Windows machine, or a VM with a desktop session |
| Whether the OS slideshow and whirl interfere on Windows `[D 2 §5]` | not detected: whirl cannot see the slideshow's state, so `config check` warns only if `Enable` reports it | a Windows machine with the slideshow on |
| Linux per-monitor and readback per environment `[D 3 §The decisive column]`, `[D 5 §1.5]` | `per-display` is honoured on sway and generic X11 (per output, per `--output`), refused on GNOME as impossible and on KDE as out of scope, each with its own reason; `anchor_verified: 0` until a rotation completes | one session per environment, or a matrix VM |
| Hyprland's helper requirement and its API stability `[D 3 §The resident helper]` | not a v0.1 target | a Hyprland session |

---

## 4. Config file

### 4.1 Format, and why JSON stays

**JSON, UTF-8, at the platform path `[D 6 §1]` gives: `~/Library/Application Support/whirl/config.json`,
`$XDG_CONFIG_HOME/whirl/config.json`, `%APPDATA%\whirl\config.json`.** Three reasons, one of them
measurable:

1. **Both readers already exist and neither needs a dependency.** The prototype validated and used
   this file with the worker's `encoding/json` and the daemon's own scan `[M 7]`. The daemon's rule
   is a zero-dependency binary `[M 3]`, and TOML would add a parser plus a serialiser to satisfy
   one flat scalar table and one flat array of flat objects. `decision:` JSON, with the daemon's
   parser handling the full string escape set (`\"`, `\\`, `\n`, `\t`, `\r`, `\b`, `\f`, `\uXXXX`)
   because Windows paths are backslash-heavy `[D 2 §2]` and a parser that only handles `\"` will
   corrupt a `%APPDATA%` path. The daemon parses only the top-level scalars it needs (`[D 5 §2.0]`);
   the sources array is validated by the worker.
2. **A frontend in another language reads it for free**, which is the frontend contract's whole
   premise (section 8).
3. **Annotated without a second file.** `[D 5 §F1]` wants the generated config "fully commented";
   JSON has no comment syntax. `decision:` any object key whose first character is `_` is a
   comment, ignored by every whirl parser, at every level. The file stays valid JSON for every
   other tool, the generated file is annotated in place, and the cost is one reserved key prefix,
   named in 4.3 as the one rule a user has to remember.

The two legacy names `[D 6 §9]` adds are accepted and deprecated: `keep` is an alias for
`cache.max_files`, and `cache_dir` is an alias for `cache.root` (absolute paths only). Both log a
deprecation warning, neither is written back, and `_`-prefixed keys are the only other accepted
non-schema keys.

### 4.2 The annotated example

This is the file the daemon writes when `config.json` is missing `[D 6 §8.7]`, with the annotations
in place and every key at its default. There is no `whirl config init` verb: `[D 5 §1.1]`'s set is
closed and `[D 6 §8.7]` puts the write on the daemon. It parses as JSON: `[L 6]` is that check.

```json
{
  "_comment_1": "whirl configuration. JSON, UTF-8, no comments in the syntax: any key whose first",
  "_comment_2": "character is an underscore is a comment and is ignored by every whirl parser, at",
  "_comment_3": "every level. Every key below is written at its default value, so deleting a line",
  "_comment_4": "keeps the default and there is no key whose absence means something different.",
  "_comment_5": "Precedence, lowest first: compiled defaults, this file, the environment",
  "_comment_6": "(WHIRL_CONFIG, WHIRL_SOCKET, WHIRL_STATE_DIR, WHIRL_CACHE_DIR, WHIRL_BACKEND,",
  "_comment_7": "WHIRL_WALLHAVEN_API_KEY), then daemon flags, which exist for tests only.",
  "_comment_8": "Two legacy names are still accepted and log a deprecation warning: `keep` for",
  "_comment_9": "cache.max_files and `cache_dir` for cache.root. Neither is written back.",

  "config_schema": 1,
  "_comment_config_schema": "Bumped only when a key changes meaning. A file with a newer value is read, reported in status.state_schema_newer, and not rewritten.",

  "socket": null,
  "_comment_socket": "null means the platform default: ~/Library/Application Support/whirl/whirl.sock on macOS, $XDG_RUNTIME_DIR/whirl.sock on Linux (falling back to $XDG_STATE_HOME/whirl/whirl.sock), \\\\.\\pipe\\whirl-<user> on Windows. An absolute path here is used verbatim. The daemon refuses to start if the resolved path does not fit the platform's sun_path (104 bytes on macOS).",

  "log_level": "info",
  "_comment_log_level": "off | error | warn | info | debug. The log is a file the daemon owns; it is never an interface.",

  "schedule": {
    "interval_seconds": 1800,
    "worker_deadline_seconds": 300,
    "_comment_interval_seconds": "The rotation interval. The OS scheduler starts the daemon; this timer lives in the daemon and the deadline is a wall-clock comparison, so a laptop that slept through three slots rotates once on wake and then returns to the grid.",
    "_comment_worker_deadline_seconds": "A worker that has not finished in this many seconds gets SIGTERM, then SIGKILL 5 s later, and the slot is spent. 300 s is ~90x the measured worst case and covers a fully expired download."
  },

  "startup": {
    "enabled": true,
    "mode": "last",
    "respect_manual": true,
    "_comment_enabled": "Rotate once when the daemon starts, so a login restores the wallpaper.",
    "_comment_mode": "last | rotate. `last` re-applies the newest history entry and spends no download.",
    "_comment_respect_manual": "When the platform can read back the current image and it is not the one whirl set, record it as an `external` history entry and do not overwrite it. Degrades to false, reported as status.respect_manual_effective, where the platform cannot read back."
  },

  "display": {
    "mode": "all",
    "_comment_mode": "all | per-display. `all` is one image on every display. `per-display` is honoured only where the platform documents a per-display setter a one-rotation worker can reach (Windows, sway, generic X11) and is refused in config check elsewhere: GNOME cannot do it, KDE is out of scope, and macOS is unverified, where it is accepted and runs as all with status.display_mode_reason naming why."
  },

  "min_width": 1600,
  "min_height": 900,
  "_comment_min_width": "The shared resolution floor, applied by reading the image header. The specs name no global default; this is the prototype's value (prototype/wh-rotate/example-config.json), and most users should set it to their own panel's resolution. A source may override it with its own min_width/min_height.",

  "filters": {
    "max_bytes": 41943040,
    "ratio_tolerance": 0.02,
    "target_ratio": null,
    "_comment": "max_bytes is 40 MiB, the largest file the pipeline will admit. ratio_tolerance is a fraction of the target ratio. target_ratio null means any, or the primary display's ratio where the worker can read it."
  },

  "state": {
    "history_entries": 50,
    "_comment_history_entries": "The history ring. 50 entries at roughly 341 bytes each in the index, so the daemon's resident state stays ~8 KB."
  },

  "dedupe": {
    "recent_entries": 50,
    "_comment_recent_entries": "How many recent entries a candidate is compared against. Larger than the ring it is not, and a larger window costs only hashes."
  },

  "cache": {
    "root": null,
    "max_bytes": 2147483648,
    "max_files": 500,
    "grace_seconds": 600,
    "orphan_grace_seconds": 300,
    "_comment_root": "null means the platform cache root (~/Library/Caches/whirl, $XDG_CACHE_HOME/whirl, %LOCALAPPDATA%\\whirl\\cache). An alias `cache_dir` is also accepted, absolute paths only. config check refuses a root whose filesystem does not support flock, and reports lock_mode: excl_file if a weaker lock had to be used.",
    "_comment_max_bytes": "2 GiB. Whichever of the two caps binds first evicts. config check refuses cache.max_bytes < filters.max_bytes, because that config has a guaranteed permanent overshoot.",
    "_comment_max_files": "500. Both caps exist because a count cap does not bound bytes: the same 500 files span 60x in size across samples, and 500 files is ~1.5-1.9 GB at the sampled means.",
    "_comment_grace_seconds": "A cache file newer than this is never reclaimed, so an in-flight set cannot have its file deleted underneath it.",
    "_comment_orphan_grace_seconds": "How long a file with no index entry survives before the sweep reclaims it. This is what covers a worker killed between its rename and its report."
  },

  "backend": "native",
  "_comment_backend": "native | noop. `noop` runs every stage except the platform setter, so a whole rotation can be exercised in a test or in CI without touching the desktop. Also settable as WHIRL_BACKEND=noop or --backend noop.",

  "sources": [
    {
      "_comment_kind": "local: one or more directories, filtered by a header read and a glob. The source entries below are the two schemas from docs/spec/features.md 2.2 and 2.3, at their defaults.",
      "id": "pictures",
      "kind": "local",
      "weight": 1,
      "capabilities": ["resolution", "extension"],
      "paths": ["~/Pictures/Wallpapers", "/Volumes/Media/walls"],
      "recursive": true,
      "max_depth": 8,
      "follow_symlinks": false,
      "include": ["*.jpg", "*.jpeg", "*.png", "*.heic", "*.webp"],
      "exclude": ["*/.git/*", "*/screenshots/*", "*.tmp"],
      "min_width": 2560,
      "min_height": 1440,
      "mode": "reference",
      "_comment_id": "Unique, and the prefix of every origin_key this source produces. [A-Za-z0-9._:-]+, at most 64 bytes, because a space here would break the line protocol's framing.",
      "_comment_weight": "Relative chance of being chosen per rotation. 0 disables the source without deleting it. The daemon normalises; it does not need the weights to sum to anything.",
      "_comment_capabilities": "What the source can answer. Defaults to the kind's own list and may be narrowed, never widened: config check refuses a capability the kind does not implement. A capability not declared is applied by the shared pipeline instead, which is why the result set is the same either way.",
      "_comment_paths": "~ is expanded. A path that is missing or unreadable is a warning; it is an error only if every path fails.",
      "_comment_max_depth": "Bounds a runaway tree, such as a symlinked home directory, at a predictable scan.",
      "_comment_follow_symlinks": "Off by default so that loops are impossible rather than merely unlikely.",
      "_comment_include": "Matched against the path relative to the source root. Exclude wins. A file whose header cannot be read is excluded and logged at debug, never guessed at from its name.",
      "_comment_mode": "reference sets the wallpaper from the original path, and a deleted file becomes a broken wallpaper with no error at the time it breaks. copy copies into the cache first, which is what a removable volume or a network share needs."
    },
    {
      "id": "space",
      "kind": "wallhaven",
      "weight": 3,
      "capabilities": ["resolution", "ratio", "purity", "colors", "category"],
      "query": "space nebula",
      "categories": "111",
      "purity": "100",
      "sorting": "random",
      "order": "desc",
      "ratios": "16x9,16x10",
      "atleast": "2560x1440",
      "colors": null,
      "top_range": "1M",
      "collection": null,
      "pages": 1,
      "api_key_ref": null,
      "_comment_query": "The site's tag syntax, including +tag, -tag, @user, id:, type:, like:.",
      "_comment_categories": "Three flags: general, anime, people. 111 is all three.",
      "_comment_purity": "100 sfw, 110 sketchy, 111 nsfw. NSFW needs a valid key and fails 401 without one.",
      "_comment_sorting": "date_added | relevance | random | views | favorites | toplist.",
      "_comment_ratios": "Pushed down as `ratios`, and also applied by the shared pipeline with filters.ratio_tolerance.",
      "_comment_atleast": "Pushed down as `atleast`; the pipeline still enforces min_width x min_height, because the source's own admission is not the whole filter.",
      "_comment_top_range": "Only meaningful with sorting=toplist.",
      "_comment_collection": "A path segment instead of a search: /api/v1/collections/USERNAME/ID. That endpoint exposes only the purity filter, so every other filter is applied locally.",
      "_comment_pages": "How many 24-result pages to sample. Default 1, hard cap 5: one API call per page against a documented 45 requests per minute.",
      "_comment_api_key_ref": "A NAME, never a value. null means the key is looked up in the platform's own store, in this order: WHIRL_WALLHAVEN_API_KEY, then keychain item `whirl-wallhaven` on macOS, `secret-tool lookup service whirl key wallhaven` on Linux, Credential Manager generic credential `whirl/wallhaven` on Windows. A key in this file is a bad_config refusal, because a key in a config file is a key in every backup of that file."
    }
  ]
}
```

### 4.3 Precedence and validation

Order, later wins:

1. compiled defaults (every value in 4.2),
2. the config file,
3. the environment: `WHIRL_CONFIG` (config path), `WHIRL_SOCKET`, `WHIRL_STATE_DIR`,
   `WHIRL_CACHE_DIR`, `WHIRL_BACKEND`, and `WHIRL_WALLHAVEN_API_KEY` (the key itself, never a path
   to it),
4. command-line flags on the daemon and the worker, which exist only for tests and for the
   `--backend noop` switch `[D 5 §2.4]` needs: `--config`, `--socket`, `--backend`.

Rules, all of them checked before the daemon serves a request:

- **A key that is not in the schema is a warning, not an error**, named with its line in the log.
  A typo in a key must not stop a wallpaper rotator from starting.
- **A value that is out of range, or an ordering rule that is violated, is a refusal to start**
  `[D 6 §8.7]`, with the key and the two values named in the message. The ordering rules are:
  `cache.max_bytes >= filters.max_bytes` (the cache must hold at least one admissible image),
  `cache.max_files >= 2`, `state.history_entries >= 1`, `dedupe.recent_entries >= 1`,
  `schedule.interval_seconds >= 60`, and `schedule.worker_deadline_seconds >= 60`.
- **A source that fails the daemon's structural check refuses startup; a source that fails the
  worker's semantic check is disabled, not fatal.** `decision:` the split, and the reason it is
  needed: `[D 6 §8.7]` says a config that fails `whirl config check` is a config the daemon refuses
  to run, while `[D 5 §2.0]` says the daemon parses only the scalars it needs and the worker
  interprets the sources. Structural facts are the daemon's, because a config error the user must
  fix by hand cannot be discovered by a process that has already started serving: an unknown
  `kind`, an `id` outside `[A-Za-z0-9._:-]+` or over 64 bytes, a missing required key, a negative
  `weight`. Semantic and environmental facts are the worker's, because they are transient and one
  bad folder must not stop the rotator: a path that does not exist, a missing API key, a filter that
  removes everything. The second group appears as `source: <id> ... enabled=0 reason=<...>` in
  `status`, `sources` and `config check`, and in the log; the first group is a `bad_config` refusal
  naming the key, and `whirl config check` reports it with the same message because they are one
  implementation `[D 6 §8.7]`.
- **`display.mode = per-display` is accepted and refused per platform** (3.7): the file parses, the
  daemon starts, `display_mode_effective: all` and a `display_mode_reason` name why. That is
  `[D 5 §1.3]`'s fallback, with the reasons now known per platform.
- **The daemon re-reads the config on every rotation** `[D 5 §F0]`, and a successful re-read emits
  `config_reloaded` on the subscribe stream. A failed re-read keeps the previous config and writes
  `last_error: bad_config` into the log; it does not stop the daemon, because a running daemon with
  a stale schedule beats no rotator at all.

## 5. Scheduling decision

### 5.1 The split, adopted verbatim

> **The OS scheduler owns the process. The daemon owns the clock. Neither platform's OS scheduler
> is given the rotation schedule.** `[D 4 §Part 3]`

Identically on all three platforms, and for the same reason each time: every OS interval trigger
has a floor or a failure mode that a wallpaper rotator cannot live with, while every OS supervisor
is good at exactly one thing, starting a process again after it dies.

- launchd `StartInterval` is floored at 10 s by the default `ThrottleInterval` (measured: 5 and 1
  both fire every 10.1 s) and **loses firings across sleep** `[D 4 §Part 1, §Part 2]`.
- The Task Scheduler delivers a missed start about ten minutes after wake, and only if
  `StartWhenAvailable` is set; `WakeToRun` depends on the power plan `[D 4 §Part 1]`.
- systemd `OnUnitActiveSec` is monotonic and pauses across suspend; `OnCalendar` catches up but
  cannot express an arbitrary interval, and its `AccuracySec` defaults to a minute
  `[D 4 §Part 1]`.

A daemon that is running and comparing wall-clock time rotates within its poll slice of wake on all
three, with no configuration and no wake timer `[D 4 §Part 2]`. That is the whole argument, and it
is why all three platform sections below configure exactly one thing: start at login, restart on
death.

### 5.2 macOS

Ship `~/Library/LaunchAgents/com.guruor.whirl.plist` with `RunAtLoad` true, `KeepAlive` true,
`StandardOutPath`/`StandardErrorPath` pointing at the log, and **no `StartInterval` and no
`StartCalendarInterval`** `[D 4 §Part 3 macOS]`. `KeepAlive` implies `RunAtLoad`, so one key gives
login start plus crash restart; the measured restart cadence is 10.1 s, floored by
`ThrottleInterval` `[D 4 §Part 1]`, which is the interval the user should expect to see in
`uptime_s` after a crash and not a bug. A daemon killed with `SIGKILL` is restarted on the same
cadence `[D 4 §Part 1]`.

`Decision:` the plist carries no schedule, no `EnvironmentVariables` block beyond what 1.6 needs,
no `StartInterval`, no calendar entry, and no `WatchPaths`. `launchctl kickstart -k` is a
supervisor action that restarts the daemon and is not the user-facing "rotate now"; `whirl next`
over the socket is `[D 4 §Part 3 macOS]`.

Why this and not launchd's own timer, in one line each: `StartInterval` loses the firing when the
lid was shut, which is the common case on the machine this is for; `StartCalendarInterval` catches
up and coalesces but cannot express "every N minutes" for an arbitrary N `[D 4 §Part 3 macOS]`.

### 5.3 Windows

One per-user task, created at logon, marked interactive-only, with
`-MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Seconds 0)` and a
`RestartOnFailure` policy `[D 4 §Part 3 Windows]`. `ExecutionTimeLimit` `PT0S` matters: the default
is 72 hours, after which the Task Scheduler kills the daemon `[D 4 §Part 1]`, and a wallpaper
rotator that stops every three days is the exact class of bug this document exists to prevent.

Three keys are deliberately left at their defaults because the default is the right behaviour here:

- `StartWhenAvailable` stays **false**: a missed rotation is not worth a run ten minutes after the
  laptop opens its lid, and the daemon's own deadline already does the right thing on wake
  `[D 4 §Part 1, §Part 3 Windows]`.
- `WakeToRun` stays **false**: waking a machine to change a wallpaper is not worth the battery
  `[D 4 §Part 1]`.
- `DisallowStartIfOnBatteries` is explicitly set to **false**, overriding its `true` default, or
  whirl would silently not run on a laptop on battery `[D 4 §Part 1]`.

There is no time trigger on the task, on purpose: the OS scheduler's job is that the process
exists, not that the rotation happened.

### 5.4 Linux

A `systemd --user` unit `whirl.service` with `Restart=always` and `RestartSec=5`, installed with
`systemctl --user enable --now whirl.service` `[D 4 §Part 3 Linux]`. No timer unit, so no
`OnCalendar`, no `OnUnitActiveSec`, no `AccuracySec` question. `loginctl enable-linger` is **not**
required and is not recommended in v0.1: a rotation with no session has nothing to set, and the
only thing that would benefit is housekeeping `[D 4 §Part 2]`. If housekeeping off-session is ever
wanted, it is a second unit rather than a linger flag on this one.

### 5.5 The daemon's clock

The rules, all of which follow from `[D 4 §Part 3 macOS]`'s wall-clock rule and
`[D 6 §8.6]`:

1. **The deadline is a wall-clock comparison.** `next_at` is an absolute UTC time persisted in
   `state/current.json`; the scheduler wakes in bounded slices (a few seconds) and fires when
   `now >= next_at`. That is what makes "the machine slept through the slot" visible; a monotonic
   clock pauses across suspend and would make it invisible `[D 4 §Part 3 macOS]`.
2. **Durations are monotonic.** Timeouts, the heartbeat interval and the worker escalation use a
   monotonic clock. `[D 6 §8.6]`.
3. **The wait is never a single sleep to the deadline.** `[D 6 §8.6]` names the asymmetric failure
   of the composed form: a backward clock jump makes the difference large and positive, so
   rotations stall silently. The daemon therefore sleeps in slices of a few seconds, uses the
   monotonic clock for the slice length, and re-tests with rule 1's comparison, which is also
   `[D 4 §Part 2]`'s requirement that waking from sleep produce a rotation rather than a burst.
   The prototype got this half right: `[M 18]`, `whd.rs:184-191`, already sleeps in slices clamped
   to 1-20 s and says why in its own comment; what it got wrong was the deadline it advances
   afterwards, rule 4.
4. **A missed or failed slot advances `next_at` by whole intervals, and a failed slot must not
   re-arm inside itself.** On any outcome, success or failure:
   `while next_at <= now { next_at += interval }`, then persist. Not `next_at = now + interval`.
   Two prototype bugs are the basis. `[M 14]`: it re-anchors at the end of the rotation, so every
   late rotation pushes the whole schedule late by the rotation's duration plus the scheduling
   slack, and the schedule drifts away from the grid the user configured. `[M 17]`: the error path
   never touches `next_at` at all, so a failing rotation re-queues every 1-20 s forever and a broken
   source becomes a permanent retry storm against the API it just failed on. The whole-interval
   advance fixes both and gives the sleep case its shape: ten missed slots collapse into one
   rotation on wake with `next_at` in the future, which is what `[D 4 §Part 2]` asks for and what
   `[D 6 §8.6]` means by "missed slots are not replayed".
5. **A clock jump is announced, not absorbed.** `[D 6 §8.6]`'s mechanism, adopted: compare the
   wall-clock delta between two consecutive scheduler observations against the monotonic elapsed
   time; beyond `2 * interval` the daemon logs one line, sets `clock_jump: <n>` in `status`, and
   re-anchors `next_at` to the current wall clock plus `interval`, and the subscribe stream carries
   a `clock_jump` event with the delta. Re-anchoring is only ever correct here, because the jump
   means the relationship between the deadline and reality is unknown; in the ordinary late-slot
   case rule 4 is what applies.
6. **A restart re-derives, it does not re-schedule.** On start the daemon loads the persisted
   `next_at`: if it is in the future it waits, and if it is in the past it rotates once and then
   advances by rule 4. That is what keeps the schedule stable across the crash restart the OS
   supervisor performs at 10.1 s cadence `[D 4 §Part 1]`.
7. **Logout stops it, on purpose.** A `LaunchAgent` stops at logout, an interactive-only task stops,
   and a `systemd --user` service stops without linger; none of the three wallpaper APIs works
   without a session, so this is the correct behaviour rather than a gap `[D 4 §Part 2]`.
8. **A pause freezes the deadline, and a resume re-arms it from now.** While `paused: 1` no rotation
   starts and `next_at` is not advanced, so a pause is not a pile of missed slots waiting at the end
   of it. `resume` writes `next_at = now + interval` and persists it before answering, which is
   `[D 5 §F4]`'s rule ("`resume` re-arms the next slot from now") and what the prototype already
   implements (`whd.rs:377`). Rule 4's whole-interval advance does not apply here: the user asked
   for a slot from now, not for the old grid to be honoured. The consequence a client can see:
   `next_in_s` is `-` while `paused: 1` (2.10), because no deadline is running, and after `resume`
   it is the full interval.

## 6. Security model

### 6.1 The trust boundary is the user account

`whirl` runs as the logged-in user, with the user's authority and no more. It is not setuid, it does
not run as root or SYSTEM, it has no privileged helper, and it needs none: every platform's
wallpaper API is reachable from an ordinary process in the user's session `[D 1 §3]`,
`[D 2 §4]`, `[D 3 §The decisive column]`. It writes only inside the three per-user directories
`[D 6 §1]` fixes, and reads only the paths the user put in the config or named on the command line.

What that boundary does and does not buy:

- **It does buy** that no other account can reach the daemon. The socket is `0600` in a `0700`
  directory (2.1), the Windows pipe's DACL grants the owning user only, and there is no TCP
  listener anywhere. A different user cannot rotate the wallpaper, read the history, read the cache
  or pause the schedule.
- **It does not buy** anything against a process running as the same user. That process can already
  set the wallpaper directly, read the cache, and write the state files. It can also talk to the
  daemon, which means it can rotate the wallpaper, pause the schedule and read the history. This is
  inherent, and stating it is more useful than pretending the socket is an authentication boundary
  it cannot be.
- **It is not a privilege boundary at all.** A worm in the user's session gains nothing from whirl
  that it did not already have; that is also why there is no token, no password and no TLS. A local
  socket with filesystem permissions is the strongest thing that fits the threat, and a
  shared-secret scheme over a Unix socket is security theatre.

### 6.2 Hardening that follows from the above

| Measure | Why | Basis |
|---|---|---|
| umask `0077` around `bind`, then `fchmod 0600` | measured: a socket bound without a `chmod` is `0755` at this machine's umask, so it is connectable by others between `bind` and `chmod` | `[L 3]` |
| state, cache and log directories `0700`, files `0600` | state files carry the user's wallpaper paths and rotation history | `[D 6 §1]` |
| peer UID checked on each accepted connection (`getpeereid` on macOS, `SO_PEERCRED` on Linux; the pipe DACL on Windows) and refused otherwise | defence in depth: the mode check depends on the filesystem behaving, and the peer check does not | `decision:`, mechanism `[L 1]` shows the socket is a filesystem object |
| stale socket unlinked only after a failed connect probe | unlinking a live daemon's socket leaves it running and unreachable, then lets a second daemon take the path | `[M 15]` |
| one daemon enforced by `state/locks/daemon.lock` | two daemons would each write `current.json` and each spawn workers | `[D 6 §7.2]` |
| every state write is temp + fsync + rename | 3068 of 3508 concurrent reads of an in-place rewrite saw a truncated file | `[D 6 §6.3 L 5]` |
| the worker's environment is scrubbed, not inherited | a per-rotation process should not carry the session's whole environment into a process that logs its errors | `[M 16]`, 1.6 |
| the log holds no secret and no image bytes | the log is 0600, but it is also copied into bug reports | `[D 6 §7.2]` |
| no `Enable(FALSE)`, no store edits outside the API, no direct `dconf` or `appletsrc` writes | the platform owns those files and rewrites them; editing them is how a rotation "does not stick" | `[D 2 §1]`, `[D 3 §KDE 3, §GNOME 1]` |

### 6.3 The Wallhaven API key

The key is the only secret whirl ever handles. Rules, from `[D 5 §2.4]`:

- **Three places it may come from, in order:** the environment variable
  `WHIRL_WALLHAVEN_API_KEY`; the platform's own store (`security find-generic-password -s
  whirl-wallhaven` on macOS, `secret-tool lookup service whirl key wallhaven` on Linux, Windows
  Credential Manager generic credential `whirl/wallhaven`); or a `0600` file the config points at.
- **The config file holds a reference, never a value.** `api_key_ref` is a label such as
  `keychain:whirl-wallhaven`. The generated config has `null`, and a config containing something
  that looks like a key is a `bad_config` refusal with the key named, because a key in a config file
  is a key in every backup of that file.
- **It is never in argv.** The worker gets it in its environment (1.6), not as a parameter, so it
  does not appear in `ps` output for any user on the machine.
- **It travels in the `X-API-Key` header, never as a query parameter.** A query parameter lands in
  proxy logs and in the `Referer` of any link the page contains; a header does neither.
- **It is never echoed.** Not in `status`, not in `sources`, not in an `event`, not in an `ERR`
  message, not in the log, not in the cache index, not in a filename. An error from the API that
  contains the key (or the URL with the key in it) is rewritten before it is logged: the daemon logs
  the status code and the API's error code, not the raw URL. `whirl config check` reports whether a
  key was found and from where, never the value.
- **A missing key disables the source, it does not fail the daemon.** `source: <id> wallhaven
  weight=3 enabled=0 last=- reason=no key at keychain:whirl-wallhaven`, and the local sources keep
  rotating `[D 5 §2.4]`.

### 6.4 `set path` references rather than copies

`decision:` when a client sends `set path <file>`, the platform is pointed at the user's own file,
which is the same default as a local source's `mode: reference` `[D 5 §2.2]`, and the `set:` response
names that path. The documented consequence applies unchanged, and it is worth stating in the
document rather than discovering later: a wallpaper whose file is deleted becomes a broken wallpaper
with no error at the time it breaks, because macOS returns `NO` with `The file doesn't exist.` when
its write target has been deleted `[D 1 V5c]`. That measurement is why `mode: copy` exists in the
config `[D 5 §2.2]`, and a user who wants the copy semantics gets them by putting the file in a
`local` source with `mode: copy`, or by `whirl favorite`-ing an entry the pipeline already cached.
`set path` on a file inside a `local` source's tree is not special-cased: it becomes a `manual` entry
in history and `prev` walks back through it like any other.

## 7. Failure modes

Every row is a complete answer: what the daemon does, and what the user sees. The user's view is the
`whirl` output and the `status` keys, because those are the only two surfaces.

| # | Failure | What the daemon does | What the user sees |
|---|---|---|---|
| 1 | **The daemon is not running when a client calls** | Nothing; it does not exist. The CLI does not start it `[D 5 §1.1]` | `whirl: cannot reach the daemon at <path>` on stderr, exit 2. If the socket file exists, the message adds `(stale socket; the daemon is not running)`. A stale socket is never unlinked by the client, only by the daemon after a connect probe `[M 15]` |
| 2 | **The worker cannot set the wallpaper** | The failed candidate is counted, one further candidate from the same or the next source is tried within the same slot `[D 5 §1.4]`, the slot is consumed either way (5.5 rule 4), and the cache entry from the failed attempt stays until the next sweep | `ERR set_failed <platform message>` to the client, `last_error: set_failed`, and the previous wallpaper is still on screen. On macOS the message is the platform's own, e.g. `The file doesn't exist.` `[D 1 V5c]`; on KDE a non-empty DBus error is reported verbatim `[D 3 §KDE 1]` |
| 3 | **No candidates** (empty folders, everything filtered out, every source disabled) | Every configured source is tried once for that slot; nothing is downloaded and nothing is set; the slot is consumed; no backoff timer is invented. `whirl config check` is the diagnostic, and it names the stage that removed the candidates `[D 5 §2.5]` | `ERR no_candidates no source produced an admissible image`, exit 1, `last_error: no_candidates`, and `source: ... reason=...` lines in `status` explaining which source was empty and why |
| 4 | **Disk full** | The download dies at `enospc`, the part file is removed if the filesystem allows it at all, and `cache_writable: 1` stays true. The sweep still runs, because deletions free space even on a full disk, and a sweep that cannot rewrite `index.json` reports `cache_over_reason: sweep_error` and retries at the next rotation `[D 6 §8.1]`. `sweep_deferred` is not this key: it has exactly one meaning, the rotation lock was already held by someone else `[D 6 §5.5]` step 1 | `ERR enospc <path>` once, exit 1. A state write that fails leaves the previous state file intact (temp + rename `[R5]`) and logs `state write failed: <errno>`; the daemon keeps running with the last good state rather than refusing to serve. If the sweep could not rewrite the index, `status` carries `cache_over_reason: sweep_error` until one succeeds, and `cache_over_cap` reports the overshoot. Nothing is deleted to make room: the caps are not raised, and no user file outside the cache is touched |
| 5 | **Network down** | `wallhaven` sources fail at connect; local sources are still tried in the same slot, and if a local source wins, the rotation succeeds. If every source is network-dependent, the slot is consumed and the next one retries | `ERR offline` if nothing could be served, exit 1, `last_error: offline`. With a local source present: a normal successful rotation and `source: <id> wallhaven ... last=offline reason=connect: Network is unreachable` in `status` |
| 6 | **A display is disconnected mid-rotation** | The worker enumerates displays at the start of the setter step and keys them by identity, not index: display UUID on macOS `[D 1 §2]`, device path string on Windows `[D 2 §1]`, output name on sway `[D 3 §The decisive column]`. A display that vanished between the fetch and the set fails that display's call and nothing else; the remaining displays are set; there is no index-based retry | `ERR set_failed` only if every display failed; otherwise a successful rotation whose `set:` line names the path, plus a per-display failure line in the log. A display connected later gets the image at the next rotation `[D 5 §1.3]` |
| 7 | **The worker hangs** | 300 s deadline, `SIGTERM`, 5 s, `SIGKILL` (1.7.1); `rotate.lock` is released by the kernel on exit, or removed by the daemon where the `excl_file` fallback is in force `[D 6 §7.2]` `[D 6 §8.8]`; the slot is consumed | `ERR timeout`, exit 1, `last_error: worker_timeout`, and the daemon is still answering `status` while all of this happens |
| 8 | **The worker is killed mid-rotation** (OOM, user, supervisor) | Part file or unreported cache file is reclaimed by the sweep after its grace window `[D 6 §5.5]`; the anchor is reconciled by 1.7.3 if the setter had already succeeded | `ERR worker_failed` (or `ERR timeout` if the deadline was the cause), exit 1; nothing on screen changes unless the setter had already run, in which case the wallpaper did change and `status` reports `last_via: recovered` |
| 9 | **The state directory is not writable** | The daemon refuses to start, with the directory and the `errno` in the message `[D 6 §8.5]` | `launchctl`/`systemctl`/Task Scheduler log or the whirl log carries `state dir not writable: <path> (EACCES)`; every `whirl` verb exits 2 because there is no daemon |
| 10 | **The cache directory is read-only** | The daemon starts, `cache_readonly: 1`; local sources in reference mode can still rotate to a file already present, and nothing new is admitted `[D 6 §8.4]` | `whirl next` gives `ERR cache_readonly <path>` if nothing usable is cached; `status` shows `cache_readonly: 1` and a reason |
| 11 | **`favorites.json` is corrupt** | It is quarantined, the cache protects the recovery window's files, and pin-changing verbs are refused while reads keep working `[D 6 §6.4]` | `ERR favorites_degraded <quarantine path>` from `whirl favorite`, `status` shows `favorites_degraded: 1` and the quarantine path |
| 12 | **Two clients ask for a rotation at once** | The first takes the slot; the second gets `ERR busy` immediately (1.8). Nothing is queued | `whirl next` prints `whirl: busy: a rotation is already in flight` and exits 1; the first client's rotation completes normally |
| 13 | **The config is invalid** | At startup, a refusal to start naming the key `[D 6 §8.7]`. On re-read, the previous config stays in force, the failure is logged, and the daemon keeps rotating | `status` shows the old values; the log has `<key>: <value> is out of range`; `whirl config check` prints the same and exits 1 |
| 14 | **`per-display` is not reachable on this platform** | Honoured where the research found a documented per-display setter (Windows), accepted and run as `all` where the answer is unverified (macOS, and sway and generic X11 honour it per output and per `--output`), refused at `config check` where it is impossible (GNOME) or out of scope (KDE) `[D 5 §1.3]`, `[D 2 §1]`, `[D 1 §2]`, `[D 3 §GNOME 2]`, `[D 3 §KDE 2]` | `status` shows `display_mode`, `display_mode_effective` and `display_mode_reason`: `unverified_platform` on macOS, `impossible_on_this_desktop` on GNOME, `out_of_scope_on_this_desktop` on KDE; `whirl config check` prints the same reason and exits 1 on the two that are refusals |
| 15 | **Windows, with per-virtual-desktop wallpapers active** | Nothing it can detect, and nothing it can query. `IDesktopWallpaper` does not model virtual desktops, and Windows treats per-desktop background mode and per-monitor wallpaper mode as mutually exclusive, so a set made while the user has several desktops open either lands on the desktop that is active at that moment or is silently reverted by the shell. The call returns success and the readback agrees for the desktop the daemon can see, so there is no `ERR`, no `last_error` and no `anchor_verified: 0`; the rotation is logged as an ordinary success `[D 2 §3]`, whose sources are [19], [30] and [35] (the [35] report is an open proposal, cited there only as corroboration of the shape, not as proven behaviour) | The new image appears on the desktop the user is on when the rotation runs; after switching desktops the previous image is back, or the change reverts on its own, with no error anywhere: `whirl next` exits 0, `status` shows `last_error: -`, and the log has nothing to say. `whirl config check` cannot warn either, because the mode lives in the shell and not in the config. v0.1 cannot detect it and does not pretend to; the check that would settle it is `[D 2 §3]`'s "switch between desktops" step on a real Windows machine |
| 16 | **`daemon.lock` was left behind by a daemon that is gone** (only where the `excl_file` fallback is in force, `[D 6 §8.7]`) | Refuses to start at 1.5 step 1 and does not touch the file: taking it over needs a compare-and-swap that a filesystem without `flock` cannot provide, and two daemons is the outcome 7.1 and R5 forbid. It classifies the recorded holder, pid plus the platform's own start time for it, so the message says whether that pid is a live holder, a recycled pid, or gone `[D 6 §8.8]` | The supervisor's stderr and the whirl log carry `state/locks/daemon.lock left by pid <p>, started <t>, which is not running; remove <path>`, and the daemon exits 1, which is the frontend contract's "the daemon refused". Every `whirl` verb exits 2 with row 1's message until the file is removed, and the supervisor keeps retrying on the cadence section 5 gives it. `status` says nothing about the condition and `lock_mode` gains no third value, because a daemon that refuses at 1.5 step 1 never binds the socket; a recycled pid is named as recycled rather than as a holder, so the operator is not told a live process is blocking them when the holder is gone |

## 8. Frontend contract

A frontend is an ordinary client of section 2. This is what it may rely on, and what it must never
do. It exists so that a TUI, a menu bar item or a script can be written without reading the daemon's
source, and so that the daemon can be changed without breaking them.

**May rely on:**

1. **The greeting, and version negotiation** (2.4). Read the greeting before writing; send `hello`
   if you care; refuse to run if `protocol` is not one you know.
2. **`status` as a complete snapshot.** Every key in 2.10 is stable: it may appear, it may gain new
   keys, it will not be renamed or removed without a protocol version bump. Unknown keys are
   ignored, never an error.
3. **`subscribe` for change notification**, with `seq` increasing by exactly 1 per event. Compare
   `seq` across events and across a reconnect: a gap means you missed events and must re-read
   `status`; a `seq` lower than the last one you saw means the daemon restarted and you must re-read
   `status`. Never persist `seq` `[D 6 §6.1]`.
4. **The heartbeat.** If nothing at all arrives for 90 s, the daemon is gone; reconnect and expect
   `status` to be your recovery path.
5. **The five error codes a frontend can act on:** `busy` (retry later), `not_found` (your id is
   stale, re-read `status`), `timeout` (a rotation is probably still running), `favorites_degraded`
   (hide the pin affordance), `bad_config` (point at `whirl config check`).
6. **One rotation at a time, and one command connection plus one subscription is the intended
   shape** (2.3 step 5); the daemon allows 16 connections and refuses the rest with `busy`.
7. **Exact strings for the CLI's own contract:** exit 0 success, 1 the daemon refused, 2 the daemon
   is unreachable, 3 usage error. A frontend that shells out to `whirl` can branch on those, and on
   the `ERR` code in the message.

**Must never:**

1. **Write any state file.** `current.json`, `history.json`, `favorites.json`, `cache/index.json`,
   the locks and the log are the daemon's. `[D 6 §7.1]` states the rule; a second writer is how a
   state file gets torn `[D 6 §6.3 L 5]`.
2. **Call a platform setter directly.** Not `gsettings`, not `swaymsg`, not `hyprctl`, not
   `osascript`, not COM. Two writers to the same wallpaper fight, and the daemon's anchor stops
   matching what is on screen.
3. **Start, stop or restart the daemon.** The OS supervisor owns its lifetime (5.1); a frontend that
   starts one gets a second daemon, or a stale socket, and `daemon.lock` means the second one exits
   anyway `[D 6 §7.2]`.
4. **Spawn its own worker or implement a source.** Sources are the worker's `[D 5 §2.0]`; a second
   implementation drifts.
5. **Unlink the socket.**
6. **Read the config file and treat it as the effective plan.** The effective values differ by
   definition: `display_mode_effective`, per-source `enabled` and `reason`, alias resolution,
   environment overrides. `status` and `sources` are the effective plan (4.3).
7. **Parse the log as an interface.** It is a log; it is 0600 and it carries no secret, and its
   format is not a contract `[D 6 §7.2]`.
8. **Assume any identifier is unique across sources.** A source's `id` is unique in the config, and
   `origin_key` is unique within a source; the unit that is stable across everything is the content
   `digest` `[D 6 §4.3]`.
9. **Poll.** `subscribe` exists so that nothing polls: `[D 5 §1.1]`'s `whirl idle` row is the rule
   ("Block until state changes. For frontends, so none of them polls.") and a 1-second poll loop from
   a frontend is the thing this protocol was designed to avoid.

## 9. Rules for the resident process

The rules the daemon is built to, each one with the measurement that justifies it. They are the
reason section 1 looks the way it does, and they are the tests a reviewer can hold an
implementation to.

**R1. It never owns pixels.** No decoding, no thumbnailing, no colour extraction, no image buffers
in the daemon. *Basis:* `[M 1]` and `[M 2]`, the same daemon one flag apart:
`1.8 -> 2.3 MB flat` delegated versus `12 -> 140 MB and never back` in-process. Test: the daemon's
RSS after 100 rotations is within a few hundred KB of its RSS before them.

**R2. It never links a GUI toolkit, and no window, tray icon or AppKit/COM/GTK object of any kind
lives in it.** *Basis:* `[M 8]`, the 883 MB single-process app whose every feature became a
permanent memory floor, and `[L 3]`, the daemon built here at 727,072 bytes with no dependencies.
Test: `whirld` does not depend on the worker's platform crate, and its binary stays under a
megabyte.

**R3. It never holds an unbounded index.** The history is a ring of `state.history_entries` (50)
entries `[D 6 §6.2]`, the cache index is bounded by `cache.max_files` (500), and the whole index is
about 341 bytes per entry, so 500 entries is 170,607 bytes `[D 6 §2.1 L 5]`. *Basis:* `[D 6 §2.1]`
and `[D 6 §5.1]`. Test: parse `cache/index.json` after a forced sweep at the cap; the file is the
specified size and entry count.

**R4. It grows nothing without a cap: not bytes, not files, not threads, not connections.**
*Basis:* `[D 6 §5.1]`, where the count cap does not bound bytes (the same 500 files span 60x in
size, and the measured tightest draw of a Wallhaven query is the reason both caps exist), and
`[L 4]`, where 128 unbounded connections moved the prototype's RSS 1.8 -> 5.1 MB and its fds 7 ->
263. Tests: 300 connections get `ERR busy` at 16 and RSS stops moving; the cache stays under both
caps after a sweep; `cache_over_cap: 1` with `cache_over_reason` appears when a pin, the anchor or
the grace window prevents the sweep from reaching the cap `[D 6 §5.3]`.

**R5. Exactly one process writes any given file, and every state write is atomic.** *Basis:*
`[D 6 §6.3 L 5]`, 3068 of 3508 concurrent reads of a state file being rewritten in place saw a
truncated file, against 0 with temp + rename. Tests: kill the daemon during a state write and read
the file back, and `daemon.lock` refuses the second instance `[D 6 §7.2]`.

A sixth, which is a consequence rather than a separate measurement and is stated because it is easy
to violate: **the control plane never waits on the worker** (1.8). `status`, `history`, `favorites`,
`sources`, `pause` and `resume` are answered from memory in every state, including while a rotation
runs and while a sweep runs. Test: during a forced 300-second hanging worker, `whirl status`
answers in one round trip.

## 10. Where this document diverges from, or narrowly reads, the specs

The card's rule: if I disagree with a spec, say so here and flag it in the handoff rather than
diverging silently. Each row quotes the sentence it disagrees with. **This section is re-based
against the current spec text.** Rows 10.1, 10.2 and 10.3 are marked `resolved by 0382364` rather
than deleted: the round-1 spec fixes ("docs: fix the four defects from the research and spec
review", `0382364`) rewrote the sentences those rows quoted, so the disagreements are gone, and a
resolved row quotes the sentence that settled it so a reader who saw the earlier draft can check
that the disagreement really closed. Every quote below was checked by substring search against
`docs/spec/features.md` and `docs/spec/state-and-cache.md` when this revision was written, and 10.8's
quote was corrected rather than re-framed: it was a mis-transcription, not a resolved row.

**10.1 macOS per-Space setting. Resolved by 0382364.** This row used to quote features.md §1.3 as
saying "The spike reports per-Space setting on macOS via `NSWorkspace.setDesktopImageURL` with the
`allSpaces` option", and asked for that sentence to be struck from the spec. The fix did more than
strike it: the paragraph states what `docs/research/macos.md` §2 and §3 measured, in the research's
own terms, including "with and without the undocumented `allSpaces` option the same two `Index.plist`
nodes are written, the write lands on the Space that is frontmost at that moment, and no parameter in
the public API names a Space". It also fixes the option's status rather than softening it:
"`allSpaces` is undocumented" and inert, and must not be used. The architecture follows the research
in 3.2 and 3.6, and there is nothing left to disagree with.

**10.2 "one call on macOS". Resolved by 0382364.** This row used to quote features.md §1.3 as saying
`"all"` "is the default because it needs one call on macOS and Windows". The replacement sentence
drops both the call count and the platform claim: `"all"` "is the default because it is reachable
from a short-lived worker on every platform the spike touched", and the paragraph below it gives the
per-platform reach in three bullets (macOS: the frontmost Space only, `allSpaces` inert; Windows:
`SystemParametersInfoW` for one image everywhere and no per-virtual-desktop wallpaper API; Linux: per
environment). That is the reading 3.1, 3.3 and 3.4 implement, and it now names the API the
architecture uses: the old misattribution is gone, because the same section names
`IDesktopWallpaper` for per-monitor, as `[D 2 §1, §3]` does.

**10.3 The research is no longer "in flight". Resolved by 0382364.** This row used to quote
features.md §1.3's assumption block as saying the four research cards "are in flight and have
produced no output yet". The block now says the question "is now answered by the four research notes,
which landed after the first draft of this spec", gives the per-platform answer, and keeps the old
board query explicitly as history rather than as a current claim. Consequence, which 3.1, 3.7 and
7.14 apply: the spec's "on a platform where the research has not answered yet" fallback has answers
per platform now, so it is the safety net rather than the expected path.

**10.4 "fully commented when generated" (F1) in JSON.** JSON has no comment syntax. Resolved with
the `_` key convention (4.1), not with a second file and not with a non-standard dialect: the file
stays parseable by every JSON parser while carrying its annotations.

**10.5 F1's `SIGHUP` / `whirl reload`.** Not implemented. The daemon re-reads the config before
every rotation `[D 5 §2.0]`, the verb set is closed `[D 5 §1.1]`, and `SIGHUP` does not exist on
Windows, so a signal path would be a per-platform behaviour difference for no gain. The user-visible
rule instead: a config edit takes effect at the next rotation, uniformly, and `whirl next` is the
lever if it must be sooner.

**10.6 The config validation split.** `docs/spec/state-and-cache.md` §8.7 says: *"`config.json`
parses but fails `whirl config check`: a value out of range, or an ordering rule ... The daemon does
not start either, and the message names the failing key and both values."* `docs/spec/features.md`
§2.0 says: *"The daemon parses only the top-level scalars it needs"*, and its validation policy says
*"an unknown field is a warning; an unknown `kind` or a missing required field is an error naming the
known kinds and the offending source `id`"*. Those cannot all hold unless the daemon knows the source
kinds and their required fields. 4.3 splits it: structural source facts are the daemon's and refuse
startup, semantic and environmental ones are the worker's and disable one source. This narrows 2.0's
"only the scalars it needs" to mean the daemon does not interpret sources for selection, while still
validating their shape, because 8.7's requirement that `config check` and startup agree is the one
that keeps a config from being legal on demand and illegal at boot.

**10.7 The prototype README's macOS claims.** `prototype/README.md`'s "Corrected by later research"
section already retracts the `allSpaces` and AppleScript claims, and this document cites the research
rather than the spike. One thing the README does **not** flag: the in-process row `[M 2]` and its
trace `[M 9]` cannot be reproduced from this tree, because the artifact that produced them (`-inproc`,
an HTTP control surface, `/status` fields `inproc` and `goroutines`) is not committed while
`prototype/whd/measure.py` still expects it `[L 5]`. I use the number as the card requires, and 1.4
states the caveat and what would settle it.

**10.8 The double wake.** `prototype/README.md` calls the duplicate `idle` notification *"Cosmetic,
unfixed here."* (that line is still there). This row used to quote
`docs/spec/state-and-cache.md` §7.3 step 5 as requiring *"one notification, and only once"*; that
sentence was never in the document. What step 5 does say is *"The daemon notifies `idle` subscribers
once, and only once."*, and then, about the prototype's wart, *"with R2 in force there is one state
transition, so there is one notification"*. So the spec already agrees with this document, and the
only disagreement left is with the prototype: 2.9 defines one event per state transition, and the
`rotating` key covers the intermediate state for a client that wants it. Corrected here, not in the
spec.

**10.9 `whirl idle` has no protocol verb of its own.** `features.md` §1.1's verb table, the
`whirl idle` row, requires *"Block until state changes. For frontends, so none of them polls."* (The
requirement is not in F10: that row is `status` and `sources` introspection, cited in 2.10 for the
stability of the key set.) Met by `subscribe` plus a client-side first-event exit (2.5.1), which also
drops the prototype's 60 s timeout line and `whctl watch`'s 200-iteration cap with it. The
requirement stands; the mechanism is one verb shorter.

**10.10 A charset for source ids.** features.md §2.1 says only "Stable name for status, history and
error messages." 2.2 adds
`[A-Za-z0-9._:-]+` and a 64-byte bound, because an `id` is a non-final field in the `source:` and
`entry:` records and a space in it would make the framing ambiguous.

**10.11 The startup readback gains a second trigger.** features.md §1.5 reads the current desktop
image at startup to detect a manual change. 1.7.3 uses the same readback after a worker dies between
its setter call and its exit, because otherwise the file that is on screen is the one file the sweep
would reclaim as an orphan, and `docs/spec/state-and-cache.md` §5.3 protects the anchor precisely
because a wallpaper whose file disappears is a broken wallpaper. Extension of the rule, not a
contradiction of it.

**10.12 The clock.** `docs/spec/state-and-cache.md` §8.6's rule is adopted: wall clock for the
deadline, monotonic for durations, bounded slices rather than a composed sleep. Added in 5.5: the
persisted `next_at` advances by whole intervals instead of being re-anchored from `now`, which is
what keeps the schedule on the user's grid and makes "one rotation on wake, never a burst"
structural rather than accidental. This is the fix for the prototype's re-anchor `[M 14]` and its
never-advance-on-failure path `[M 17]`.

**10.13 No other unresolved conflicts.** I checked the two spots where `docs/spec/state-and-cache.md`
§9 records a reconciliation with `features.md` (the dedupe window decoupled from the history ring,
and the config aliases): both are consistent as written and this document adopts them without
comment. One narrow reading is named here rather than hidden, because it is the kind of thing an
implementer would otherwise discover by surprise: §6.2 lists `prev` among the `via` values a history
entry can carry, and 2.5 has a `prev` set write no history entry at all, so that value appears in the
`set:` response and in `last_via` (2.10) and never in `history`. The spec is not contradicted, a
value it permits is simply not produced, and 2.5 gives the reason (a walk that appends to the ring it
walks cannot be predicted twice). This is the same shape as 10.6's narrowing.

**10.14 The `origin_key` prefix.** `docs/spec/state-and-cache.md` §4.1's JSON example writes
`"origin_key": "wallhaven:ab12cd"`, prefixing the source *kind*; 2.5 uses the source `id`
(`space:ab12cd`). Basis for the divergence: §4.1's prose says "`origin_key` is the source-scoped
candidate id", and with the kind as prefix two `wallhaven` sources that both return the same
wallpaper would produce the same key, which would silently merge two candidates the rest of that
section says land twice. Low stakes, but it is a difference, and it is the kind of thing a client
implementation would copy from the example rather than from the prose.

## 11. Evidence

`[M n]` is a measurement or a read constant from the prototype: a line of `prototype/README.md`, or
a source line, given with its location so it can be checked without trusting me. Recorded on this
machine (macOS 26.5.2, build 25F84, arm64; rustc 1.94.0; Python 3.14.7).

| | Value | Where |
|---|---|---|
| `[M 1]` | delegated daemon: 1.8 MB at start, 2.3 MB after 7 rotations, flat | `prototype/README.md:21` |
| `[M 2]` | in-process daemon: 12 MB -> 140 MB and never returns; 141 MB transient | `prototype/README.md:22` |
| `[M 3]` | "A daemon can hold config, schedule, history and favorites while staying under 3 MB, if it delegates the image work." | `prototype/README.md:62` |
| `[M 4]` | worker: 0 resident between rotations, 21-25 MB peak for 1.2-3.4 s per rotation | `prototype/README.md:20` |
| `[M 5]` | the worker's HTTP client timeout is 2 minutes | `prototype/wh-rotate/main.go:267` |
| `[M 6]` | the daemon chmods its socket to `0600` after binding it | `prototype/whd/whd.rs:466`; observed in `[L 3]` |
| `[M 7]` | the daemon has no JSON parser: `sources` reads the worker's config file and splits it on the literal `"kind"` | `prototype/whd/whd.rs:407-414` |
| `[M 8]` | Spice (Go + Fyne): 883 MB and climbing, with a 354 MB uncached cache | `prototype/README.md:19` |
| `[M 9]` | the ratchet, in twelve requests: idle 12.0; rot1 1920x1080 (7.9 MB RGBA) 24.7; rot2 2518x3723 (35.8) 46.1; rot3 1920x1080 46.2 "small image, stays"; rot6 4672x7008 (124.9) 140.1 "floor moves again"; rot7-12 ~10 MB images 140.3 "permanently". `runtime.GC()` and `FreeOSMemory()` ran after every decode | `prototype/README.md:26-35` |
| `[M 10]` | defaults: interval 1800 s; state dir `~/.local/state/wh`; socket `<state>/sock`; cache dir a scratch path, not a platform cache root | `prototype/whd/whd.rs:434-452` |
| `[M 11]` | the prototype's history ring is 64 entries | `prototype/whd/whd.rs:161` |
| `[M 12]` | "The daemon wakes twice per rotation (worker start and finish), so an `idle` subscriber sees duplicate notifications. Cosmetic, unfixed here." | `prototype/README.md:86-87`; the two `bump` + `notify_all` pairs are `whd.rs:146-147` and `whd.rs:171-173` |
| `[M 13]` | `whctl`: exit codes 0 ok / 1 command failed / 2 unreachable; a 300 s read timeout; `ACK ` goes to stderr and exits 1; every other line is printed raw; `watch` stops after 200 idles; the greeting must start with `OK ` | `prototype/whd/whctl.rs:2, 32, 68-72, 74, 42` |
| `[M 14]` | no worker deadline: the daemon blocks in `c.output()`; the client gives up at 240 s while the worker keeps running; on success the deadline is re-anchored as `next_at = now + interval` | `prototype/whd/whd.rs:120, 220, 165` |
| `[M 15]` | the socket is unlinked unconditionally at startup | `prototype/whd/whd.rs:456` |
| `[M 16]` | the worker is spawned with `env()`, so it inherits the daemon's whole environment | `prototype/whd/whd.rs:113-114` |
| `[M 17]` | a failed rotation never advances `next_at`, so the scheduler re-queues it every 1-20 s forever | `prototype/whd/whd.rs:165` (the `Ok` arm only) with the loop at `whd.rs:180-203` |
| `[M 18]` | the prototype's scheduler already sleeps in bounded slices, `next_at - now` clamped to 1-20 s, and fires when `now >= next_at`; its own comment says the slice exists so a machine that slept rotates once on wake | `prototype/whd/whd.rs:180-203` |

`[L n]` is something run on this machine while writing this document. Nothing here changed the
wallpaper, the machine's real configuration, or any file outside the task's worktree and a scratch
directory.

| | What was run | Result |
|---|---|---|
| `[L 1]` | `xcrun --show-sdk-path` then read `sys/un.h` | `sun_path` is 104 bytes |
| `[L 2]` | read `sys/syslimits.h` | `PATH_MAX` is 1024 |
| `[L 3]` | built the prototype with `rustc -O` and drove it on a scratch socket and scratch state/cache directories: greeting, `status`, an unknown verb, empty `history`, empty `favorites`, an abandoned half-line, socket mode before and after startup, binary sizes, startup line | greeting is `OK whd 0.1.0 protocol 1`; `status` keys are `pid, version, rss_mb, open_fds, uptime_s, rotations, paused, rotating, interval_s, next_in_s, last, last_at, history, favorites, cache_files, gen`; an unknown verb answers `ACK unknown command 'frobnicate'` and the connection keeps working; empty `history` and empty `favorites` both answer a bare `OK`; an unset scalar prints `last: ` with a trailing space; the socket is `0600` after startup and `0755` when bound without a `chmod` at umask `0022`; RSS at start 1.7 MB; `whd` is 727,072 bytes and `whctl` 469,496 bytes; the startup line prints the version twice: `whd whd 0.1.0 listening on ...` |
| `[L 4]` | held N idle connections open to the prototype and read the daemon's own `status` after each step | 0 conns: 1.8 MB / 7 fds; 1: 1.8 / 9; 8: 2.0 / 23; 32: 2.7 / 71; 64: 3.5 / 135; 128: 5.1 / 263. An abandoned half-line left the daemon answering |
| `[L 5]` | read `prototype/whd/measure.py` and grepped the prototype for a decode path | `measure.py:12,43,52` drives `http://127.0.0.1:8791` and reads `inproc` and `goroutines`; `whd.rs` has neither a decode path nor those fields, so `[M 2]`/`[M 9]` are not reproducible from this tree |
| `[L 6]` | parsed the annotated config example in 4.2 with `json.loads` | parses; every `_`-prefixed key is a comment and the six ordering rules and every default are as printed |
| `[L 7]` | resolved the configured socket and config paths and measured their lengths | macOS socket 69 bytes, Linux `$XDG_RUNTIME_DIR` socket 24, Linux state fallback 54, macOS config path 70 |
| `[L 8]` | `command -v whirl whirld whirl-worker` | none of the three names exists in `PATH` on this machine |
| `[L 9]` | the mechanical check described in 2.11: every verb defined in 2.5 and 2.9 appears in a transcript, and every verb in a transcript is defined | all 19 verbs defined in 2.5 and 2.9 appear in transcripts A, B or C; the only verb-shaped tokens in those transcripts that are not defined are the two deliberate error cases in C (frobnicate, status\0); nothing defined is left unexercised |
| `[L 10]` | scanned the finished document for em dashes and typographic quotes, which this repo's docs do not use | em dash: 0; en dash: 0; typographic double quote: 0; typographic single quote: 0 (none present) |
| `[L 11]` | the checks this revision (`fix the ten review defects`) ran with `python3` against the edited files, in the task's scratch directory: every `sha256/xx/yy/<digest>.<ext>` path in this document tested against its own digest, every sentence section 10 quotes tested as a whitespace-normalized substring of the document it is attributed to, the four sentences section 10 says were removed or mis-transcribed tested for absence, the ten defects each re-tested as a positive assertion on the new text, and the dash/typographic scan of `[L 10]` re-run on this file and on `docs/development.md` | 8 of 8 cache paths recompute (`xx` is the digest's first two characters, `yy` the next two); 14 of 14 quoted sentences exist where they are attributed and the 4 removed or mis-transcribed ones are gone; 61 of 61 defect assertions hold, 0 failures; em dash 0, en dash 0, typographic quotes 0 in both files |

## 12. Checklist for the reviewer

Not a verdict, a map. Each of the card's nine required items, where it is, and the one thing in it
that is easiest to get wrong.

| Required | Where | Attack this first |
|---|---|---|
| 1 Process model | 1.1-1.9 | that the worker contract in 1.6 is implementable as written, and that 1.7.3 closes the orphan-versus-displayed-file hole instead of opening one |
| 2 Protocol, implementable alone | 2.1-2.12 | write a client from 2.1-2.9 only, then check the transcripts against the grammar; the `ERR` code set and the two-field `entry:` records are the most likely places to find an inconsistency |
| 3 Cross-platform verdict | 3.1-3.7 | whether any row hedges without naming what it depends on, and whether the resident cost is named for every "yes" |
| 4 Config file | 4.1-4.3 | that the example's keys and defaults match the prose, and that no key is invented without a spec citation |
| 5 Scheduling decision | 5.1-5.5 | that the whole-interval advance in 5.5 is equivalent to the behaviour `[D 4 §Part 2]` describes waking from |
| 6 Security model | 6.1-6.4 | that the reachability claims are true of the socket mode and the pipe DACL, and that no rule requires a secret anywhere but the three named stores |
| 7 Failure modes | 7 | that each row is complete: what the daemon does, and what the user sees |
| 8 Frontend contract | 8 | that nothing in "may rely on" is contradicted by "must never" |
| 9 Rules for the resident process | 9 | that each rule names a measurement, not a preference |
| Acceptance: every decision tied to evidence | the table in "Decisions at a glance" | any row whose basis column points at something that does not support it |
| Acceptance: transcript consistent with the grammar | 2.11, `[L 9]` | the transcripts are generated from the same constants as the status block, so a mismatch means I wrote the grammar wrong, not the transcript |
| Acceptance: no TBD | whole document | grep for the three usual placeholder markers; the only hit should be this row |
| Acceptance: spec disagreements stated | 10 | that each quoted sentence is quoted accurately and that the resolution does not quietly move a feature |