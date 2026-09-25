# whirl v0.1: feature set and the source abstraction

Status: draft for review. Written before the code it describes.

This document fixes the v0.1 line so later requests can be answered with "not in
v0.1" instead of a debate. It decides three things: what ships, what a "source"
is as data, and what is explicitly out of scope.

Reading conventions used throughout:

- Values taken from an outside source carry a bracketed number, `[1]`, resolved
  in the Sources block at the end.
- `decision:` marks a call made in this document. Decisions are not sourced, they
  are owned here.
- `assumption:` marks a call that is waiting on a card that has not answered yet,
  with the fallback named in the same paragraph.
- `evidence:` marks something observed by running a command, with the command
  named so the next person can rerun it.

The rule every row below is measured against, from `README.md`:

> The resident process owns state. It never owns pixels.

## Part 1: the v0.1 feature set

Every feature carries a reason to exist in v0.1 and a cost. Anything that could
not produce both went to Part 3 instead.

| # | Feature | What it does | Why it is in v0.1 | Cost |
|---|---|---|---|---|
| F0 | Daemon/worker split | A resident daemon owns config, schedule, history, favorites and status. Every pixel-touching operation runs in a worker process that exits when the rotation ends. | This *is* the design rule. Without it, every feature below pays rent in resident memory forever. | One extra process per rotation, 21-25 MB for ~1.5 s, then gone. Measured for the spike in `prototype/README.md`. |
| F1 | Config file | One file, at a fixed path, fully commented when generated, read at start and re-read on `SIGHUP`/`whirl reload` (v0.2 if the reload is not free). | Config-as-data is what lets sources be data (Part 2) and keeps the GUI out of scope. | A parser in the daemon for the handful of scalars it needs, plus a full parser in the worker. Zero resident cost beyond the parsed scalars. |
| F2 | Weighted rotation over N sources | Each rotation picks one source by integer weight, then one candidate from it. | Multiple sources with weights is the smallest thing that makes "local folder plus Wallhaven" work as one rotation. | One integer draw per rotation, O(N) over a list read from config. |
| F3 | Control surface (CLI verbs) | The verb set in 1.1 over the daemon's line protocol, `0600` unix socket, named pipe on Windows. | The protocol is the API, so the CLI is the only required client and no frontend has to own state. | One thin binary, 0 MB resident (it exits). Socket `0600` and a peer check on the daemon side. |
| F4 | Pause / resume | `pause` stops the scheduler from rotating; `resume` re-arms the next slot from now. The current wallpaper is untouched either way. | The single most-used control on a rotator: stop it without uninstalling or killing it. | One bool in state, already in the protocol. |
| F5 | History and `prev` | A bounded ring of the last 50 set entries (`id`, source kind, origin, cached path, timestamp), persisted, and a verb to step back through it. | `prev` is the undo of a rotation. Without it the only remedy for a wallpaper you dislike is a manual search. | ~50 short lines in a state file, a few KB. No images. |
| F6 | Favorites | See 1.2. A separate collection of records, pinned in the cache. | Users keep images they like; this is the one curation primitive that pays for itself without a browser or a gallery. | Records in state plus a pin flag the evictor honours. Disk bounded by the user's own favorites, not by cache policy. |
| F7 | Multi-display policy | See 1.3. Default: one image everywhere. | "Rotation that just works" has to have an answer for two monitors, and the answer must not drag a resident toolkit in. | Zero, in the default mode: one setter call per display or one OS-wide call, depending on what the platform research confirms. |
| F8 | Failure handling | See 1.4. No network, no candidates, unreadable image, full disk. | A rotator that can leave a black desktop is worse than no rotator. This is the feature that makes the daemon safe to autostart. | Bounded retries inside one worker run. No retry queue in the daemon, no watcher thread. |
| F9 | Startup behaviour | See 1.5. Set a wallpaper at login, and do not stomp an image the user set by hand since the last rotation. | The daemon is started by the OS supervisor, and login is the one moment a wallpaper app is judged. | One read of the current desktop image at startup. No polling, no watcher. |
| F10 | `status` and `sources` introspection | Machine-readable `key: value` output: state, rotation count, next-in, last entry, cache count, per-source last outcome and disabled reason. | Debuggability without a GUI, and the honest place for "this source is disabled because it needs an API key". | A few lines of formatting per verb. Keys are stable and are part of the protocol contract. |

### 1.1 The control surface

