# 0002. Let a frontend write the config and own no part of the daemon

- **Status:** proposed
- **Date:** 2026-10-02
- **Deciders:** Guru (project owner, `guruor`)
- **Supersedes:** nothing

## Context

A reference frontend is being planned in its own repository: a tray item plus a
settings window, macOS first. It lets a user change the settings and, where a
source is `wallhaven`, enter an API token. Four questions the documents leave
open are decided here, and the last section leaves a fifth open on purpose.
Nothing in this document is implemented by writing it.

The constraints every decision below has to fit:

- **Section 8 is the frontend contract.** Its "must never" list already forbids
  writing a state file (item 1), calling a platform setter directly (item 2),
  starting, stopping or restarting the daemon (item 3), spawning a worker or
  implementing a source (item 4), unlinking the socket (item 5), reading the
  config file and treating it as the effective plan (item 6), parsing the log as
  an interface (item 7), and polling (item 9).
- **Section 2 is the whole protocol, and it has nineteen verbs**
  (`crates/whirl-core/src/protocol.rs:291-311`). None of them reads or writes a
  config value; the two that name the config are `config path` and
  `config check` (section 2.5).
- **`docs/spec/features.md` 1.1 already decided that the config file is the
  write surface** for the settings that look like verbs: "`schedule set`,
  `interval set`: writing the config file is the interface" (`features.md:68`),
  and it refused `daemon start|stop|restart` because "lifecycle belongs to
  launchd, Task Scheduler or a systemd user unit" (`features.md:69-71`).
- **`docs/spec/features.md` 2.0 splits the config between two processes.** The
  daemon parses only the top-level scalars it needs; the `sources` array is
  interpreted "only by the worker" (`features.md:278-288`).
- **Section 6.3 owns the key rule**, and features.md 2.4 is the resolution order
  the code implements (`docs/architecture.md:1700-1723`, `features.md:390-441`).
- **Section 5.1 owns the process**: "The OS scheduler owns the process. The
  daemon owns the clock" (`docs/architecture.md:1538-1539`), and 5.2 names the
  macOS unit, `~/Library/LaunchAgents/com.guruor.whirl.plist`
  (`docs/architecture.md:1560`).
- **`docs/quickstart.md:97-99`** says the macOS agent is not implemented.

### What exists, measured

Every measurement below was run on 2026-10-02 at `development`'s tip `6455a9c`,
with `target/debug` built from that commit. The probes are shell scripts under a
scratch directory outside the repository, so a command below that starts or stops
the daemon stands for the few lines that do it. The scratch directory is written
as `<scratch>` and an ellipsis marks lines elided from a long output; every other
character is as printed, including the refusals. No credential, token or key
appears in any command or output below.

**1. The daemon reads the config file once, at startup, and never again. The
worker re-reads it at every spawn.** The probe starts the daemon on
`interval_seconds = 1800`, subscribes, edits the file to `900` while the daemon
runs, then reads `status` before and after a restart:

```
$ whirl status | grep -E "^(interval_s|sources|source):"
interval_s: 1800
sources: 1
source: pictures local weight=1 enabled=1 last=- reason=-
$ whirl idle &                      # one subscribe connection
$ cp config-900.json config.json    # the same file the daemon read
config.json now says:   "schedule": { "interval_seconds": 900 },
$ kill %1
bytes on the subscribe stream during and after the edit:       14
--- the stream, verbatim ---
subscribed: 1
$ whirl status | grep -E "^interval_s:"
interval_s: 1800
$ # restart the daemon on the same file
$ whirl status | grep -E "^(interval_s|uptime_s):"
uptime_s: 0
interval_s: 900
```

The sources half of the same fact, with the daemon still running and the file
edited from `weight=1` to `weight=3`:

```
$ whirl sources
count: 1
source: pictures local weight=1 enabled=1 last=- reason=-
$ grep -n '"weight"' config-weight.json
8:    { "id": "pictures", "kind": "local", "weight": 3, "paths": ["<scratch>/walls"] }
$ whirl status | grep -E "^source:"
source: pictures local weight=1 enabled=1 last=- reason=-
$ whirl config check | grep -E "^source:"
source: pictures local weight=3 enabled=1 last=- candidates=2 admitted=2 rejected_resolution=0 rejected_ratio=0 rejected_size=0 rejected_type=0 rejected_dedupe=0 reason=-
```