`decision:` v0.1 ships this verb set and no more:

| Verb | Effect |
|---|---|
| `whirl next` | Rotate now: pick a source by weight, pick a candidate, set it. |
| `whirl prev` | Set the previous entry in history. |
| `whirl set <path\|id>` | Set an explicit file path, or a history/favorite entry by id. |
| `whirl status` | One block of `key: value` lines. |
| `whirl pause` / `whirl resume` | As F4. |
| `whirl history [n]` | Most recent entries first, default 10. |
| `whirl favorite [id]` / `whirl unfavorite <id>` / `whirl favorites` | As F6. Defaults to the currently displayed entry. |
| `whirl sources` | Each configured source, its weight, its last outcome, and its disabled reason if it has one. |
| `whirl config path` / `whirl config check` | Print the config path; validate and print the effective plan (which sources load, which filters apply). `check` runs in the worker, so a bad source is caught before it costs a rotation. |
| `whirl idle` | Block until state changes. For frontends, so none of them polls. |
| `whirl version` | Daemon version and protocol version. |

`decision:` deliberately not in v0.1, and why each can wait:

- `search`, `browse`, `preview`: an interactive browser is a frontend, and a bad
  one is the first step back toward a gallery app.
- `tag`, `blacklist`, `curate`: curation is a non-goal (Part 3). Editing a tag
  list in a text file already works.
- `schedule set`, `interval set`: writing the config file is the interface.
- `daemon start|stop|restart`: lifecycle belongs to launchd, Task Scheduler or a
  systemd user unit. A second supervisor inside the product competes with the
  first one, and that is how a daemon ends up autostarted twice.
- `stats`, `export`, `import`: nothing consumes them yet.
- `blur`, `effects`, `crop`: image editing, non-goal.
- `whirl next --source x`: forcing a source is a debugging move; `whirl sources`
  plus an edit of `weight` covers it, and a per-verb source override invites a
  second, ad-hoc selection path.

The CLI is a client of the line protocol, not a special case of it: every verb
above is one or two protocol commands, and no protocol command exists only to
serve the CLI.

### 1.2 Favorites

`decision:` a favorite is **a separate collection**, not "never delete this cached
file".

Why, in one line: a cache-entry rule gives no answer when the file is gone for a
reason the evictor did not cause (cache cleared, cache directory moved, platform
re-encoded, file corrupt), and it makes the cache "an LRU with holes", where the
disk bound is no longer `keep` but "keep, or everything the user ever liked".
Splitting the two keeps one eviction rule and one durable record.

The mechanism:

- A favorite is a record: `id`, `source kind`, `origin` (URL or absolute path),
  `cached path`, `added at`. Persisted in the state directory, not in the cache.
- Materialised files for favorites are pinned: the evictor skips them, and
  `whirl favorites` is instant.
- If a pinned file is missing or corrupt (user cleared the cache by hand, the
  cache directory moved), the worker re-materialises it from `origin` at use
  time: a local source re-references the path, a remote source re-downloads.
  The collection survives; the bytes are recoverable.
- Cost, stated plainly: disk is now bounded by `keep` plus the user's favorites.
  That is a bound the user controls, which is the only kind of growth this
  project accepts. Unfavoriting releases it.
- For a `local` source in `reference` mode (2.2), pinning is a no-op: the file
  lives in the user's own folder and is never copied.

### 1.3 Multi-display policy

`decision:` default is **one image on every display**, and per-display is opt-in
and conditional.

- Config: `display.mode = "all" | "per-display"`, default `"all"`.
- `"all"` is the default because it needs one call on macOS and Windows and is
  reachable from a short-lived worker on every platform the spike touched. The
  spike reports per-Space setting on macOS via `NSWorkspace.setDesktopImageURL`
  with the `allSpaces` option, and states that Windows has no per-virtual-desktop
  wallpaper API and that its `SystemParametersInfoW` path sets one image
  everywhere (`prototype/README.md`).
- `"per-display"` means each display gets its own independently chosen image.
  It requires an enumeration API, an identity that survives hotplug, and a
  per-display setter, all callable from a process that lives for one rotation.