So the daemon's own scalars and the `source:` records it prints are frozen at
startup, while the worker reads whatever the file says at each spawn. The code
says the same: `load_config` is called once, from `Effective::resolve`
(`crates/whirld/src/plan.rs:71`, reached from `crates/whirld/src/main.rs:99`),
and the worker loads the file itself (`crates/whirl-worker/src/main.rs:116`)
because the daemon passes it the path (`crates/whirld/src/worker.rs:214-215`).

**2. `Event::ConfigReloaded` is declared and never emitted.** Every occurrence in
the tree:

```
$ grep -rn "ConfigReloaded" crates/
crates/whirld/src/events.rs:73:    ConfigReloaded,
crates/whirld/src/events.rs:99:            Event::ConfigReloaded => "config_reloaded",
crates/whirld/src/events.rs:131:            | Event::ConfigReloaded
crates/whirld/src/events.rs:276:            (Event::ConfigReloaded, "event: 188 config_reloaded\n"),
```

Line 72 carries `#[allow(dead_code)]`, and the other three are the event's own
name, its (empty) field list and its encoding test. `docs/architecture.md:1529-1532`
promises the re-read and the event; the running daemon above produced neither.

**3. With the daemon down, every verb fails, including the one that names the
config file.**

```
$ whirl config path
whirl: cannot reach the daemon at <scratch>/whirl.sock: No such file or directory (os error 2)
exit=2
$ whirl status
whirl: cannot reach the daemon at <scratch>/whirl.sock: No such file or directory (os error 2)
exit=2
$ whirl ping
whirl: cannot reach the daemon at <scratch>/whirl.sock: No such file or directory (os error 2)
exit=2
```

**4. On a first run the daemon writes the annotated default itself**, at the
resolved path, mode `0600`:

```
$ # no config file exists
$ whirld
whirld: wrote the default config at <scratch>/Application Support/whirl/config.json
whirld: config <scratch>/Application Support/whirl/config.json
whirld: listening on <scratch>/whirl.sock
$ ls -l <scratch>/Application Support/whirl/config.json
-rw-------  1 <user>  staff  9969 <date> <scratch>/Application Support/whirl/config.json
$ whirl config path
config: <scratch>/Application Support/whirl/config.json
```

**5. A config that carries a key-shaped `api_key_ref` is refused, with the field
and the line named, and the daemon exits 1.**

```
$ whirld          # WHIRL_CONFIG points at a config whose api_key_ref is a 40-character synthetic pattern
whirld exit=1
whirld: sources[0].api_key_ref (line 8): <scratch>/config-key-shaped.json: api_key_ref holds a NAME, never a value; a key in the config file is a key in every backup of it. Use WHIRL_WALLHAVEN_API_KEY or the platform's own store
```

That is `crates/whirl-core/src/config.rs:1260-1289`; the label form
`keychain:whirl-wallhaven` is accepted.

**6. A missing key disables the source in the worker's plan, and nothing else.
The daemon's own `status` does not know.**

```
$ whirl sources
source: space wallhaven weight=1 enabled=1 last=- reason=-
$ whirl config check
source: space wallhaven weight=1 enabled=0 last=- reason=sources[id=space].purity (compiled default): purity=111 requires an API key, none resolvable (checked env WHIRL_WALLHAVEN_API_KEY, keychain label 'whirl-wallhaven')
source: pictures local weight=1 enabled=1 last=- candidates=2 admitted=2 rejected_resolution=0 rejected_ratio=0 rejected_size=0 rejected_type=0 rejected_dedupe=0 reason=-
config check exit=0
```