`assumption:` whether `"per-display"` is possible at all is **not decided here**.
It is decided by the four research cards, which are in flight and have produced
no output yet. `evidence:` queried the board
(`sqlite3 ~/.hermes/kanban/boards/whirl/kanban.db "select id,status from tasks"`)
and inspected each research worktree (`git log --oneline wt/t_57b8de1e` etc. from
`/Users/govind.rajpurohit/Workspace/Personal/whirl`): `t_57b8de1e` (macOS),
`t_a7a134e2` (Windows), `t_be6f5e61` (Linux) and `t_b9e349e6` (schedulers) are
all `running`, and no `docs/research/*.md` file exists on any branch yet. The
spec deliberately does not assume their answer.

Fallback, stated now so the spec is actionable either way:

- If the research finds per-display reachable from a short-lived process on a
  platform, `"per-display"` is honoured there in v0.1.
- If it is not reachable, or requires a resident connection to a compositor,
  `"per-display"` is rejected at `whirl config check` with that platform's
  reason, and v0.1 ships `"all"` only. The key stays in the schema so the config
  does not have to change later.
- On a platform where the research has not answered yet, `"per-display"` is
  accepted and runs as `"all"`, with one warning line in the log and a
  `display_mode_effective: all` key in `whirl status`. Failing loudly on an
  unanswered platform would make the same config file portable on one machine
  and not another for a reason the user cannot act on.

Hotplug, in both modes: a display connected or disconnected since the last
rotation is picked up at the next rotation. `decision:` v0.1 does not watch for
display changes, because a watcher is resident code and a resident watcher is
exactly the pattern that produced the 883 MB app. Cost: a newly connected display
shows the OS default until the next rotation.

`decision:` `display.per_display_images` (a distinct image per display) is not a
separate feature; it is what `"per-display"` means, and it inherits the same
platform dependency.

### 1.4 Failure behaviour

All four cases below are what the *worker* does. The daemon's job is to record
the outcome and keep the next slot on the normal cadence.

**No network.**

1. Sources that do not need the network (`local`) are still tried, in weighted
   order, if any are configured.
2. If every source needs the network, the rotation fails. The currently displayed
   image is untouched, the failure is logged, `status` gains
   `last_error: offline`, and `whirl next` exits non-zero with that message.
3. No backoff storm: the next attempt is the normal next slot, not a retry loop.
   A laptop that closed its lid in a tunnel should not wake up hammering a CDN.

**No candidates** (the source answered but the filter pipeline left nothing).

1. The worker walks the remaining sources in descending weight order, at most one
   attempt per source per rotation.
2. If all sources yield nothing, the rotation fails as above with
   `last_error: no_candidates`. `whirl config check` is the tool that shows which
   filter removed what (F10, 2.5).

**A displayed image fails to load.**

1. The setter is called only on a file that was fully downloaded (atomic rename
   from `*.part`) and header-validated: non-zero size, a known image magic, and
   readable dimensions. `decision:` header validation, not a full decode, because
   the worker is about to hand the pixels to the OS anyway and a second decode is
   the cost this project exists to avoid.
2. If the setter fails, the bad cache entry is deleted, the candidate id is
   marked bad for the rest of the run, one more candidate is tried.
3. If that also fails, the worker exits non-zero. It never blanks the desktop:
   the OS keeps the previous wallpaper when a set call fails.
4. A file that is missing or corrupt is re-materialised from its recorded origin
   once (a local reference, or a re-download), then deleted and reported if it
   fails again.

**Disk full or unwritable cache.** The `*.part` file is removed, the error is
reported, no state is half-written (state writes are temp-file plus rename).
`decision:` no automatic cache wipe on disk-full; evicting the user's pins to
save space is a surprise, and `whirl config check` plus the status line is the
honest signal.

### 1.5 Startup behaviour

`decision:` whirl sets a wallpaper at login, conditionally.

- `startup.enabled` (default `true`): on daemon start, apply a wallpaper. The
  daemon itself is started by launchd, Task Scheduler or a systemd user unit;
  which one, and how missed runs across sleep are handled, is
  `docs/research/scheduling.md` (card `t_b9e349e6`, `running`).
- `startup.mode = "last" | "rotate"`, default `"last"`: re-apply the last entry
  in history, or pick a new one. `"last"` avoids burning a Wallhaven request and
  a download on every login.

**An image the user set manually since the last rotation.**

`decision:` whirl respects it by default.

- At startup the daemon reads the current desktop image if the platform exposes
  it, and compares it with `state.last_path`.
- If they differ, the user changed it by hand. whirl records the difference as an
  `external` entry in history (so `prev` comes back to it) and, under
  `startup.mode = "last"`, does not overwrite it.