The rotation went on with the remaining source, `config check` exited 0, and
`grep -c WHIRL_WALLHAVEN_API_KEY` over the daemon's log returned `0`. The
reason's shape is `crates/whirl-worker/src/sources/wallhaven.rs:392-405`, and
features.md 2.4's fallback (`features.md:426-438`) is what it implements.
`docs/architecture.md:1513-1525` says the disabled source "appears as
`source: <id> ... enabled=0 reason=<...>` in `status`, `sources` and
`config check`"; the measurement above shows it appears in `config check` only.

**7. There is no config-value verb anywhere in the protocol.**

```
$ grep -rn "ConfigGet\|ConfigSet\|config get\|config set" crates/*/src/protocol.rs crates/*/src/socket.rs
(no match)
$ grep -n "pub const VERBS" -A 24 crates/whirl-core/src/protocol.rs
291:pub const VERBS: [&str; 19] = [
292-    "hello",
293-    "ping",
294-    "version",
295-    "status",
296-    "next",
297-    "prev",
298-    "set path",
299-    "set id",
300-    "pause",
301-    "resume",
302-    "history",
303-    "favorites",
304-    "favorite",
305-    "unfavorite",
306-    "sources",
307-    "config path",
308-    "config check",
309-    "subscribe",
310-    "close",
311-];
```

**8. No supervisor unit ships in the repository, and none is installed on this
machine.**

```
$ git ls-files | grep -E "\.plist$|\.service$|\.timer$|LaunchAgents|schtasks"
docs/research/probes/com.whirl.research.probeA.plist
docs/research/probes/com.whirl.research.probeB.plist
docs/research/probes/com.whirl.research.probeC.plist
docs/research/probes/com.whirl.research.probeE.plist
docs/research/probes/com.whirl.research.probeF.plist
docs/research/probes/com.whirl.research.probeG.plist
docs/research/probes/com.whirl.research.probeG2.plist
docs/research/probes/com.whirl.research.probeH.plist
docs/research/probes/com.whirl.research.probeR.plist
docs/research/probes/evidence/microsoft-schtasks-install-uninstall.txt
$ ls -la ~/Library/LaunchAgents/com.guruor.whirl.plist
No such file or directory
```

**9. The platform store is implemented on macOS and on no other platform.** The
resolution order is the environment variable, then the store
(`crates/whirl-worker/src/sources/wallhaven.rs:117-126`); the store lookup
returns `None` everywhere but macOS, and on macOS it runs
`security find-generic-password -s <label> -w` (`whirl-worker`, same file,
lines 162-187, with `if !cfg!(target_os = "macos") { return None; }` at 172-174).
The label defaults to `whirl-wallhaven` and an `api_key_ref` prefix such as
`keychain:` is dropped (lines 83-85, 137-149).

The same section carries a divergence, reported rather than hidden
(`whirl-worker/src/sources/wallhaven.rs:105-113`): features.md 2.4 is taken as
authoritative over `docs/architecture.md` 6.3, which adds a third source ("a
`0600` file the config points at", `architecture.md:1707`) that features 2.4's
resolution list does not have. The implementation has the two arms of 2.4 and no
third.

**10. On macOS the store can be written without the value reaching `argv`, and
read without the value being printed.**

```
$ security help find-generic-password
Usage: find-generic-password [-h] [-a account] [-s service] [options...] [-g] [keychain...]
    -g  Display the password for the item found
    -w  Display only the password on stdout
$ security help add-generic-password
Usage: add-generic-password [-h] [-a account] [-s service] [-w password] [options...] [-A|-T appPath] [keychain]
...
Use of the -p or -w options is insecure. Specify -w as the last option to be prompted.
```

`find-generic-password` prints no password unless `-g` or `-w` is passed, and
`-w` puts the value in `argv`, which section 6.3 forbids ("It is never in argv",
`architecture.md:1712`).

## Decision

### 1. A frontend writes the config file itself; the daemon grows no verb for it

A frontend changes the config by writing the config file, under the contract
below, and the daemon gains no `config get` and no `config set`. The daemon's
half of the contract is the promise 4.3 already makes: it re-reads the config on
every rotation and emits `config_reloaded` on a successful re-read, keeping the
previous config and logging on a failed one (`docs/architecture.md:1529-1532`).

The writer's contract:

- **Read the file you are about to write.** Change only the keys the UI owns.
  Preserve every other key, every `_comment_*` key (4.1), and the aliases the
  file may already carry (`keep` for `cache.max_files`, `cache_dir` for
  `cache.root`, `crates/whirl-core/src/config.rs:2158-2159`). The daemon never
  rewrites this file, so nothing else will restore what the writer drops.
- **Write atomically**: temp file in the same directory, `fsync`, `rename`, mode
  `0600`, which is what 6.3 of `docs/spec/state-and-cache.md` already requires of
  every write whirl makes and what the daemon itself leaves this file as
  (measurement 4).
- **Do not read it back as the effective plan** (8, "must never" 6). After the
  write, the frontend reads `status` and `sources` to learn what the daemon
  made of it.
- **Bound every value to the rules of 4.3** (`schedule.interval_seconds >= 60`
  and the rest, `docs/architecture.md:1508-1512`), because there is no offline
  validator: the daemon's parser is reachable only by starting the daemon, and
  `config check` needs a running one (measurement 3).
- **Find the path in this order**: if the daemon answers, `config path`
  (measurement 3 and 4); if it does not, the platform's default path (4.1,
  `crates/whirl-core/src/config.rs:1609-1611`), and say that the write is
  unconfirmed because no daemon was there to confirm it. A frontend cannot see
  `WHIRL_CONFIG` or `--config`, so a daemon started with a moved config is the
  one case the file write cannot reach.

The case that decides it: **the daemon is not running, which is the normal state
when a first-time user opens the settings window.** Every verb fails then
(measurement 3), so verbs would give the UI nothing to call in exactly the state
it starts in, and the UI would need the file-write path anyway. One write path
that works in both states beats two, one of which is unvalidated by design.

### 2. The frontend writes the token to the platform store and never reads it back

**Who writes it:** the frontend writes the token itself, straight into the
platform's own store. Named exactly, per 6.3 (`architecture.md:1704-1707`) and
features.md 2.4:

| Platform | The store, and how the token is written | How the UI checks it is there |
|---|---|---|
| macOS | Keychain generic password, service `whirl-wallhaven`, written through the Security framework (`SecItemAdd` / `SecKeychainAddGenericPassword`). **Not** `security add-generic-password -w <value>`, which puts the token in `argv` and which 6.3 forbids (measurement 10). | `security find-generic-password -s whirl-wallhaven` with neither `-g` nor `-w`, which prints attributes and not the value (measurement 10) |
| Linux | Secret Service, `service whirl`, `key wallhaven`, written with `secret-tool store --label whirl-wallhaven service whirl key wallhaven`, the value on stdin | `secret-tool search`, which prints attributes; reading the value back is the one thing the UI never does |
| Windows | Credential Manager generic credential `whirl/wallhaven`, written through `CredWrite`; the UI reads back only the attributes it needs to render the row | the credential's presence, never its blob |

**What the config gains when it works:** nothing, when the token is written to
the platform's documented default label, because `api_key_ref: null` already
means that label (`crates/whirl-worker/src/sources/wallhaven.rs:83-85`). The
config gains exactly one key, `"api_key_ref": "keychain:<label>"`, when the user
writes the token under a label of their own. The config never gains the value: a
key-shaped `api_key_ref` is the `bad_config` refusal of measurement 5, and it is
the daemon, not the UI, that performs that refusal.

**How the UI proves it never reads the token back:** the value is never in the
UI's process, never in `argv`, never in the UI's own files, never in a log line,
and the UI never runs a read that prints it (measurement 10). What the UI shows
is what the daemon says about the source, not what the UI read from the store.

**What the UI shows when no key is set:** the source disabled with the daemon's
own reason, "a missing key disables the source, it does not fail the daemon"
(`architecture.md:1721-1723`): the source row renders `enabled=0 reason=<the
where-looked string>` (measurement 6), and rotations continue on the remaining
sources. The UI does not disable anything of its own, does not downgrade the
purity, and does not fail the window. Two costs are stated rather than hidden:
the measurement shows the reason reaches a client through `config check` today
and not through `status` (measurement 6, against `architecture.md:1513-1525`);
and on Linux and Windows the store arm is not implemented, so a frontend there
can only use `WHIRL_WALLHAVEN_API_KEY` until the worker's store arm lands
(measurement 9).