- Config: `startup.respect_manual` (default `true`). Setting it to `false` means
  "always re-apply mine", which is the behaviour for someone using whirl as the
  single source of truth.
- The comparison is a path comparison, not a hash: cheap, and it is the only
  signal the OS APIs are likely to give.
- `assumption:` reading back the current desktop image is not confirmed on every
  platform. The macOS research card has the API candidates; Windows has
  `SPI_GETDESKWALLPAPER`; the Linux answer is the least likely to be uniform.
  Fallback if a platform cannot report it: `respect_manual` degrades to `false`
  on that platform, that is logged once at startup, and `whirl status` reports
  `respect_manual_effective: 0`, so "why did it overwrite my picture" has an
  answer in the tool's own output.

## Part 2: sources are data

A source is a config entry. It is not a compiled-in provider, not a dynamic
plugin, and not a separate binary. The test of this section is 2.6: a contributor
adds a source without asking a question, and without the daemon, the protocol or
the cache changing.

### 2.0 Which process reads what

`decision:` the config file is one JSON file. The daemon parses only the top-level
scalars it needs (socket path, interval, cache directory, `keep`, log level,
`display.mode`, `startup.*`). The `sources` array is interpreted **only by the
worker**, which reports the effective plan back (`whirl sources`,
`whirl config check`).

Why: it keeps the daemon's parser small enough to stay dependency-free, and it
means a new source kind is invisible to the daemon. Cost: two parsers, the small
one in the daemon and the full one in the worker. Fallback if the daemon's parser
becomes a burden: the daemon may shell out to `whirl config get <key>` and cache
the scalar, the way the spike's daemon execs the rotator rather than linking it.

Config validation policy: an unknown field is a warning; an unknown `kind` or a
missing required field is an error naming the known kinds and the offending
source `id`. Silent skips are how a config file becomes a mystery.

### 2.1 Shared source fields

| Key | Type | Default | Meaning |
|---|---|---|---|
| `id` | string | required, unique | Stable name for status, history and error messages. |
| `kind` | string | required | `local`, `wallhaven`. The only place the type of a source appears. |
| `weight` | integer >= 0 | `1` | Relative chance of this source being chosen per rotation. `0` disables without deleting. |
| `min_width`, `min_height` | integer | global values | Per-source override of the shared resolution filter. |
| `max_bytes` | integer | global value | Per-source override of the shared size cap. |

### 2.2 `local` source schema

```json
{
  "id": "pictures",
  "kind": "local",
  "weight": 1,
  "paths": ["~/Pictures/Wallpapers", "/Volumes/Media/walls"],
  "recursive": true,
  "max_depth": 8,
  "follow_symlinks": false,
  "include": ["*.jpg", "*.jpeg", "*.png", "*.heic", "*.webp"],
  "exclude": ["*/.git/*", "*/screenshots/*", "*.tmp"],
  "min_width": 2560,
  "min_height": 1440,
  "mode": "reference"
}
```

| Key | Type | Default | Meaning and cost |
|---|---|---|---|
| `paths` | array of strings | required | One or more directories. `~` is expanded. A path that is missing or unreadable is a warning; it is an error only if every path fails. Cost: one `stat` per path per rotation. |
| `recursive` | bool | `true` | Walk subdirectories. |
| `max_depth` | integer | `8` | Hard bound on the walk. Cost: bounds a runaway tree (a symlinked home directory) at a predictable scan instead of an unbounded one. |
| `follow_symlinks` | bool | `false` | Default off to make loops impossible rather than merely unlikely. Cost: a symlinked wallpaper folder has to be named as a real path. |
| `include` / `exclude` | array of globs | as above | Matched against the path relative to the source root. Exclude wins over include. |
| `min_width`, `min_height` | integer | global | Applied by reading the image header, never the filename. `decision:` a file whose header cannot be read is excluded and logged at debug, because a file we cannot measure is a file we cannot promise will display. |
| `mode` | `"reference"` \| `"copy"` | `"reference"` | `reference` sets the wallpaper from the original path: zero extra disk, the user's folder stays the source of truth. `copy` copies into the cache first, which is what a removable volume or a network share needs, because a wallpaper pointing at an unmounted volume is a broken wallpaper. Cost of `copy`: disk equal to the image, and the file enters cache eviction. |