### 3. The supervisor unit belongs to whirl's installation, and the frontend only reports it

whirl's own installation installs the supervisor unit: on macOS
`~/Library/LaunchAgents/com.guruor.whirl.plist`, the file 5.2 already specifies
(`architecture.md:1560-1566`); the `systemd --user` unit of 5.4 on Linux; the
per-user task of 5.3 on Windows. The daemon does not install it (the daemon is
what the unit starts) and a frontend does not install it.

A frontend's one start-at-login control covers two facts, and separates them:

- **Its own login item is the frontend's own business**, opened through its own
  platform's mechanism. Nothing in section 8 speaks to it.
- **The daemon's supervisor unit is the product's**, and the frontend reports it
  and does not own it. The daemon half of the switch is disabled unless the unit
  is installed, and the window says which half is on.

The frontend never runs `launchctl`, `systemctl`, `schtasks` or the equivalent,
never writes the unit file, and never starts, stops or restarts the daemon (8,
"must never" 3; `architecture.md:1798-1800`).

**Consequence, plainly:** the unit is a prerequisite of the daemon half of the
switch, not of the UI. The UI is fully usable against a hand-started daemon
today (`docs/quickstart.md:58-64`), and its autostart switch for the daemon
ships disabled with the reason, until whirl's install path ships the unit. The
UI never shows a switch that lies.

### 4. Frontends live in their own repositories, and section 8 is the only contract

The reference frontend is a separate repository. Anyone may write another one.
whirl's contract to all of them is section 8 and nothing else, and each
repository points at the other.

Whirl's documentation gains, in one line each:

- a sentence in section 8 that frontends are separate repositories, that the
  reference one exists at its own URL, and that any language that can open a
  socket and read a line may write one (3.5, `architecture.md:1284-1285`);
- the URL of the reference frontend's repository, in section 8 and in
  `docs/README.md`.

Whirl deliberately does not gain: a frontend crate or binary in this workspace, a
GUI toolkit in any dependency graph (3.5 and `[R2]`, `architecture.md:1276-1278`),
a plugin or ABI surface, or CI for a repository it does not contain. The protocol
version is the API boundary, so a breaking frontend change is a `protocol` bump
(2.4) and the frontend refuses to run on a protocol it does not know, which is
already section 8 "may rely on" 1. What would reverse this: a frontend feature
that the socket cannot carry at all. That is a protocol decision with its own
ADR, not a reason to add a crate.

## Alternatives considered

- **`config get` and `config set` verbs on the daemon, so validation, alias
  resolution and the atomic write live in one place (decision 1).** Lost on the
  state that decides it: with the daemon down, the verbs do not
  exist, and every verb fails (measurement 3), so the UI needs the file-write
  path for its first run regardless; adding the verbs then buys a second write
  path with its own validation rather than one. It also breaks the split of
  features.md 2.0 in two directions: a `config set` on a source field would make
  the daemon parse and rewrite the `sources` array that only the worker
  interprets (`features.md:278-288`), and a `config get` would hand a client a
  value that is not the effective plan, which is the reading 8, "must never" 6
  forbids. features.md 1.1 already rejected the same shape for `schedule set`
  (`features.md:68`).
- **The daemon writes the file when it is up, the UI writes it when the daemon
  is down (decision 1).** Lost on cost with no gain: two writers of one file
  under two contracts, and the one that runs in the untested state is the one
  with no validation behind it.
- **The daemon re-reading the config only at each rotation, with the settings
  change invisible until then (decision 1, as accepted).** Not rejected, priced:
  the write takes effect at the next rotation, which is up to
  `schedule.interval_seconds` away, and the UI can say "next rotation in
  `next_in_s`" with the key 2.10 already defines. Lowering that latency is left
  open below rather than decided here.
- **The UI installing the unit and calling the supervisor (decision 3).** Lost:
  `launchctl bootstrap` and `systemctl --user enable --now` start the daemon, so
  a frontend running them is a frontend starting the daemon, which is 8, "must
  never" 3, and the lifetime 5.1 gives the supervisor is then owned by the UI.
- **The UI shipping its own login item that launches the daemon (decision 3).**
  Lost: same rule, and it is worse, because the login item is invisible in the
  product's own output and survives the UI being uninstalled.
- **The daemon installing its own unit at first start (decision 3).** Lost: the
  daemon is what the unit starts, so the first start is the one the unit never
  made, and installing a supervisor unit is an installation action rather than a
  runtime one.
- **A `whirl autostart enable` verb, so the UI could ask for the unit (decision
  3).** Not rejected on its merits, deferred: no such verb exists (the nineteen
  verbs of measurement 7), a new CLI verb needs its own ADR
  under `docs/development.md` section 6, and if a frontend is to reach it the
  rule in 2.5.1 makes it a protocol verb too. Left open below.
- **`whirl config get <key>` as the daemon's own fallback parser.** It is named
  as a possible fallback in `features.md:287-288`, and it is not this decision:
  that sentence is about the daemon reading its own scalars, and it is left as
  written.
- **A third key source: a `0600` file the config points at (decision 2).** Lost:
  features.md 2.4's resolution list has two arms, the code implements two, and
  the divergence is already recorded in the worker
  (`whirl-worker/src/sources/wallhaven.rs:105-113`). Adding an arm would put the
  key on disk where features.md 2.4 puts it in a process's environment. If it is
  ever added it supersedes this ADR and 6.3's third sentence.
- **The UI reading the token back to show it (decision 2).** Lost: 6.3's "It is
  never echoed" (`architecture.md:1716-1720`) and features.md 2.4's logging rule
  (`features.md:440-441`). A UI that can print the token is a UI that puts it in
  a screenshot, a crash report or a support thread.
- **A frontend crate in this workspace (decision 4).** Lost: it would drag a GUI
  toolkit into a workspace whose whole dependency policy is zero third-party
  crates (`docs/development.md:273`), and 3.5's reason for the protocol being
  the only interface stops being true the moment one process holds both.

## Consequences

- **Easier:** the UI works in the state users actually open it in, with no daemon
  (measurement 3 and 4); the write surface is the one features.md 1.1 already
  chose; the key never enters the UI's process or `argv`; and whirl gains no
  protocol verb, no dependency and no crate for its frontend.
- **Harder:** the daemon-not-running case makes the UI mirror 4.3's ordering
  rules locally, and mirrored rules drift; the drift is bounded by the daemon
  being the validator on the next start. A settings change is visible at the next
  rotation, not at the write.
- **Forbids:** a `config get`/`config set` verb for a settings UI; a UI that
  starts, stops, restarts or installs the daemon or its unit; a UI that reads the
  token back; a UI that reads the config file as the effective plan; a frontend
  crate, binary or toolkit in this workspace.
- **Reversed when:** the daemon cannot be woken to answer a settings write at all
  (the open question below), or a frontend needs something the socket cannot
  carry, or the platform stores turn out to be unusable from the frontend's own
  process. Each is a superseding ADR.
- **Enforced by:** section 8's "must never" list as a review checklist for any
  frontend, and the existing refusals this document leans on: the `bad_config`
  refusal for a key-shaped `api_key_ref` (`config.rs:1260-1289`) and the
  `api_key_ref` label rule. The re-read of 4.3 and the `config_reloaded` event
  have no test today, which is the gap this decision makes visible; the change
  that implements them is named below.
- **Deliberately untouched:** the nineteen verbs and 2.5.1's table (`config path`
  stays the only way a client learns the path, and it still needs a daemon);
  `docs/spec/features.md`, which already says what 1.1 and 2.4 needed to say;
  the worker's two-armed key resolution; and `docs/milestones.md`.

## Implied changes

The documentation lines this decision implies, one line each, so the change that
makes them needs no other source. "No text change" rows are changes where the
decision rests on a document that already says the right thing and the missing
half is code.

`docs/architecture.md`