Format support, with its cost stated: JPEG and PNG everywhere. `decision:` HEIC is
settable on macOS only; on Windows and Linux the worker skips an HEIC file with a
log line, and v0.1 does **not** convert it. Converting means shipping a decoder,
and a decoder is the kind of dependency this project is a reaction against.

### 2.3 `wallhaven` source schema

```json
{
  "id": "space",
  "kind": "wallhaven",
  "weight": 3,
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
  "api_key_ref": null
}
```

Every key maps to a documented query parameter or endpoint:

| Key | API parameter | Notes |
|---|---|---|
| `query` | `q` | Supports the site's tag syntax, including `+tag`, `-tag`, `@user`, `id:`, `type:`, `like:` [1]. |
| `categories` | `categories` | Three flags, general/anime/people, `111` is all three [1]. |
| `purity` | `purity` | `100` sfw, `110` sketchy, `111` nsfw. NSFW requires a valid API key [1]. |
| `sorting` | `sorting` | `date_added`, `relevance`, `random`, `views`, `favorites`, `toplist` [1]. |
| `order` | `order` | `desc` or `asc` [1]. |
| `ratios` | `ratios` | `16x9,16x10` [1]. |
| `atleast` | `atleast` | Minimum resolution, e.g. `2560x1440` [1]. |
| `colors` | `colors` | Hex colour, optional [1]. |
| `top_range` | `topRange` | Only meaningful with `sorting=toplist` [1]. |
| `collection` | path segment | Uses `/api/v1/collections/USERNAME/ID` instead of search. That endpoint exposes only the `purity` filter, so every other filter is applied locally by the shared pipeline [1]. |
| `pages` | `page` | How many pages to sample when building a candidate list. One page is 24 results [1]. `decision:` default 1, hard cap 5. Cost: one API call per page, against a documented 45 requests per minute limit [1]. |
| `api_key_ref` | - | A name, never a value. See 2.4. |

This is the only Wallhaven surface v0.1 touches:

| Endpoint | Purpose | Key required? |
|---|---|---|
| `GET https://wallhaven.cc/api/v1/search` | Candidate listing. Without parameters it returns the latest SFW wallpapers [1]. | Not for SFW. NSFW needs a valid key, otherwise `401 Unauthorized` [1]. |
| `GET https://wallhaven.cc/api/v1/w/<id>` | Single wallpaper metadata. | Not for a public SFW wallpaper; NSFW is blocked to guests [1]. |
| `GET https://wallhaven.cc/api/v1/collections/<username>` | Another user's public collections. | No, public collections only [1]. |
| `GET https://wallhaven.cc/api/v1/collections/<username>/<id>` | The wallpapers in a collection. | No for a public collection; yes for one of your own that is private [1]. |
| `GET https://wallhaven.cc/api/v1/settings` | Read the account's settings and blacklists. | Yes, always. `decision:` not used in v0.1; the response would be a second, hidden source of filtering the user cannot see in the config file. |

Not in v0.1: `/api/v1/tag/<id>` (nothing consumes tag metadata yet) and the toplist
browsing flow (an interactive browser is a non-goal).

### 2.4 The API key rule (hard requirement)

**A Wallhaven API key is never stored in the config file.** Not in the shipped
default, not in an example, not in the repository, not in a note, not in a commit.
The config file is expected to be one of the things a user shows someone else, and
keys in config files are how they leak.

Resolution order at runtime, first hit wins:

1. Environment variable `WHIRL_WALLHAVEN_API_KEY`.
2. The OS secret store, by label. `api_key_ref` names the label, never the value:
   - macOS: Keychain, item `whirl-wallhaven`, e.g. `security find-generic-password
     -s whirl-wallhaven -w`.
   - Linux: Secret Service over `secret-tool lookup service whirl key wallhaven`.
   - Windows: Credential Manager, generic credential `whirl/wallhaven`.
3. Nothing found: the key is treated as absent, per the rule below.

Transmission: the key travels in the `X-API-Key` request header, not in the query
string [1]. Cost of that choice: none, and it keeps the key out of URLs, which
means out of the daemon's log file, out of any proxy log, and out of the error
string the worker prints.

When a key is genuinely required:

- `purity` includes `110` (sketchy) or `111` (nsfw). Documented: NSFW wallpapers
  are blocked to guests, and an NSFW request without a key or with an invalid one
  returns `401 Unauthorized` [1].
- A private collection of your own [1].
- `/api/v1/settings`, which v0.1 does not call [1].

When no key is needed:

- `search` restricted to `purity=100` [1].
- `/api/v1/w/<id>` for a public SFW wallpaper [1].
- A public collection of any user [1].

Fallback when there is no key. `decision:` a key-requiring source is **disabled at
load**, not silently downgraded to SFW. Silent downgrade is the wrong failure: the
user asked for a purity level and would get a different one with no signal. So:

- `whirl config check` fails that source with
  `source 'x': purity=111 requires an API key, none resolvable (checked env
  WHIRL_WALLHAVEN_API_KEY, keychain label 'whirl-wallhaven')`.
- `whirl sources` lists it as `disabled (api key)` and `whirl status` carries the
  same reason, so a rotation that is quietly drawing from two of three sources is
  visible in the tool's own output.
- Rotation continues with the remaining sources. If the disabled source is the
  only one, rotations fail as `no_candidates` in the F8 sense, with that reason,
  rather than as a mystery.

Logging rule: the key, or any prefix of it, never appears in a log line, an error
message, a state file, or `whirl status` output. Only the label is ever printed.

### 2.5 Filtering: what a source declares, and what the pipeline owns

Two layers, and the split is the point.

**Layer 1, pushed down to the source.** A source declares its capabilities, and
the pipeline pushes only those filters into the request:

```json
"capabilities": ["resolution", "ratio", "purity", "colors"]
```

- `wallhaven` declares `resolution` (`atleast`), `ratio` (`ratios`), `purity`,
  `colors`, `category` [1].
- `local` declares `resolution` and `extension`. A local filesystem can answer
  these with a glob and a header read.
- A capability not declared is not attempted at the source; it is applied in
  Layer 2. That is what keeps "a source declares its own filtering" honest: the
  source says what it can answer, the pipeline answers the rest, and the result is
  the same set of images either way.

**Layer 2, the shared pipeline.** Runs in the worker, in this order, on the
enumerated candidates:

1. Resolution: `min_width` x `min_height`, global with per-source override.
   Rationale for a floor at all: a 1024x768 image on a 2560x1440 display is a
   visibly bad rotation and the cheapest filter to run.