1. **section 8, "May rely on"**: add a bullet naming the config file as the write
   surface with the writer's contract of decision 1, and a second bullet naming
   the separate-repository rule and the reference frontend's URL of decision 4.
2. **section 8, "May rely on"**: add a bullet for the one start-at-login control:
   the frontend's own login item is its own, the daemon's unit is the product's
   and is reported, not owned (decision 3).
3. **section 8, "Must never" 6**: narrow the sentence so reading the config file
   to edit it is allowed and reading it as the effective plan is still forbidden.
4. **section 8, "Must never" 3**: add "install the supervisor unit" to the list
   of daemon-lifetime actions, alongside start, stop and restart.
5. **section 2.5.1**: no row changes and no verb is added; add one sentence
   after the table recording that no verb reads or writes a config value and
   that a client edits the file under section 8, so a later reader knows
   `config get`/`config set` were considered and refused.
6. **section 2.10**: no new key; add a sentence to the `source:` row and its
   legend pinning that `enabled=` and `reason=` are the worker's last answer and
   not the daemon's startup copy, which is what 4.3 already claims and
   measurement 6 shows is not yet true.
7. **section 6.3**: add the write side of the token (the UI writes the store, by
   the exact mechanism per platform, never through `argv`), narrow "It is never
   in argv" to name `security add-generic-password -w` as the trap, and record
   two facts: the `0600`-file arm is not implemented, features.md 2.4 governs,
   and the Linux and Windows store arms are not implemented either, so the
   environment variable is the only arm on those platforms today.

`docs/quickstart.md`

8. Replace "Not implemented yet: the launchd agent..." (lines 97-99) with the
   install step for the supervisor unit, which is whirl's installation and not
   the daemon's, and note that the reference frontend is a separate repository.

`docs/README.md`

9. The `decisions/` row (line 19) names two ADRs, `0001-...` and `0002-...`.

`docs/decisions/`

10. This file, `0002-frontends-write-config-own-no-daemon.md`, and no edit to
    `0001`.

Code implied by the same decisions, named so the work has a home:

11. The daemon re-reads the config at each rotation and emits `config_reloaded`
    on success, keeps the previous config on failure (4.3's promise, and the
    event `events.rs` already declares).
12. The daemon fills the `source:` records it prints in `status` and `sources`
    with the worker's `enabled` and `reason` (4.3, measurement 6).
13. The supervisor unit is generated and installed by whirl's installation
    (5.2/5.3/5.4), with the documented install and uninstall step.
14. The worker's store arm for Linux and Windows (measurement 9).

## Left open

**Not decided here: a collection preview.** If a thumbnail or collection preview
lands, it takes one of two surfaces, a daemon verb (for example `thumb`) or a
frontend reading the local directory a source already points at. The consequence
differs either way, which is why it is not decided now. As a verb, it is new
grammar in 2.5 with its own blocking, timeout and error-code rows, a new CLI row
in 2.5.1, and a second class of cached file to answer to the eviction rules of
`docs/spec/state-and-cache.md` 5; section 8 gains nothing, because a client
pulling previews on demand is still not polling. As a directory read, the
frontend needs the resolved path of a `local` source, which the `source:` record
of 2.6 and 2.10 does not carry today, so the route either gains a path field on
that record or sends the frontend to the config file, which is 8, "must never"
6; it also re-implements `~` expansion, the include glob and the header read that
the worker's pipeline owns. A later change answers it; the question that settles
it is whether a preview must work with the daemon down, because a verb needs the
daemon and a directory read does not.

Two smaller questions, left open with what would settle each:

- **Does the daemon re-read the config more often than once per rotation?** It
  wakes in bounded slices to compare the deadline (5.5), so a re-read there would
  make a settings write visible within seconds. The cost is a config read every
  slice. Settled by a change that measures the settings-change latency users
  actually tolerate against that read.
- **Does whirl gain an install verb for the supervisor unit, so the UI may ask
  for it?** Decision 3 says the frontend does not install it; whether the product
  offers a command the frontend may run is a new verb and its own ADR. Settled by
  whichever change implements the unit.