2. Aspect ratio: `filters.ratio_tolerance` (default `0.02`), against
   `filters.target_ratio` (default `any`, or the primary display's ratio). A
   16:9 image on a 16:10 display gets cropped by the OS, which is fine; a 4:3 one
   gets letterboxed, which is not.
3. Size: `filters.max_bytes` (default 40 MB). Cost: one `Content-Length` or
   `HEAD` for remote, one `stat` for local, before the download.
4. Content type: JPEG and PNG everywhere; WebP and HEIC per the platform note in
   2.2.
5. Dedupe, per rotation: exclude any `id` in the recent window (last 20 entries
   in history, plus cache contents). This is the feature that stops a `random`
   query from setting the same wallpaper twice in a week.
6. Dedupe, across sources: two sources can produce the same bytes (a local copy of
   something downloaded earlier, or two overlapping queries). The worker computes
   a content hash (SHA-256) when it materialises a file and keeps an index of
   hashes in the cache; a candidate whose bytes are already present is deduped.
   Cost: hashing up to `max_bytes`, roughly 0.1 s, in the worker, which is a
   process that is about to exit. The daemon does not see the hash and does not
   index anything.

Filter reporting: `whirl config check` prints, per source, the candidates found and
the candidates removed per stage. Cost: a debug run, no resident state.

### 2.6 Adding a new source: the test

What a contributor writes:

1. One file implementing the worker's source interface, with three methods:
   - `validate(config) -> Result<(), Error>`: reject a config that cannot work,
     with a message naming the offending key.
   - `enumerate(ctx) -> Vec<Candidate>`: return candidates as
     `{id, origin, width, height, bytes?}`. It may be lazy; nothing but metadata
     comes back at this stage.
   - `capabilities() -> FilterSet`: which Layer 1 filters it can push down.
2. A row in the source table in this document, and in the config example in
   `docs/development.md` when that exists.
3. A fixture test: a recorded or synthetic source response, and the expected
   candidate list after the shared pipeline.

What a contributor must **not** have to touch:

- the daemon, in any file;
- the line protocol, or any CLI verb;
- the filter pipeline, the cache, the evictor, or the per-OS setters;
- the config schema of any other source.

One honest exception, stated rather than hidden: a compiled language needs a
place where the `kind` string becomes a type, so the contributor adds **one line**
to the source factory:

```text
"wallhaven" => wallhaven::source(cfg),
"local"     => local::source(cfg),
"<new>"     => newkind::source(cfg),   // the only line a contributor adds
```

`decision:` that line is allowed, and it is the whole test: it must be one line,
it must contain no logic, and if adding a source ever needs a second line
anywhere, the abstraction failed and the source shape (not the contributor's
diligence) gets revisited. A compile-time registry beats a "dynamic plugin" that
loads code into a resident process, which is precisely the memory pattern this
project exists to avoid.

## Part 3: non-goals

Out of scope for v0.1. Each has a reason of one line.

| Non-goal | Reason |
|---|---|
| A settings GUI | A resident toolkit is the 883 MB failure mode, and the config file is the interface. |
| A tray icon as a requirement | An optional thin frontend is fine; requiring one makes every install carry a desktop GUI stack. |
| Video and live wallpapers | Continuous decode and a resident compositor connection, the exact per-frame residency the design rule forbids. |
| Cloud photo accounts (Google Photos, iCloud, Dropbox) | Each one is an OAuth client, a token store and a pagination API, for zero resident-memory benefit. A synced folder is already a `local` source. |
| Image editing, crop or blur on the fly | Pixel work in the daemon, and the rotated file is no longer the file the user favourited. |
| Tagging and curation | Turns the product into a gallery, which `README.md` names as a non-goal, and needs a store, not a cache. |
| Multi-user or remote control | The socket is `0600` and local by design; remote control is an attack surface bought with no user benefit. |
| Per-display images before the platform research answers | Stated as a dependency, not an assumption (1.3): the answer decides it, and it may be a v0.2 item. |
| Windows and Linux *per-virtual-desktop* differentiation | No documented API on Windows; Linux is per-DE and partly compositor-specific. Wait for `docs/research/windows.md` and `linux.md`. |
| A wallpaper browser, gallery or preview UI | Named as a non-goal in `README.md`; browsing belongs to a web frontend over the protocol, if anyone wants one. |
| Dynamic plugin loading (`dlopen`-style sources) | Loads foreign code into the resident process; the source factory in 2.6 gets the same extensibility for free. |
| Colour extraction, palettes, statistic reporting | Nothing consumes them, and each is a decode in the daemon or an extra worker pass per rotation. |
| Scheduling rules beyond a fixed interval (work hours, holidays, per-workspace) | The OS timer plus a fixed interval covers "just works"; a rules engine needs a calendar and a state machine in the resident process. |
| Packaging, code signing, installers, auto-update | Real work, but it is release engineering, not v0.1 product scope; `docs/development.md` owns it. |

## Acceptance check

Against this card's criteria:

- **Every feature states a cost and a reason.** Table in Part 1, cost column
  mandatory; F1 through F10 each carry both. Anything that could not got a row in
  Part 3 instead (`search`, `blur`, `stats`, a browser, scheduling rules).
- **The source schema is concrete enough to add a source without a question.**
  2.2, 2.3 and 2.6: required keys, defaults, the interface's three methods, the
  fixture a contributor writes, and the one factory line they are allowed to add.
- **The Wallhaven section names the exact endpoints and which need a key, with a
  citation.** 2.3 and 2.4, with `wallhaven.cc/help/api` as `[1]` for every
  endpoint, parameter and the 45-a-minute limit [1].
- **The multi-display policy names its dependency.** 1.3 names the research cards
  by id, states that they are `running` with no output yet (with the command that
  showed it), and gives the fallback in both directions.
- **No feature requires a resident GUI toolkit.** Audit, per feature: F0 is the
  absence of one; F3 is a socket client that exits; F6 is a list in a state file;
  F7 uses the platform's wallpaper API from a worker and explicitly does not watch
  for display changes; F8 and F9 are one-shot worker and startup logic; the tray
  icon is a non-goal, not a deliverable. Passes.

Grounding note: this is a design document, so most sentences are decisions this
document owns rather than facts lifted from elsewhere. Only the Wallhaven API
surface is sourced, and `sources.py verify docs/spec/features.md` reports
`citations OK` with 9 cited sentences against 317 prose sentences. That 3% is the
intended shape: the sourced fraction is small and every sourced sentence is
traceable to `[1]`. Read anything unpinned as a decision, not as an observation.

## Sources

[1] https://wallhaven.cc/help/api — API v1 - wallhaven.cc
