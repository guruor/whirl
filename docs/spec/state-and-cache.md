# whirl: state, cache layout, and eviction

Status: draft, review round 2. Specification only, no production code. Round 1's
two blocking defects are fixed: the INV-CACHE-2 check is now stated on a config
that 5.1 declares legal and that a daemon will actually start with (5.4), and the
degraded-favorites case is now named as a fourth permitted overshoot cause with
the sweep's behaviour spelled out instead of a hidden exemption (5.3, 5.5, 6.4,
8.7). Round 1's non-blocking caveats are also applied: the byte-cap sample is
reported as three draws rather than one (5.1, [L 6], [L 8], [L 9]), and three
citation imprecisions and one wrong section number are corrected. This revision
adds the rule the document did not have for the `excl_file` lock file: which
release sentence is true of which lock (7.2), what the fallback records (8.7), and
what the daemon does when it finds `state/locks/daemon.lock` held by a holder
that is gone (8.8), including the `rotate.lock` case and what the daemon reports,
which is a startup message rather than a `status` key.

This document fixes where whirl keeps its files, what each file contains, what
gets deleted and when, and which process is allowed to write what. It is written
so that an implementer needs no follow-up question, and a reviewer can check the
result against a running install.

**It supersedes, for state and cache only:**

- `prototype/wh-rotate/main.go`: the cache location, the `wh-<id>.<ext>` naming,
  and `prune()` (count-only, mtime-ordered, no protection for the displayed
  image, skipped on two exit paths).
- `prototype/whd/whd.rs`: the state location, the newline-separated history and
  favorites files, and their non-atomic `fs::write`.
- `prototype/README.md`: the defaults (`~/Pictures/Wallhaven` as a cache,
  `~/.config/wh-rotate/config.json` as config on every platform, `keep: 40`).

**It refines, without changing the feature set:** `docs/spec/features.md`. Where
this document and that one touch the same key, the rule is named in section 9.

Reading conventions, the same as `docs/spec/features.md`:

- `[n]` a source outside this repository, resolved in the Sources block.
- `[L n]` a local artifact or a command run on this machine, in the local table.
- `[D n]` a sibling research document in this repository.
- `decision:` a call this document owns. `evidence:` something observed by
  running a command. `assumption:` a call waiting on an answer that does not
  exist yet, with the fallback named in the same paragraph.

## 0. The rules, on one page

| # | Rule | What it prevents |
|---|---|---|
| R1 | State and cache never share a directory, on any platform. | A "clear the cache" that destroys favorites, and a state reset that forces a re-download. |
| R2 | Every file in the state directory has exactly one writing process: the daemon. | Half-written state when the daemon and a worker run at once. |
| R3 | The worker writes only inside the cache, only to content-addressed paths, only while holding the rotation lock. | Two processes renaming over each other; a partially downloaded file becoming a wallpaper. |
| R4 | Nothing is set as wallpaper except a file that has been fully written and renamed into place. | A torn image on screen; the prototype's `*.part` becoming a candidate. |
| R5 | The file the platform is currently displaying is never deleted by whirl, whatever the disk pressure. | Deleting the anchor out from under the platform, which on macOS leaves a broken store entry [D 1]. |
| R6 | Both a byte cap and a count cap, whichever binds first. | The 354 MB cache this project exists because of, and the same footprint hidden behind a small file count. |
| R7 | Identity is content, not filename and not URL. | The same image set twice in a week, twice on disk under two names. |
| R8 | Every state write is temp file, fsync, rename, in the target's own directory. | A reader parsing a file at the moment of its truncation [L 5]. |

## 1. Roots and the per-platform layout

`decision:` three roots, and one of them per platform convention rather than one
shared `~/.config/whirl` everywhere (which is what the prototype does on macOS
today).

The sentence that decides which file goes where on macOS is Apple's own:
`Caches` "Contains cached data that can be regenerated as needed. Apps should
never rely on the existence of cache files" [2]. A file whirl must not lose does
not go in `Caches`. The XDG spec says the same thing in different words:
`$XDG_CACHE_HOME` is for "user-specific non-essential (cached) data", while
`$XDG_STATE_HOME` "contains state data that should persist between (application)
restarts, but that is not important or portable enough to the user that it
should be stored in `$XDG_DATA_HOME`", and it names "actions history (logs,
history, recently used files, …)" as its content [3]. On Windows the same split
exists in the folder set itself: `FOLDERID_RoamingAppData` (display name
"Roaming", default `%APPDATA% (%USERPROFILE%\AppData\Roaming)`) against
`FOLDERID_LocalAppData` (display name "Local", default `%LOCALAPPDATA%
(%USERPROFILE%\AppData\Local)`) [4], and CSIDL describes the local one as "a data
repository for local (nonroaming) applications" [6].

### 1.1 macOS

| Path | Contents | Deletable at any moment? |
|---|---|---|
| `~/Library/Application Support/whirl/config.json` | The config file. The only file the user edits. | No. User data. |
| `~/Library/Application Support/whirl/state/current.json` | Live scalars: pause, counters, next slot, the display anchor. | Yes. Derived, rebuilt on start (section 6.4). |
| `~/Library/Application Support/whirl/state/history.json` | Ring of the last 50 entries. | No, but low value: bounded convenience. |
| `~/Library/Application Support/whirl/state/favorites.json` | The favorites collection. | No. The one irreversible file (section 6.4). |
| `~/Library/Application Support/whirl/whirl.sock` | The control socket, `0600`. | Yes. Recreated on start. |
| `~/Library/Caches/whirl/` | `index.json`, `sha256/**`, `tmp/**`. The whole directory. | Yes, for whirl. See 1.4 for the display caveat. |
| `~/Library/Logs/whirl/whirl.log` | The log, one file rotated at 1 MiB, three kept. | Yes. Diagnostics only. |

`decision:` the directory is literally `whirl`, not a reverse-DNS bundle id.
Apple's convention is "a subdirectory whose name matches the bundle identifier
of the app" [2]; whirl v0.1 ships as a bare binary with no bundle, so the name
is the product name. If whirl ever ships with an identifier, the directory
becomes that identifier and the move is a one-time migration, not a schema
change.

`decision:` the log goes to `~/Library/Logs/whirl/`, not into
Application Support, because Apple reserves that directory for "log files for
the console and specific system services" [2] and the `whirl/` subdirectory
follows the same naming convention as the two directories above. This is a
convention call, not a quoted requirement: Apple's table is written for the
system domain, and the reason to follow it is that a user with a disk-space
problem can delete logs without wondering whether they were state.

### 1.2 Linux (XDG)

| Path | Contents | Deletable at any moment? |
|---|---|---|
| `$XDG_CONFIG_HOME/whirl/config.json` (default `~/.config/whirl/config.json`) | Config file. | No. |
| `$XDG_STATE_HOME/whirl/current.json` (default `~/.local/state/whirl/`) | Live scalars and the display anchor. | Yes, derived. |
| `$XDG_STATE_HOME/whirl/history.json` | History ring. | No, low value. |
| `$XDG_STATE_HOME/whirl/favorites.json` | Favorites. | No. |
| `$XDG_STATE_HOME/whirl/whirl.log` | Log. | Yes. |
| `$XDG_CACHE_HOME/whirl/` (default `~/.cache/whirl/`) | `index.json`, `sha256/**`, `tmp/**`. | Yes, for whirl (1.4). |
| `$XDG_RUNTIME_DIR/whirl.sock` | Control socket, `0600`. | Yes. |

- All four variables are read as absolute paths and a relative value is treated
  as unset, which is what the specification requires: "All paths set in these
  environment variables must be absolute. If an implementation encounters a
  relative path in any of these variables it should consider the path invalid
  and ignore it" [3]. Fall back to the documented defaults above.
- `decision:` the socket prefers `$XDG_RUNTIME_DIR`, which is the specification's
  home for "user-specific runtime files and other file objects" [3] and is on a
  tmpfs that the session cleans up. If it is unset, the socket falls back to
  `$XDG_STATE_HOME/whirl/whirl.sock`.
- `decision:` the log lives under `$XDG_STATE_HOME`, which the spec explicitly
  lists as a home for "actions history (logs, history, recently used files, …)"
  [3].
  XDG has no log directory, and inventing one would be worse than using the
  documented one.
- `assumption:` whirl does not write anything into `~/.config/dconf/user`, which
  is GNOME's file and belongs to `gsettings` [D 2]. Nothing in this document
  changes that.

### 1.3 Windows

| Path | Contents | Deletable at any moment? |
|---|---|---|
| `%APPDATA%\whirl\config.json` | Config file. | No. |
| `%LOCALAPPDATA%\whirl\state\current.json` | Live scalars and the anchor. | Yes, derived. |
| `%LOCALAPPDATA%\whirl\state\history.json` | History ring. | No, low value. |
| `%LOCALAPPDATA%\whirl\state\favorites.json` | Favorites. | No. |
| `%LOCALAPPDATA%\whirl\cache\` | `index.json`, `sha256\**`, `tmp\**`. | Yes, for whirl (1.4). |
| `%LOCALAPPDATA%\whirl\whirl.log` | Log. | Yes. |
| `\\.\pipe\whirl-<user>` | The control pipe. | Yes. Recreated on start. |

- `decision:` the config is the only thing in the roaming folder, and it is
  small. History, favorites and the cache are local. The reason is the folder
  names: the roaming folder is Microsoft's "common repository for
  application-specific data" [6] and is the one whose contents are documented as
  roaming ("it will roam with the user", said on the same page of another CSIDL,
  `CSIDL_ADMINTOOLS`), while the local one is the "data repository for local
  (nonroaming) applications" [6]. History, favorites
  and the cache all reference this machine's absolute paths and this machine's
  cache bytes, so roaming them would sync a set of broken references and move
  megabytes across the profile for nothing. That is an inference from the folder
  descriptions, not a quoted rule: `[unverified]` for whether Microsoft states a
  size ceiling anywhere current.
- `decision:` paths are resolved with `SHGetKnownFolderPath` and the
  `KNOWNFOLDERID` constants, never by reading `%APPDATA%` or `%LOCALAPPDATA%`
  out of the environment, because Microsoft says so directly: "For new code,
  always use SHGetKnownFolderPath with KNOWNFOLDERID constants. The older
  SHGetFolderPath and CSIDL values are deprecated and should not be used in new
  applications" [5]. The environment variables are the same strings by default
  [4], which is why they appear in the table and not in the code.
- `decision:` the pipe name is `\\.\pipe\whirl-<user>`, where `<user>` is the
  account name, sanitised to `[A-Za-z0-9._-]` and truncated to 32 characters, so
  two users on one machine cannot collide. A pipe name may contain "any
  character other than a backslash" and the whole name may be up to 256
  characters [7], so this is well inside the documented form.
- `assumption:` per-user isolation of the pipe rests on the pipe's default
  security descriptor plus the `0600`-equivalent ACL whirl sets at creation. The
  Windows research card did not answer this, and it is listed in section 8.7 as
  an open item rather than assumed closed.

### 1.4 What is safe to delete at any moment

`decision:` the mapping above is the contract, and the reviewer can check it with
one experiment: delete the deleted-safe set, restart, and confirm the daemon
comes up with an empty history, a working rotation, and the favorites intact
(they are not in that set).

The honest caveat, and the reason the invariant in section 5.4 exists: deleting
the **cache root** is safe for whirl's bookkeeping, but it is not guaranteed to
leave the pixels alone. If the image on screen came from the cache, removing its
file can leave the platform pointing at a path that no longer exists, which is
exactly the state `[D 1]` measured on macOS (a store entry naming a file that
"no longer exists on disk", and a later apply of that entry failing with
`The file doesn't exist`). So:

- The supported way to clear the cache is `whirl reset --cache`, which clears it
  and then restores the display from the anchor's origin (section 8.2).
- `rm -rf <cache root>` is safe for whirl and may lose the image the desktop is
  showing. This document says so instead of claiming the stronger, false thing.

## 2. Cache layout

```
<cache root>/
  index.json                  the authoritative metadata (section 2.1)
  sha256/aa/bb/<64 hex>.<ext> every materialised image, content addressed
  tmp/<run>-<rand>.part       in-flight downloads, never candidates
```

`decision:` content-addressed filenames. The name is the SHA-256 of the bytes,
with a two-level `aa/bb/` fanout and the extension taken from the sniffed header,
never from the URL.

Why the name is the hash, and not the prototype's `wh-<source-id>.<ext>
(main.go:282)`:

1. Identity and filename become the same fact, so dedupe is a `stat`, not a
   table lookup that can disagree with the disk.
2. A re-materialised favorite lands at the same path it had before, which is what
   makes "the evictor skips pinned files" (features.md 1.2) a stable rule rather
   than one that breaks after a cache clear.
3. The filename doubles as an integrity check: a file whose bytes do not hash to
   its name was truncated or damaged by something else, and that is detectable
   without a second index.

Cost: a hash pass over the bytes, measured in section 4.2 at 14 ms for 21 MB,
which is done incrementally while the response streams in, so it is not a
separate read.

`decision:` the fanout is `aa/bb` for filesystem ergonomics only (a directory
listing of 50,000 entries is unpleasant on every platform; 512 entries is not).
It costs one extra `mkdir -p` on a first download. Nothing depends on it.

### 2.1 `index.json`

The index is the cache's own metadata and lives inside the cache root, so that
"delete the cache" is one operation and can never leave an index describing a
directory that is gone.

```json
{
  "schema": 1,
  "seq": 41,
  "written_at": "2026-09-25T07:41:12Z",
  "root_id": "8f1d0c2e-...",
  "entries": {
    "ab12cd34...64 hex...": {
      "ext": "jpg",
      "bytes": 3822331,
      "first_seen": "2026-09-24T22:10:03Z",
      "last_used": "2026-09-25T07:41:12Z",
      "source": "space",
      "kind": "wallhaven",
      "origin": "https://w.wallhaven.cc/full/ab/wallhaven-ab12cd.jpg",
      "origin_key": "wallhaven:ab12cd",
      "width": 2560,
      "height": 1440,
      "pinned": true
    }
  },
  "dangling": []
}
```

- `root_id` is a UUID minted when the cache directory is created. It exists so
  that a state file (which outlives the cache) can tell "the cache was cleared"
  from "the cache is a different cache": an entry whose recorded `root_id` does
  not match has to be re-materialised. Cost: one UUID, and one comparison.
- `pinned` is a cache-side copy of "some favorite record names this hash". The
  authoritative pin list is `favorites.json`, in state, which the daemon owns;
  the index copy exists so the sweep needs one file open, not two. The daemon
  writes both from the same in-memory set, so they cannot drift; if they do
  (a hand-edited file), the union wins and the sweep protects the file.
- `last_used` is the eviction key, and it is in the index rather than in the
  file's mtime on purpose: a cache hit does not write to the file, and
  `utimes` on a read-only cache directory would fail. The index is writable
  whenever the cache root is (section 8.5).
- `dangling` is a small list of entries whose file is missing, kept so that
  `whirl status` can report them before the next sweep reclaims them.
- `origin` is what `prev`, `set <id>` and the favorites re-materialisation use.
  `origin_key` is the source-scoped candidate id, used for the cheap
  recent-window check before download (section 4.1).

`decision:` there is no per-entry sidecar file and no directory per entry. One
JSON index rewrites atomically (section 6.2), and its cost at the default cap is
measured: 500 entries serialise to 166.6 KiB compact and 215.0 KiB indented, 341
bytes per entry (`python3 index_size.py 500`, [L 5]). That is one `write` per
sweep, and it is why the index can be rewritten in full rather than patched.

## 3. Download and atomic swap

The rule, stated once: **the platform setter is never handed a path that is not
the final, fully written, content-verified destination.**

The protocol, in order, for every candidate that needs bytes:

1. Take the rotation lock (section 7.2). Failure to take it means another
   rotation is in flight; the worker exits with `busy`.
2. `mkdir -p` the cache root, `sha256/`, `tmp/`, mode `0700` directories,
   `0600` files. A failure here is `cache_unwritable` (section 8.4).
3. Create `tmp/<run>-<rand>.part` with `O_CREAT|O_EXCL`. `<run>` is the daemon's
   rotation id and `<rand>` is 4 random bytes, so two workers cannot collide even
   if the lock ever failed to do its job.
4. Stream the response into the part file, feeding the same bytes to a SHA-256
   hasher. Enforce `filters.max_bytes` mid-stream: `Content-Length` is a hint and
   a lying or absent header must not be able to fill the disk. The moment the
   byte count exceeds the cap, delete the part file and fail with `too_large`.
5. Sniff the header before anything is published: the first bytes identify JPEG,
   PNG, WebP or HEIC, and the width and height come from the header. A file whose
   header cannot be read is not a file whirl will promise to display
   (features.md 2.2), so delete the part file and fail with `not_an_image`. This
   also produces the extension used in the final name, so a URL ending in `.php`
   cannot choose it.
6. Flush and close the part file. Compute the final path from the digest.
7. If the final path already exists, the bytes are identical by construction, so
   `unlink` the part file and report a cache **hit** (the daemon bumps
   `last_used`). Otherwise `rename(tmp/.../<name>.part, sha256/aa/bb/<name>.<ext>)`.
   `decision:` no `fsync` on this path: the file is a cache, a power failure may
   cost a re-download, and the size check against the index (section 6.3) catches
   a damaged copy. The publication guarantee we actually need is atomicity, not
   durability, and that is what `rename` gives: "The rename() system call
   guarantees that an instance of new will always exist, even if the system
   should crash in the middle of the operation" [L 1].
8. Hand the final path to the platform setter. Only now, with the setter's
   success, report the entry to the daemon on stdout (section 7.3).

Constraints this protocol inherits from the platforms, and why `tmp/` must be
inside the cache root:

- POSIX: `rename` requires the two paths to be on the same file system [L 1], and
  `tmp/` sitting next to `sha256/` makes that structurally true rather than a
  runtime check.
- POSIX: "a link named new shall remain visible to other threads throughout the
  renaming operation and refer either to the file referred to by new or old
  before the operation began" [10]. So a reader of the cache never sees a
  directory entry that exists and is empty.
- Windows: `MoveFileEx` only simulates a cross-volume move when
  `MOVEFILE_COPY_ALLOWED` is set [8], and whirl does not set it, so a
  cross-volume rename fails loudly instead of silently degrading to
  copy-then-delete, which is not atomic. Go's `os.Rename` is exactly
  `MoveFileEx(from, to, MOVEFILE_REPLACE_EXISTING)` [L 4], and the flags are
  mutually exclusive with what we want, so whirl's Windows rename is
  `os.Rename` and nothing more.
- Windows: `MOVEFILE_WRITE_THROUGH` is not set, so the rename is not durable
  across a power cut [8]. Same decision as the POSIX side, same reason.

Failure cleanup, exhaustively:

| Where it fails | What is left behind | What removed it |
|---|---|---|
| Response is a non-200 | nothing written yet | nothing to clean |
| `Content-Length` over cap, before the first byte | nothing | nothing to clean |
| Body exceeds the cap mid-stream | `tmp/<run>-<rand>.part` | this step deletes it, then fails |
| Header sniff fails | the same part file | this step deletes it, then fails |
| Disk full mid-write | the same part file | the delete fails too on a full disk, so the **sweep** removes it (section 5.5) |
| Worker killed (SIGKILL, OOM, crash) | the same part file | the sweep, after `cache.orphan_grace_seconds` |
| Setter fails after the rename | a valid cache file, no display | it stays a cache entry, and the worker reports `set_failed`; features.md 1.4 deletes the bad entry and tries one more candidate |
| Daemon dies after the rename | a valid cache file the index does not know | the startup sweep reclaims it as an orphan (section 5.5) |

`evidence:` the prototype's dry-run path was reported on the card as leaving
files behind. That did not reproduce from the current tree: `main.go:417` calls
`prune()` inside the dry-run branch. What the tree does contain is two exit paths
that skip pruning entirely, and they are the bug class this section is about:
`main.go:422-425` returns on a setter failure before `prune()` is reached, and
`main.go:385-393`, the `-set` path the daemon uses for `prev`, never prunes at
all. So the correct statement is "pruning is skipped on the failure and replay
paths", not "the dry-run path leaves files". The sweep in section 5.5 runs on
every rotation outcome and at every daemon start, which is the fix for the
failure path; `-set` no longer downloads at all, so it has nothing to prune.

## 4. Identity, and dedupe

### 4.1 The decision, with the alternatives priced

| Candidate identity | What it gets right | What it gets wrong | Verdict |
|---|---|---|---|
| Content hash (SHA-256 of the stored bytes) | Filename is identity; dedupe is a `stat`; the name verifies the bytes; a re-materialised file returns to a stable path, so pins survive | Costs a hash pass; two byte-identical images from different sources collapse, which is the point but must be a stated decision | **Chosen** as the primary identity |
| Source-scoped ID (Wallhaven `id`, Wikimedia pageid, local path) | Free: the source already returned it; a cheap pre-download filter | Names are not identity: the same image from two sources has two ids and lands twice; a `local` id built from a filename (as `main.go:246-247` does) changes under a rename | **Kept as a secondary key**, `origin_key`, for the pre-download window check only |
| Canonical URL | Free; human readable; it is what a re-download needs | Same URL can serve different bytes (re-encode, a `?cb=` query), and different URLs serve identical bytes (mirrors, thumbnails, a local copy of something downloaded earlier) | **Kept as the re-materialisation hint only**, field `origin`, never as identity |

`decision:` because the two secondary keys are kept, the pipeline has both a
cheap check and the correct one:

1. Before any download, drop a candidate whose `origin_key` appears in the recent
   window: the whole history ring (50 entries by default) plus the current index.
   This is O(50) over in-memory data and it stops the common case (a `random`
   query returning the same wallpaper) before it costs a request. features.md 2.5
   says 20; section 9 records why this document uses the ring instead.
2. After the download, drop a candidate whose digest already has a file in the
   cache. This catches what the first check cannot: a local copy of an image that
   was fetched earlier from a remote source, and two queries that overlap.

### 4.2 The cost, measured

`evidence:` hashing is not the reason to argue against content addressing. On
this machine, a 21 MB file takes 14.4 / 14.6 / 13.9 ms to read and hash, against
1.5 to 2.3 ms to read alone (`python3 hash_cost.py 20`, [L 5]). About 13 ms of
CPU per 20 MB, spent inside the download loop rather than as a second pass over
the finished file, in a process that is about to exit. The cheapest alternative
that is actually correct, comparing bytes against every existing file, is a
second read of both files and a partial read of the candidates: more I/O than the
hash it replaces.

### 4.3 Collisions and the honest caveat

`decision:` a SHA-256 collision is treated as out of scope, and the consequence
is stated rather than hidden: if two different images ever hashed the same, the
second download would be discarded as a cache hit and one candidate would be
silently lost. The mitigation is not worth its cost: verifying a hit by byte
comparison is a second read of two files per rotation to defend against an event
with no known construction.

`decision:` the same-origin-different-bytes case is not a dedupe failure, it is a
correct miss. A re-encoded response is a different set of pixels, so it gets a
different hash and a different cache file, and the old one ages out through the
normal LRU. Treating the URL as identity would have shown the user the old
image while the log claimed a new one.

`decision:` for a `local` source in `mode: reference` (features.md 2.2) there is
no cache file and no hash to compare. Its identity for the recent-window check is
the tuple `(source id, absolute path, size, mtime)`, which is cheap and gets the
one thing that matters here, "do not set that same file again this hour", at the
cost of a legitimate re-set after a touch. `copy` mode hashes like any other
bytes, because it is going to store them. The asymmetry is stated because it
means "dedupe by hash" is precisely "dedupe by hash for bytes whirl stores".

## 5. Eviction

### 5.1 Both caps, and why

`decision:` `cache.max_bytes` (default **2 GiB**) and `cache.max_files` (default
**500**). Whichever binds first evicts. The two bound different failures:

- Only a byte cap: 50,000 files of 20 KB each is 1 GB and a directory listing
  nobody can read, and the sweep's own per-entry cost turns into a per-rotation
  cost.
- Only a count cap, which is what the prototype ships (`keep: 40`): the disk
  total is unknown until the images arrive.

`evidence:` a count cap does not bound bytes, measured rather than asserted. The
rows below are single `sorting=random` draws of two pages of a real Wallhaven
search at the spec's own admission floor (`atleast=2560x1440`, `ratios=16x9`,
`purity=100`), so they are samples and not a reproducible distribution: the same
query returns a different 48 images every time, and this document reports each
draw for what it is. Three independent draws of 48 images each ([L 6], [L 8],
[L 9]) gave per-file minimums of 0.05 / 0.19 / 0.23 MB, medians of 2.25 / 2.11 /
2.59 MB, maxima of 20.34 / 16.03 / 14.02 MB, means of 3.83 / 3.23 / 2.90 MB, and
per-file spreads of 378x / 82x / 60x. The number that matters is the tightest
draw, not the widest: a count cap of 40 files spans 9.2 MB to 560.8 MB on draw 3,
7.8 MB to 641.3 MB on draw 2, and 2.2 MB to 813.6 MB on draw 1, so the
prototype's `keep: 40` carries a factor-of-60 uncertainty at best. At the three
sample means, 500 files is 1.9 / 1.6 / 1.45 GB, which is why the two defaults sit
where they do: they bind at roughly the same working set, so a user who accepts
the default sees one bound, not two.

`decision:` the caps are ordered against the admission cap that already exists in
features.md 2.5 (`filters.max_bytes`, default 40 MB): `whirl config check` fails
with a named key if `cache.max_bytes < filters.max_bytes`, because a config in
which no admissible image can fit inside the cache is a config with a guaranteed
permanent overshoot.

### 5.2 A single image larger than the cap

Three cases, and each has one answer:

1. Larger than `filters.max_bytes` (40 MB default): rejected before the download
   where the source gave a size, and mid-stream where it did not (section 3, step
   4). It never enters the cache, so the question does not arise.
2. Larger than `cache.max_bytes`: not reachable by a download, because 5.1
   requires `cache.max_bytes >= filters.max_bytes` (a config that breaks the
   ordering does not start, 8.7), and the admission filter already rejects
   anything above `filters.max_bytes`. It is reachable only by a file the user or
   another tool drops into the cache directory by hand. The sweep keeps it if it
   is protected (it cannot be evicted without violating INV-CACHE-2 when it is
   the anchor) and reports the overshoot; see 5.3.
3. Equal to the cap: it fits. It is admitted, and it evicts everything else,
   which is the correct reading of "the cap is the cap".

### 5.3 The protected set, and the display rule

`decision:` the sweep may not remove, under any circumstances:

- **the anchor**: the file named by `current.json.anchor` (section 6.1), which is
  what whirl believes the platform is displaying;
- **every pinned file**: every `favorites.json` entry with a materialised
  `cached_path` and digest;
- **every file being written right now**: anything under `tmp/`, and any cache
  file created within `cache.grace_seconds` (default 600) in case a second
  process, such as a hand-run worker, is between rename and report.

`decision:` while `favorites_degraded: 1` (6.4), the protected set is the whole
cache. The daemon has quarantined an unreadable `favorites.json`, so it cannot
know which files are pinned, and any file under `sha256/` could be the file
behind a pin. Nothing under `sha256/` is removed for as long as the state lasts;
`tmp/` is still cleaned, because a part file is never a pin. The cost is stated:
while degraded the cache can only grow, and the status line reports
`cache_over_reason: favorites_degraded` rather than a bound that is not being
enforced. That is the same trade 8.5 makes in the harder case, and 6.4 records
why the two cases land differently.

Protection is absolute, and it outranks the caps: the sweep removes unprotected
entries until both caps are satisfied or there are none left, and if the
protected set alone exceeds a cap it removes nothing further (the `pinned` cause
in 5.4) and reports the overshoot rather than hiding it:

```text
cache_bytes: 3221225472
cache_bytes_cap: 2147483648
cache_over_cap: 1073741824 bytes, 3 files
cache_over_reason: pinned
```

`decision:` the displayed image is never deleted out from under the platform,
and the reason is measured rather than theoretical. On macOS the store holds a
URL, and `[D 1]` shows what happens when the file behind that URL goes away: a
wallpaper write to a deleted path returns `NO` with `The file doesn't exist.` and
writes nothing, and the Spice side effect recorded in the same document is
exactly this failure in the wild, a store entry naming a file that "no longer
exists on disk (Spice pruned it)", which could not be written back. `[D 1]` also
records that `LastSet` and the live Space's node are what a readback returns, so
a broken entry is not merely cosmetic: it is the value the platform will apply at
the next login or Space switch.

`assumption:` what a *currently rendered* image does when its file disappears
mid-session is not answered for every platform, and the fallback here is the
strict one: assume the display can lose it.

- macOS: the store keeps the URL, and the live Space's node is what the platform
  reads back, so a missing file is a broken reference even while the current
  rendering survives [D 1]. That is the case this rule exists for.
- Linux: the answer is unknown, and the Linux research card lists "confirm
  behaviour when the file is deleted after being set" as an item that must be
  tested on real Linux before release, not as an answered question [D 2].
- Windows: `[unverified]`. The Windows research card does not cover what happens
  when the file behind an installed wallpaper is removed, and whirl does not rely
  on a platform copy it has not confirmed exists.

So the invariant below holds on every platform, and no platform is exempted on
the strength of an unverified assumption.

### 5.4 The invariants a reviewer can check

`decision:` these are the acceptance tests for the eviction policy. They are
stated against observable state, so they can be checked against a running
implementation, not against this document.

**INV-CACHE-1 (bound).** After every rotation completes, and at daemon start,
the unprotected cache is at or under both caps. Formally: let `U` be the cache
files minus the protected set (5.3). Then `sum(bytes of U) <= cache.max_bytes`
and `|U| <= cache.max_files`, unless `whirl status` reports
`cache_over_cap:` with a non-zero value, in which case the surplus is
attributable to one of exactly four causes, each of which the status line names:
a single file larger than `cache.max_bytes` (`cache_over_reason: single_file`),
the protected set itself (`cache_over_reason: pinned`), a sweep that failed
(`cache_over_reason: sweep_error`, with the error in the log), or a quarantined
`favorites.json` whose pin set the daemon cannot read, which makes the whole
cache protected (`cache_over_reason: favorites_degraded`, 5.3, 6.4). Those four
are exhaustive: a sweep that cannot run, or a pin set that cannot be read, is
reported as one of them rather than silently exempted.

Check: read `cache_dir:` from `whirl status`, then
`du -sk "$CACHE/sha256"` against `cache.max_bytes`, and
`find "$CACHE/sha256" -type f | wc -l` against `cache.max_files`, allowing the
reported overshoot.

**INV-CACHE-2 (display).** The file named by the anchor exists and is not
modified by whirl for as long as it is the anchor. This holds across cap
changes, cache clears (which must restore it, section 8.2), and rotations that
happen to pick the same image again.

Check, the adversarial form, on a config that `docs/architecture.md` 4.3 and the
validator both accept: `cache.max_files` at 2, the floor that section sets
(`docs/architecture.md:1489`) and the lowest value `validate_ordering` admits
(`crates/whirl-core/src/config.rs:1320`, whose refusal of 1 is asserted at
`crates/whirl-core/src/config.rs:2450`); the byte cap at its default, so
`cache.max_bytes >= filters.max_bytes` still holds; and `cache.grace_seconds` at
0, its legal minimum (the paragraph after the steps gives both reasons).

1. Run `whirl next` once. Record `anchor_path` and `anchor_digest` from
   `whirl status`, then pin that image with `whirl favorite`, so it is protected
   by two independent rules.
2. Run `whirl next` again, then pin this image too. The cache now holds two
   protected files against a count cap of 2, and the sweep runs at the end of
   that rotation.
3. Run `whirl next` once more. The cache now holds three protected files (the two
   pins and the new anchor) against a count cap of 2, and the sweep runs at the
   end of that rotation. Confirm that (a) the image on screen is the third one,
   (b) both files pinned in steps 1 and 2 are still present, still match their
   digests, and are still listed by `whirl favorites`, (c) the current
   `anchor_path` exists and still matches `anchor_digest`, and (d) `whirl status`
   reports `cache_over_cap` with `cache_over_reason: pinned` rather than
   pretending the cache is within budget.
4. Run `whirl next` once more. The file that is no longer the anchor (the third
   image) is unprotected now and must be evicted, because the sweep removes
   unprotected entries until the caps are satisfied or there are none left (5.3),
   while both pins and the new anchor survive.

`decision:` the check deliberately does not set `cache.max_bytes` to 1. 5.1 makes
that config invalid, the daemon refuses to start on a config that fails
`whirl config check` (8.7), and a check that depends on a config the document
declares illegal is not a check a reviewer can run. There is no test-only
override that lets an illegal config through, so every runnable check has to be
legal. The check also drives the cache through rotations and `whirl favorite`
rather than dropping a file into `cache/sha256/` by hand, because a hand-placed
file with no index entry is an orphan, and step 3 of the sweep removes it once it
is past `cache.grace_seconds`, for a reason that has nothing to do with the cap
being tested.

`decision:` `cache.max_files` is 2 here, and that is the floor's decision rather
than this check's: 4.3 requires `cache.max_files >= 2`
(`docs/architecture.md:1489`), `validate_ordering` refuses anything lower and
names the key (`crates/whirl-core/src/config.rs:1320`), a test asserts that
refusal (`crates/whirl-core/src/config.rs:2450`), and 8.7 turns it into a refusal
to start, which is exactly what a value of 1 produced on a real daemon:
`whirld: cache.max_files (line 3): <config>: 1 is less than 2`, exit 1. Nothing in
this document says what the floor is for; what 5.3 and 5.5 do imply is that the
file on screen and the file arriving in a rotation are protected at the same
time, so a cap of 1 describes a cache that cannot hold an image and its successor
together. 2 is the tightest value that admits both, and it is also the smallest
cap under which this check observes anything: the check's pressure is protected
files exceeding the cap, and a protected set of two does not exceed 2, which is
why step 2 pins a second image.

`decision:` the check also needs `cache.grace_seconds` at 0. 5.3 protects every
cache file created inside that window, and at the default of 600 s every file the
check creates is inside it, so step 4's eviction is not observable: the third
image is still protected after it stops being the anchor. 4.3's ordering rules do
not bound `cache.grace_seconds` (the list is `docs/architecture.md:1487-1490`, and
this key is not in it) and `want_u64` accepts 0
(`crates/whirl-core/src/config.rs:1503`), so 0 is legal and is what the check
uses; leaving the default in force would mean waiting out the window between
steps 3 and 4.

The check's last precondition is the build, and the piece this build is missing
is not the cache: the cache and the sweep exist (the sweep is
`crates/whirld/src/cache.rs`'s `pub fn sweep`, and its `pinned` outcome is
INV-CACHE-1's `cache_over_reason: pinned` in `whirl status`), while no source kind
does (`crates/whirl-worker/src/sources/mod.rs`'s `build()` answers `None` for
`local` and `wallhaven`). Every `whirl next` therefore ends `ERR no_candidates
stage=source code=no_candidates message=no implementation for kind local in this
build`, no rotation reaches the cache, and steps 1 to 4 above are unperformable
in this build until a source kind lands.

**INV-CACHE-3 (pins).** No pinned file is removed by the sweep, and every
favorites entry is either present with a matching digest, or re-materialisable
from its `origin`, or reported as unrecoverable. `whirl favorites` prints
`missing` next to an entry whose file is absent, so the state cannot be invisible.

Check: favourite something, evict everything else by lowering `cache.max_files`
(never below 2, the floor 4.3 sets) and leaving `cache.max_bytes` legal, restart,
and confirm the file survived and `whirl favorites` still lists it.

### 5.5 The sweep

`decision:` one sweep implementation, three trigger points, and it always runs
while holding the rotation lock (7.2), which is what makes it unable to race a
download.

Triggers: at daemon start (after the socket is bound and before the first
slot); at the end of every rotation, including failed ones; on a `reset` verb;
and never on a timer, because a timer is a resident thing to wake up for.

Steps:

1. Take the rotation lock, non-blocking. Held by someone else, record
   `sweep_deferred: 1` and return; the next rotation retries.
2. Re-read `index.json`.
3. Reconcile, in both directions: an index entry whose file is missing moves to
   `dangling`; a file under `sha256/` with no index entry is an orphan, to be
   removed unless it is inside `cache.grace_seconds` (a hand-run worker between
   rename and report is the only case).
4. Remove entries in `tmp/` older than `cache.orphan_grace_seconds` (default
   300), whatever they are, because a part file has no other owner.
5. While over either cap: take the unprotected entry with the oldest
   `last_used` and remove its file and its index entry. Stop when both caps are
   satisfied, or when no unprotected entries remain (the protected set alone
   exceeds a cap; 5.3), whichever comes first.
6. Write `index.json` (atomically, section 6.2) even if nothing changed, so that
   `seq` and `written_at` are honest about the last time the sweep ran.
7. Release the lock and log one line: `sweep files=312 bytes=180224512
   removed=4 reclaimed=1 orphans=0 deferred=0`.

`decision:` when `favorites_degraded: 1` (6.4), the sweep still runs on every one
of those triggers. An exemption for the degraded state would be exactly the
hidden hole this section exists to close, so the sweep runs and removes nothing
that could be a pin instead: step 4 still removes `tmp/` part files, because a
part file is never a pin; step 3 skips orphan removal for files under `sha256/`
(an orphan could be the file behind a pin whose record was never written); and
step 5 evicts nothing at all. It writes `index.json` as usual so `seq` and
`written_at` stay honest about when it last ran, and the resulting overshoot is
reported as `cache_over_reason: favorites_degraded` (5.4) rather than as a bound
the daemon is not enforcing.

Cost, stated: step 3 is the only part that can touch the whole cache, and the
index makes it `stat` per entry, not a read of any image. At the 500-file default
that is 500 `stat` calls on a rotation boundary.

## 6. State files

All three files are JSON, UTF-8, with a `schema` integer, a monotonic `seq`, and
a `written_at` in UTC. They are human-readable on purpose: a user with a bug
report can `cat` them, and the reviewer can check them, without the daemon.

### 6.1 `current.json`

```json
{
  "schema": 1,
  "seq": 41,
  "written_at": "2026-09-25T07:41:12Z",
  "written_by": "whirl/0.1.0 pid=1234 run=41",
  "paused": false,
  "rotation_count": 128,
  "next_at": "2026-09-25T08:11:12Z",
  "last_error": null,
  "anchor": {
    "digest": "ab12cd34...",
    "cached_path": "/Users/.../Caches/whirl/sha256/ab/12/ab12cd34....jpg",
    "set_at": "2026-09-25T07:41:12Z",
    "display_mode": "all",
    "displays": null
  },
  "cache": { "root_id": "8f1d0c2e-...", "files": 312, "bytes": 180224512,
             "over_cap_bytes": 0, "over_cap_files": 0 }
}
```

- `anchor` is the display record (5.3). `cached_path` is `null` for a
  `reference`-mode local image, in which case the file whirl must not delete is
  the user's own path and the answer is simply that whirl never deletes outside
  its own cache. `displays` is non-null only in `per-display` mode.
- `cache.root_id` is the cache generation (2.1). A mismatch means "the cache was
  cleared"; the entry then has no bytes and must be re-materialised before the
  next `prev`.
- `rotation_count` counts rotations and never decreases. Nothing else in the file
  is a counter.
- This file is derived: everything in it can be rebuilt from `history.json` plus
  the cache directory, with one loss (5.3's anchor becomes "the newest history
  entry's cached path", which is the same file in every normal case).

### 6.2 `history.json` and `favorites.json`

```json
{
  "schema": 1,
  "seq": 37,
  "written_at": "2026-09-25T07:41:12Z",
  "entries": [
    {
      "kind": "wallhaven",
      "origin_key": "wallhaven:ab12cd",
      "digest": "ab12cd34...",
      "cached_path": "/Users/.../sha256/ab/12/ab12cd34....jpg",
      "set_at": "2026-09-25T07:41:12Z",
      "via": "source"
    }
  ]
}
```

- `history.json`: newest first, exactly `state.history_entries` entries (default
  50, matching features.md F5's ring), the oldest dropped on write. `via` is
  `docs/architecture.md` 2.6's closed vocabulary, the same five values the `set:`
  record and `status.last_via` carry: `source`, `manual`, `prev`, `startup`,
  `recovered`. An image the user set by hand is `kind: external` with
  `via: startup` (features.md 1.5, architecture.md 1.7.3), not a `via` of its own:
  `kind` names the origin and `via` names the route, and one closed list on both
  the wire and the disk is what keeps a state file and a `history` response from
  disagreeing. `decision:` an earlier draft of this section named `rotate`, `set`
  and `external` instead. `rotate` and `set` are `source` and `manual`; the
  `external` spelling is `startup`. A reader accepts all three from a file an
  earlier build wrote, and this build writes none of them.
- The entry is the six fields a rotation produces or the daemon knows: `kind`,
  `origin_key`, `digest`, `cached_path`, `set_at`, `via`. Architecture 2.6's
  worker result is `<digest> <origin_key> <via> <path>`; `kind` comes from the
  source the daemon spawned and `set_at` from the moment it wrote the ring.
  `digest` and `cached_path` are `null` where there is nothing to record: an
  `external` image whose bytes could not be hashed, and a `reference`-mode set,
  where the file whirl must not delete is the user's own (6.1; 2.6's `-` on the
  wire).
- `decision:` the entry carries no `source`, `origin`, `width`, `height`, `bytes`
  or `mode`. The first five are `cache/index.json` fields (2.1): that is where
  the sweep, a `prev` re-materialisation and the favorites lookup read them, and
  a state file that repeated them would be a second copy free to disagree with
  the index. `mode` is not a record field at all: it is the source's own config
  key (`sources[].mode`, architecture 4.2), and a `reference`-mode set is visible
  as `cached_path: null`. The example above used to carry all six, which is a
  shape the worker cannot produce.
- `favorites.json`: `kind`, `origin_key`, `digest`, `cached_path` and
  `"added_at"`, and no `set_at` or `via`, which describe a rotation rather than a
  pin; unordered, one `schema` and one `seq`. `state` is not stored either:
  whether the bytes are there is a fact about the disk right now, so the reader
  recomputes it and the `favorites` record prints it, and a value written before
  the file was deleted is never believed. Pinned files are found by the digest,
  which is also the cache filename, which is why "unfavorite" is a single-set
  operation with no filesystem walk.

### 6.3 The write protocol, every state file, no exceptions

1. Serialise in memory.
2. Write to `<dir>/<name>.tmp-<pid>-<rand>` in the same directory (rename is
   same-filesystem only, [L 1], and the same directory makes it true by
   construction).
3. `fsync` the temp file [L 2].
4. `rename` the temp over the target.
5. Best effort, `fsync` the directory, ignoring `EINVAL`/`ENOTSUP` on macOS where
   directory `fsync` is not uniformly supported. Log the result at debug.
6. `decision:` durability ladder, stated because it is the sort of thing a
   reviewer should be allowed to cut: `fsync` on history, `F_FULLFSYNC` (macOS) or
   `fdatasync` (Linux) on favorites. Plain `fsync` is explicitly weaker than the
   user might assume: "if the drive loses power or the OS crashes, the
   application may find that only some or none of their data was written" [L 2].
   History is a 50-entry convenience and a re-download away; a favorite is the
   only record that cannot be reconstructed once the origin 404s.

`evidence:` steps 1 to 4 are not ceremony. With the same payload and the same
writer rate, an in-place rewrite was observed torn by 87.5% of concurrent reads
(3068 bad reads of 3508), while a temp-file-plus-`rename` rewrite was observed
torn by 0 of 971 [L 5]. The mechanism is that truncation and write are two steps,
so the file is empty or partial in between; `rename` has no such state, per
POSIX [10] and per the macOS guarantee that "an instance of new will always
exist" [L 1].

`decision:` this is also why the prototype's `fs::write(path, joined)` in
`whd.rs:104-108` is superseded outright rather than adjusted: `fs::write`
truncates and writes in place, which is precisely the measured-torn case, and a
reader is guaranteed to exist here because the CLI and the next daemon start both
read those files.

### 6.4 Corrupt, truncated, or future-schema files

`decision:` the same shape for all three files, with one escalation for
favorites.

1. Parse fails, or `schema` is not an integer, or the file is empty: quarantine.
   `rename` the file to `<name>.corrupt-<UTC timestamp>` in place; never delete
   it, and keep only the newest quarantine per file (older ones are removed when
   a new one is created, so this cannot grow without bound).
2. Report it: one log line, and a sticky status key
   (`state_corrupt: history.json`, `state_quarantined: <path>`), because a state
   file that vanished silently is the failure mode that makes users distrust the
   tool.
3. Rebuild, per file:
   - `current.json`: rebuild from `history.json`'s newest entry and a fresh
     anchor; the cache is untouched. Cheap, always possible.
   - `history.json`: start with an empty ring. The cache is untouched. The user
     loses `prev` history, which is the honest cost, and `whirl status` says
     `history_lost: 1` until the next clean write.
   - `favorites.json`: **do not start empty.** A corrupt favorites file is the
     one case where the safe move is to stop writing: the daemon loads an empty
     pin set, marks `favorites_degraded: 1`, refuses `whirl favorite` /
     `unfavorite` with the quarantine path in the message, and treats the whole
     cache as protected for as long as the state lasts. No sweep removes anything
     under `sha256/` while `favorites_degraded: 1`, `whirl status` reports
     `cache_over_reason: favorites_degraded` rather than a bound it is not
     enforcing, and only `tmp/` is still cleaned (5.3, 5.5). The user resolves it
     with `whirl reset --favorites` (which is explicit) or by inspecting the
     quarantine.
     `decision:` this is a degraded mode and not a refusal to start, and it
     differs from 8.5 deliberately. 8.5 is the state directory that cannot be
     written at all: no durable record of anything is possible, so the daemon
     exits. Here the directory works and exactly one file is unreadable, the user
     can repair it in place while the daemon runs, and the cost of continuing is
     bounded and visible: the cache grows until the file is resolved, no file is
     guessed away, and the status line is the only thing that has to be believed.
     A refusal to start would buy the same guarantee 8.5 buys (a daemon never runs
     with a state it does not understand) at the price of an outage that a single
     explicit verb can fix, which is not a trade worth making here.
4. `schema` greater than the daemon's: a downgrade, not corruption. Quarantine is
   wrong here because the file is presumably fine and a newer whirl wrote it, so
   the file is left exactly as it is, that file becomes read-only for this
   daemon, and `whirl status` reports `state_schema_newer: 1`. The key is typed
   `0|1` (architecture.md 2.10) because it is a status key a client parses rather
   than a message, and the file name and both numbers go to the log, which is
   where 6.4 step 2 puts the detail: `favorites.json was written by schema 2 and
   this build understands 1`. Overwriting it would be destructive and silent;
   refusing is loud and reversible.

### 6.5 Resetting without losing the cache

`decision:` reset is a first-class pair of verbs rather than advice, because R1
makes it trivial and a user should not have to remember a path.

`decision:` **`whirl reset` is not in the v0.1 surface, and the table below is
the design for a verb no v0.1 build has.** `docs/spec/features.md` 1.1 ships a
closed verb set ("v0.1 ships this verb set and no more") and holds no `reset`;
section 9 of this document adds no verb to it ("no new verb is needed and none is
added"); and `docs/architecture.md` 2.5 and 2.5.1, the protocol grammar and the
CLI verb mapping, close over the same set. The CLI answers `whirl reset` with its
unknown-command error and the protocol has no request for it, so until the verb
lands the raw form below is the whole of what a user can do, and the mentions of
it in 1.4, 6.4 and 8.2 read as this design rather than as a command that exists.

| Verb | Effect | Cache | History | Favorites |
|---|---|---|---|---|
| `whirl reset --state` | Quarantine-then-replace the three state files with fresh empty ones | untouched | cleared | cleared |
| `whirl reset --favorites` | Clears favorites and their pins, nothing else | untouched | kept | cleared |
| `whirl reset --cache` | Empties the cache, keeps every state file, then restores the display from the anchor's origin (8.2) | cleared | kept | kept |
| `whirl reset --all` | Both, state after cache | cleared | cleared | cleared |

The raw form works too, and it is a consequence of R1 rather than a happy
accident: `rm -rf <state dir>` keeps the cache because the cache is a different
directory on every platform, and `rm -rf <cache dir>` keeps history and
favorites for the same reason. `whirl status` prints `state_dir:` and
`cache_dir:`, so the user does not have to know the platform's convention to do
it. The one caveat is the display caveat from 1.4.

## 7. Concurrency

### 7.1 The rule

> Every file has exactly one writing process. The daemon owns the state
> directory and the cache index. A worker owns only the cache files it creates,
> while holding the rotation lock. No reader takes a lock, and every reader must
> tolerate any file being replaced under it.

Why the rule is this shape and not "both processes lock the state file": the
worker produces one fact per rotation (which file it set), and a lock only
serialises the write, it does not decide who is right. Routing that fact through
the daemon, which is already the only process that knows the schedule, the ring
size and the pin set, removes the class of bug entirely. The cost is stated: if
the daemon dies mid-rotation, the worker's completed download is not recorded and
becomes an orphan, and the next rotation downloads again. One image, bounded,
and it only happens when a daemon dies.

### 7.2 Ownership table, and the locks

`Writer` names the one process that writes the file. `Readers` names the
processes that open it: a `whirl` verb that shows a file's contents is not one of
them, because it asks the daemon, which is already named on the row, and `whirl
status` reports `cache/index.json`'s `root_id` (2.1) from the daemon that opened
it. `Surface` names the `whirl` verbs a user runs to observe or change the file,
or `none` where no verb does, which leaves the filesystem as the only way in.
Every surface below is answered by the daemon, because the daemon is the process
that opens the file.

| Path | Writer | Readers | Surface |
|---|---|---|---|
| `config.json` | the user, in an editor | daemon, at start and on reload; the worker, once per run; the CLI, for the socket path | `whirl config path`, `whirl config check`, `whirl sources` |
| `state/current.json` | daemon | daemon; humans | `whirl status`; `whirl pause`, `whirl resume` |
| `state/history.json` | daemon | daemon; the worker, for the recent window of 4.1 | `whirl history`; `whirl prev` |
| `state/favorites.json` | daemon | daemon | `whirl favorites`; `whirl favorite`, `whirl unfavorite` |
| `log` | daemon | humans | none |
| `state/locks/daemon.lock` | daemon (held for its lifetime) | a second daemon, at startup | `whirl status` (`lock_mode`) |
| `state/locks/rotate.lock` | a worker, for the run; the daemon, for a sweep | both | `whirl status` (`sweep_deferred`) |
| `cache/index.json` | daemon | daemon; the worker, for the recent window of 4.1 | `whirl status` (`cache_root_id`) |
| `cache/sha256/**` | the worker run that created it | the setter call in that run; the daemon, `stat` only | `whirl status` (`cache_files`, `cache_bytes`) |
| `cache/tmp/**` | the worker run that created it | nobody | none |

- `daemon.lock` is taken with `flock(LOCK_EX|LOCK_NB)` on POSIX [L 3] and
  `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK|LOCKFILE_FAIL_IMMEDIATELY)` on Windows [9],
  or with the `excl_file` fallback of 8.7 where that call answers `ENOTSUP`.
  A second daemon exits with a message naming the first daemon's pid, rather than
  unlinking a live socket, which is what `whd.rs:456` does today with an
  unconditional `remove_file(&cfg.socket)`; a lock file left behind by a daemon
  that is gone is the second kind of refusal, classified and named in the same
  message under 8.8.
- `rotate.lock` is the same primitive, and it is what makes a sweep unable to run
  concurrently with a download (5.5) and two rotations unable to run at once.
  `decision:` the daemon holds it only for the sweep, not for the whole rotation,
  because the worker's own lifetime already serialises downloads and holding it
  across the worker would deadlock the two roles.
- Both locks are advisory. Where the lock is the OS primitive, the operating
  system releases it when the holding process exits, including `SIGKILL`: a lock
  belongs to an open file description, and termination closes the descriptors, so
  the kernel drops it without whirl doing anything [L 3] [12]. That release is
  what preferring the primitive buys. The `excl_file` fallback of 8.7 does not
  have it: that lock is a file whose existence is the lock, nothing removes the
  file when the holder is killed, and its record of the holder is a pid file that
  can go out of date. 8.8 is the rule for a lock file left behind, and
  `lock_mode` in `status` (2.10) is how a running daemon says which of the two
  it holds.
- `decision:` the socket is unlinked only after a successful connection attempt
  fails, so a running daemon's socket is never removed by a starting one.
- `decision:` the worker never opens the log file. It writes its result and its
  diagnostics to stdout and stderr, and the daemon, which is its parent and owns
  the pipes, appends them. Two processes appending to one log is a second writer
  for a file that does not need one, and this is free because the daemon already
  captures the worker's output to parse the result line.

### 7.3 The rotation handoff, in order

1. The daemon decides a slot is due. It holds nothing, and it does not queue: if
   a rotation is already in flight (`rotating` is set), the slot is skipped and
   the next one is armed. A queue behind a slow download is how a machine ends up
   doing six rotations at once after a sleep.
2. The daemon spawns the worker. **The lock has exactly one holder at a time and
   it is the worker**: the worker takes `rotate.lock` exclusively for its own
   lifetime, and the daemon takes it only for a sweep. A second rotation that
   arrives while the first is running therefore does not wait behind a queue: its
   worker fails the non-blocking lock and exits `busy`, which is a fact the user
   can see, instead of a second download nobody asked for.
3. The worker runs: enumerates, filters, downloads, renames, sets, exits with one
   result line on stdout.
4. The daemon takes `rotate.lock` (the worker has exited, so this cannot block;
   where the `excl_file` fallback of 8.7 is in force, the daemon removes the
   reaped worker's lock file here, under 8.8), validates that the reported path
   is inside the cache root and exists, writes
   `current.json`, `history.json` and the index entry, runs the sweep, and
   releases the lock.
5. The daemon notifies `idle` subscribers once, and only once. The prototype's
   `whd.rs` wakes twice per rotation (worker start and finish) and the spike
   README calls the duplicate notification cosmetic; with R2 in force there is
   one state transition, so there is one notification, and the spike's wart does
   not need to be inherited.

Readers and the CLI: the CLI is a protocol client only (features.md F3), so no
verb writes a file. That is the reason the protocol needs no file locking on the
client side, and the reason `whirl prev` can be implemented as "ask the daemon to
set an entry the daemon already has".

## 8. Failure and recovery

Every case below names what changes on disk, what the user sees, and what does not
happen.

### 8.1 Disk full

- During a download: the write fails, the part file is removed if the remove
  itself succeeds (on a genuinely full disk it may not), the worker exits
  non-zero with `last_error: enospc`, and the displayed image is untouched.
- During a state write: the temp file write fails, the temp file is removed, the
  target file is **not** touched, and the state written remains the previous
  version. This is the property the temp-then-rename protocol buys (6.3), and it
  is the specific thing the prototype's `fs::write` does not have.
- `decision:` whirl does not wipe the cache to make room. features.md 1.4 already
  rules out the automatic wipe, and the reason is worth repeating: evicting a
  user's pins to save space is a surprise, and the honest signal is the status
  line and `whirl config check`.
- `decision:` whirl does not raise its own caps on ENOSPC either. A cache that is
  allowed to grow when the disk says no is not a cache.
- A sweep on a full disk: deletions still work (they free space), index writes
  may not, so the sweep reports `cache_over_reason: sweep_error` and tries again
  next rotation.

### 8.2 Cache cleared, by whirl or by hand

- `whirl reset --cache` empties the cache and then re-materialises the anchor:
  re-download from `anchor.origin` (or re-reference the local path), set it, and
  write a fresh entry. If the origin is unreachable, the reset reports
  `cache_cleared: 1, anchor_not_restored: 1` and changes nothing about the
  current rendering. That asymmetry is deliberate: a reset must not be able to
  blank a desktop in order to satisfy a bookkeeping rule.
- Cleared by hand (`rm -rf`): whirl notices at the next start, from
  `index.json`'s absence or from a `root_id` mismatch (2.1). Every history entry
  keeps its `origin`, so every entry is still re-materialisable, and `prev`
  re-downloads rather than failing.
- `assumption:` whether the *currently rendered* image survives a hand-deletion
  is platform-specific and partly unanswered (5.3). whirl cannot repair what it
  is not told about, and a watcher is out of scope by design (features.md 1.3),
  so the repair happens at the next use.

### 8.3 A cache file deleted by the user or another tool

| Which file | What whirl does |
|---|---|
| The anchor's file | Nothing immediately: no watcher. At the next use (`prev`, `set <id>`, a startup re-apply) it re-materialises from `origin`, and if the origin is also gone it marks the entry `unrecoverable` and reports it, rather than re-setting a path that does not exist (which is the failure `[D 1]` measured). |
| A pinned file | Same repair path, and `whirl favorites` shows `missing` until the repair runs. This is features.md 1.2's design, now with a mechanism: the pin is a digest, so the re-materialised file returns to the same path and re-satisfies the pin. |
| Any other cache file | The index entry becomes dangling; the next sweep removes the entry and logs `cache_reclaimed`. Nothing else happens, and this is not an error, because a cache being partly gone is a normal state for a cache. |
| The whole cache directory | Section 8.2. |

### 8.4 Read-only cache directory

- Detected at start and at each rotation, not cached: attempt to create and remove
  `tmp/<run>-probe.part`. `whirl status` reports `cache_writable: 0` and
  `cache_dir: <path>` so the reason is visible.
- A rotation that needs to write (any remote source, or a local `copy` source)
  fails with `last_error: cache_readonly`, and the displayed image is untouched.
- A rotation from a local source in `reference` mode still succeeds, because it
  writes no bytes: it sets the user's own file and records a history entry with
  `cached_path: null`. A read-only cache degrades whirl rather than stopping it,
  which is the correct behaviour for someone who deliberately mounted it that
  way.
- Index updates become best-effort, and their failure is logged once, not per
  rotation.

### 8.5 State directory unwritable

- The daemon refuses to start rather than running without state: without
  `favorites.json` it cannot honour INV-CACHE-3, and a daemon that silently
  forgets pins is worse than one that does not start. Exit message names the
  directory and the `errno`, and says which of the two roots is the problem
  (state, not cache, is the distinction the prototype's single `state_dir`
  cannot make). This is the whole directory, not one file: a single unreadable
  state file is the degraded case in 6.4, which carries on with the loss named in
  `status` instead of refusing to run.

### 8.6 A clock jump

- `decision:` durations use a monotonic clock (mach time on macOS,
  `CLOCK_MONOTONIC` on Linux, `QueryPerformanceCounter` on Windows); wall clock
  is used for two things only: human-readable timestamps, and the persisted
  `next_at` deadline.
- Scheduling is therefore `next_in = clamp(wall(next_at) - wall(now), 0, interval)`
  and never `wall(now) >= wall(next_at)` composed into a sleep, which is what
  `whd.rs:189` and `whd.rs:195` do today. The failure is asymmetric and worth
  naming: a backward jump makes the difference large and positive, so rotations
  stall silently for the length of the jump and nothing in the log says why,
  while a forward jump produces one immediate rotation, which is fine.
- A jump is detected by comparing the wall clock between two consecutive
  scheduler observations against the monotonic elapsed time. Beyond
  `2 * interval`, whirl logs one line, sets `clock_jump: <n>` in `status`, and
  re-anchors `next_at` from the monotonic clock plus `interval`.
- Missed slots are not replayed. Bounded sleep slices and one rotation per wake
  are already in the prototype (`whd.rs:180-204`) and are the right behaviour;
  this document only adds that they must be driven by a monotonic clock so that
  the "one rotation" is guaranteed and not a wall-clock accident.
- Nothing in INV-CACHE-1 to INV-CACHE-3 depends on a timestamp. A machine with a
  clock set to 1970 evicts by `last_used` ordering, which is still well defined,
  and still respects the caps.

### 8.7 Other cases, briefly

- `ENOSPC` while writing the log: drop the log line, keep serving. The log is not
  allowed to be a failure path for the product.
- Cache root on a filesystem that does not support `flock`: the lock file's
  `flock` returns `ENOTSUP` [L 3]. Fall back to an `O_CREAT|O_EXCL` lock file
  whose existence is the lock and whose contents name the holder: its pid, and
  the platform's own start time for that pid, which is the pair 8.8 needs to tell
  a live holder from a recycled pid. Report `lock_mode: excl_file` in `status` so
  the weaker guarantee is visible rather than assumed. `decision:` the fallback is
  a property of the filesystem and not of one lock, so it covers both locks of
  7.2, and 8.8 is the rule for either of them found left behind by a holder that
  is gone.
- `config.json` missing: written with defaults and fully commented, as
  features.md F1 requires. `config.json` unparseable: the daemon does not start,
  and the message names the byte offset, because a config error is the one error
  a user must fix by hand.
- `config.json` parses but fails `whirl config check`: a value out of range, or an
  ordering rule such as `cache.max_bytes < filters.max_bytes` (5.1). The daemon
  does not start either, and the message names the failing key and both values.
  `config check` and the daemon's start-up validation are one implementation, so
  a config the tool rejects on demand is a config it refuses to run: there is no
  test-only override that admits an illegal config, which is what lets the checks
  in 5.4 be written on legal configs only.
- A second `whirl next` while a rotation is in flight: refused with `busy`, not
  queued (7.3).

### 8.8 A lock file whose holder is gone

`decision:` the rule is refusal, not recovery: a lock file left behind by a holder
that is gone is reported to the operator rather than taken over. The daemon never
unlinks, renames, truncates or overwrites a lock file in order to become the
holder of it, with one exception it holds proof for, the rotation lock of a
worker it has just reaped (`rotate.lock` below). This case belongs to the
`excl_file` fallback of 8.7 alone: under `flock` and `LockFileEx` the kernel
releases the lock when the holder exits and there is nothing to judge (7.2).

Why the file is not reclaimed. Taking it over needs a compare-and-swap on the
path, and the fallback is in force exactly where the filesystem does not provide
the primitive that would give one. Two daemons starting in the same instant, each
having read a dead holder and each having replaced the file, both run: two
writers of `current.json`, which is the one failure 7.1 and R5 are written to
prevent. So the cost of refusing is stated rather than hidden: on such a
filesystem a `SIGKILL`ed or crashed daemon needs one file removed by hand before
whirl starts again, and `lock_mode: excl_file` in `status` (2.10, 8.7) is how the
operator learns the weaker lock was in use when that happened.

What the daemon judges, and what a pid is worth. The record in the lock file is
the holder's pid and the platform's own start time for that pid (8.7), because a
pid alone is not an identity: pids are reused, so a recorded pid that answers as
alive may be an unrelated process. The pair is what makes the message true, and
the message is the whole output of this case:

| Recorded holder | Message names it as |
|---|---|
| pid live, start time matches the record | the holder: `held by pid 4711, started <t>` |
| pid live, start time differs from the record | a recycled pid: `left by pid 4711 started <t>; pid 4711 is now a different process` |
| pid gone | a holder that exited: `left by pid 4711, started <t>, which is not running` |
| liveness or start time unreadable | unjudged: `left by pid 4711, start time unreadable` |

None of the four takes the file. Liveness and start time, per platform:

- macOS and Linux: `kill(pid, 0)`, which the manual page names as the idiom:
  "A value of 0, however, will cause error checking to be performed (with no
  signal being sent). This can be used to check the validity of pid" [L 10]. So `0`
  is alive, `ESRCH` is gone, and `EPERM` is alive and not ours, which is still
  alive. Start time: macOS `sysctl(KERN_PROC_PID)` and `kp_proc.p_starttime`
  [L 10], Linux `/proc/<pid>/stat` field 22, "The time the process started after
  system boot", in clock ticks [13].
- Windows: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` for the pid, and
  `GetProcessTimes` for `lpCreationTime`, "a FILETIME structure that receives the
  creation time of the process" [14] [15]. No Windows host was available here, so
  this column is documented API behaviour rather than something measured on this
  machine (architecture.md 3.7 lists it as unverified [D 2]). A failed open is
  read as gone only where the API documents the pid as nonexistent or unopenable,
  and as unjudged for `ERROR_ACCESS_DENIED`, which the same page documents for the
  System and CSRSS processes [14].
- The judgement is not load-bearing: it changes the sentence, never the action,
  because all four rows refuse. That is deliberate, and it is what makes pid reuse
  a reporting problem rather than a correctness problem.

`rotate.lock` under the fallback. The fallback is a property of the filesystem,
so it applies to `rotate.lock` too (8.7), and the same rule applies to it with
one exception the parent is entitled to make: the daemon spawned the worker, and
once it has reaped it (1.7.1, 1.7.2, 7.3 step 4) that worker is provably gone, so
the daemon removes the lock file here when the record in it names the worker it
has just reaped. No liveness probe and no pid-reuse question arise, because only
the parent holds the exit status. A `rotate.lock` naming any other holder,
including the hand-run worker of 5.5 step 3, is not removed: the sweep does not
steal it and defers exactly as 5.5 step 1 and `sweep_deferred` (2.10) already
describe, with a holder that has since exited. That is the one place this section
reads `sweep_deferred`'s gloss ("already held by someone else") a shade wider
than its words, and it is named here rather than left for a reader to notice.

What the daemon reports, which is a message and not a key. The refusal happens at
1.5 step 1, before the control socket is bound, so there is no `status` to carry
a key and no third value for `lock_mode` (2.10). The message goes to stderr and to
the log, names the lock path, the classification above, and the one action that
clears it, for example:

    whirl: state/locks/daemon.lock left by pid 4711, started 2026-09-25T07:41:12Z,
    which is not running; remove /Users/you/.local/state/whirl/locks/daemon.lock
    and the supervisor will try again

Exit is 1, which the frontend contract already fixes as "the daemon refused"
(architecture.md section 8 item 7), and the supervisor keeps trying on its own
cadence: `KeepAlive` at 10.1 s on macOS, `Restart=always` with `RestartSec=5` on
Linux, `RestartOnFailure` on Windows (architecture.md section 5). While the
refusal stands every `whirl` verb exits 2 with the unreachable-daemon message
(architecture.md failure mode 1), which is all the CLI can say: it does not read
the lock file and does not start the daemon (2.5.1).

## 9. Where this supersedes the prototype, and where it touches features.md

| Superseded | Was | Now | Why |
|---|---|---|---|
| Cache location | `~/Pictures/Wallhaven` (`main.go:55`) | `~/Library/Caches/whirl` and the platform equivalents (1.1) | A user's Pictures folder is user data; nothing whirled owns may live there, and a cache in a synced or hand-curated folder is a cache that will be curated by hand. |
| Config location | `~/.config/wh-rotate/config.json` on every platform (`main.go:67`) | Per platform (1.1) | macOS and Windows have conventions; following them is what makes the paths predictable to a user and to a backup tool. |
| State location | `~/.local/state/wh` (`whd.rs:434`) | Per platform (1.1) | Same, and Apple's `Application Support` is the documented home for "app-specific data and support files". |
| Cache naming | `wh-<candidate id>.<ext>`, extension from the URL (`main.go:278-282`) | `sha256/aa/bb/<digest>.<ext>`, extension from the sniffed header (2) | Identity, dedupe and integrity in one field. A URL ending in `.php` can no longer choose the extension. |
| Eviction | Count only, `keep: 40`, by file mtime, `wh-` prefix (`main.go:324-353`) | Both caps, LRU by index `last_used`, protected set (5) | Count does not bound bytes (5.1); mtime is not bumped on a cache hit; and `prune` has no idea which file is on screen or favourited, so it will delete the displayed image, which is the failure `[D 1]` caught Spice committing. |
| Favorites | Paths in `favorites`, no pin link to `prune` (`whd.rs:104-108`) | Records with a digest, pins in the index and in state, sweep skips them (5.3) | A favorite that the evictor can delete is not a favorite. |
| State writes | `fs::write` in place (`whd.rs:104-108`) | temp, fsync, rename (6.3) | Measured: 3068 of 3508 concurrent reads saw a truncated file [L 5]. |
| Schema | Newline-separated paths | JSON with `schema` and `seq` (6) | A path list cannot express origin, digest or time, and cannot be validated before it is believed. |
| Who writes the cache index | features.md 2.5: "the worker computes a content hash ... and keeps an index of hashes in the cache" | The **daemon** writes `index.json`; the worker reports the digest and the origin on stdout and writes nothing but its own cache files (R2, 7) | Two writers on one file need a lock and a merge rule, and the merge rule would have to decide whose view of `pinned` is right. One writer removes the question. The worker still computes the hash, in the same pass as the download. |
| Recent-window size | features.md 2.5: "the last 20 entries in history" | The whole history ring, `dedupe.recent_entries`, default = `state.history_entries` (50) (4.1) | The ring is already a bound; a second, smaller number is a second thing that can disagree with it. The cost of the larger window is fewer repeats of an image the user liked, which is a preference, not a correctness property. |

`decision:` two keys that features.md names are kept as aliases rather than
removed, so a config written against that document keeps working:

- `keep` is accepted as an alias for `cache.max_files`, with a deprecation
  warning naming the new key. It is not silently reinterpreted as bytes.
- `cache_dir` (the prototype's key) is accepted as `cache.root` when it is an
  absolute path, with a warning, because overriding the cache location is a
  legitimate thing to want in a test.

`decision:` the following keys are added by this document, and
`whirl config check` validates each with a named error:
`cache.root`, `cache.max_bytes`, `cache.max_files`, `cache.grace_seconds`,
`cache.orphan_grace_seconds`, `state.history_entries`, `dedupe.recent_entries`.

`decision:` `whirl status` gains the keys this document needs a reviewer to be
able to read: `state_dir`, `cache_dir`, `cache_files`, `cache_bytes`,
`cache_bytes_cap`, `cache_files_cap` (alias `keep`), `cache_over_cap`,
`cache_over_reason`, `cache_writable`, `cache_root_id`, `sweep_deferred`,
`state_corrupt`, `state_quarantined`, `state_schema_newer`, `history_lost`,
`favorites_degraded`, `clock_jump`, `lock_mode`, `anchor_digest`,
`anchor_path`. features.md F10 fixes the *stability* of status keys, not the set,
and every key here is a fact the invariants are checked against.

`decision:` no new verb is needed and none is added. features.md 1.1 ships a
closed verb set, and the invariants above are checkable with `whirl status` plus
`du` and `find`. A `whirl cache verify` verb (re-hash every cache file against
its name) is deliberately deferred: it is the only thing that would want a full
read of the cache, it is a diagnostic rather than a feature, and v0.1 has no verb
budget for it.

## 10. Acceptance check

Against this card's criteria:

- **Every path and format is unambiguous enough to implement without a follow-up
  question.** Sections 1 (a row per file, per platform, with the default value of
  each XDG variable and of each known folder), 2.1 (index schema, field by field),
  6.1 and 6.2 (state schemas with every field), 6.3 (the write protocol, step by
  step), 9 (every config key this document adds, with its default).
- **The eviction rule is a testable invariant that protects the displayed
  image.** INV-CACHE-1 states the bound with its four permitted exceptions and
  names the status keys that carry them; INV-CACHE-2 is stated in the adversarial
  form on a config that is legal under 5.1 (`cache.max_files` at 2, the floor
  `docs/architecture.md` 4.3 sets, byte cap at its default, `cache.grace_seconds`
  at 0) so a reviewer can attempt to break it in three steps without
  needing a config the document refuses to run; section 5.3 is the protection
  rule; section 5.5 is the one sweep that
  implements it. The "displayed image is never deleted out from under the
  platform" requirement is 5.3, and it is grounded in a measurement of the
  platform rather than in a belief: `[D 1]`'s deleted-path write failure and its
  Spice side effect.
- **Corruption and partial-write behaviour is specified for every state file.**
  Section 6.4 covers parse failure, truncation, empty file, and a newer `schema`,
  per file, including the one case (favorites) where the answer is to stop
  writing. Section 6.3 covers the partial-write case structurally: a reader can
  see only the old file or the new one.
- **The concurrency rule names which process owns which file.** Section 7.1 is
  the rule in one sentence; 7.2 is the per-file table with the writer and the
  readers; 7.3 is the handoff order; 8.8 is what happens to a lock file whose
  holder is gone, which is the one case where the OS primitive of 7.2 cannot
  answer for itself.
- **Citations for platform conventions.** Apple's File System Programming Guide
  for `Application Support`, `Caches` and `Logs` [1][2]; the XDG Base Directory
  Specification for config, state, cache and runtime [3]; Microsoft's
  `KNOWNFOLDERID`, Known Folders and CSIDL pages for `%APPDATA%`,
  `%LOCALAPPDATA%` and the "use `SHGetKnownFolderPath`" rule [4][5][6]; the pipe
  name form [7]; `MoveFileEx` flags [8] and `LockFileEx` [9] for the two Windows
  primitives; POSIX `rename` [10] and the macOS manual pages [L 1][L 2][L 3] for
  the atomicity and durability claims underneath section 3 and 6.3; the Linux
  `flock` page [12] for the release-on-close claim that 8.8 rests on, and the
  Linux and Windows process-identity pages [13][14][15] for its liveness rule.
- **Specification only.** No production code, no `prototype/` edits, no
  credentials. The only files added are this document and the probe scripts and
  their README under `docs/spec/probes/`, which exist to produce `[L 5]`, `[L 6]`
  and `[L 8]` and are not part of the product.
- **The review gate.** This document is completed with `kanban_request_review`,
  not `kanban_complete`, per the orchestrator's note on the card.

## 11. Evidence

Local artifacts. `[L n]` is cited inline above.

| # | What was run | Observed |
|---|---|---|
| [L 1] | `man 2 rename` | "If new exists, it is first removed. Both old and new must be ... on the same file system." and "The rename() system call guarantees that an instance of new will always exist, even if the system should crash in the middle of the operation." |
| [L 2] | `man 2 fsync` | "Note that while fsync() will flush all data from the host to the drive ..., the drive itself may not physically write the data to the platters for quite some time". F_FULLFSYNC named as the stricter variant. |
| [L 3] | `man 2 flock` | Exclusive and shared locks, `LOCK_NB` returns `EWOULDBLOCK` when held, `ENOTSUP` for an unsupported file type, locks are on files rather than descriptors. The release-on-close sentence is not on this page; [12] states it, and 8.8 relies on [12] for it. |
| [L 4] | `grep -n -A12 "func rename" $(go env GOROOT)/src/os/file_windows.go`; `internal/syscall/windows/syscall_windows.go:357-367` | Go 1.26.1 on this machine: `os.Rename` on Windows is `windows.Rename`, which is `MoveFileEx(from, to, MOVEFILE_REPLACE_EXISTING)`. `MOVEFILE_COPY_ALLOWED` is not passed. |
| [L 5] | `python3 docs/spec/probes/atomic_write_probe.py 400` (four runs), `python3 docs/spec/probes/hash_cost.py 20`, `python3 docs/spec/probes/index_size.py 500` | In-place rewrite: 3068 bad reads of 3508, 3054 of 3496, 3832 of 4270, 2387 of 2830, so 87.4%, 87.4%, 89.7% and 84.3% of concurrent reads saw a truncated or unparsable file. Temp-plus-`rename`: 0 bad reads of 971, 981, 949, 983. Hashing a 21.0 MB file: 14.4, 14.6 and 13.9 ms against 2.3, 1.6 and 1.5 ms for the read alone. A 500-entry `index.json` matching the section 2.1 schema: 170607 bytes compact, 220135 bytes indented, 341 bytes per entry. |
| [L 6] | `curl -A "whirl-spec-probe" "https://wallhaven.cc/api/v1/search?sorting=random&purity=100&ratios=16x9&atleast=2560x1440&page={1,2}"` then `python3 docs/spec/probes/size_stats.py wallhaven-p1.json wallhaven-p2.json` | Draw 1 of 3, 2026-09-25: 48 images, `file_size` 0.05 MB to 20.34 MB, median 2.25 MB, mean 3.83 MB, per-file spread 378x. A count cap of 40 spans 2.2 MB to 813.6 MB. The endpoint is the one features.md 2.3 specifies; no key needed for `purity=100` [11]. `sorting=random` returns a different 48 images on every request, so this row is one sample and not a reproducible distribution. |
| [L 7] | `sw_vers; uname -m; python3 --version` | macOS 26.5.2 (25F84), arm64, Python 3.14.7, Go 1.26.1, rustc 1.94.0. Every local measurement above was taken on this machine. |
| [L 8] | `curl -sS -A "whirl-spec-probe" "https://wallhaven.cc/api/v1/search?sorting=random&purity=100&ratios=16x9&atleast=2560x1440&page={1,2}"` into `/tmp/whirl-l6b/p1.json` and `p2.json`, then `python3 docs/spec/probes/size_stats.py /tmp/whirl-l6b/p1.json /tmp/whirl-l6b/p2.json` | Draw 2 of 3, re-run on this machine while applying this review round: 48 images, 0.19 MB to 16.03 MB, median 2.11 MB, p90 10.19 MB, mean 3.23 MB, total 154.9 MB, per-file spread 82x. A count cap of 40 spans 7.8 MB to 641.3 MB, 129.1 MB at this draw's mean; 500 files is 1.61 GB at this mean. |
| [L 9] | the same query, re-drawn by the round-1 review of this document (recorded in this card's comment thread, 2026-09-25 13:21) | Draw 3 of 3: 48 images, 0.23 MB minimum, 2.59 MB median, 5.56 MB p90, 14.02 MB maximum, mean 2.90 MB, spread 60x. Not produced by the author of this document; included because it is an independent draw of the same query and it is the tightest of the three, which is why 5.1 quotes the 60x draw and not the 378x one. |
| [L 10] | `man 2 kill`, `man 1 ps`, `man 2 flock`, `ps -o pid=,lstart= -p <pid>`, and `grep -n p_starttime "$(xcrun --show-sdk-path)/usr/include/sys/proc.h"` with `grep -n KERN_PROC_PID "$(xcrun --show-sdk-path)/usr/include/sys/sysctl.h"`, while writing 8.8 | `kill(pid, 0)` is the existence check: "A value of 0, however, will cause error checking to be performed (with no signal being sent). This can be used to check the validity of pid", and `ESRCH` is "No process or process group can be found corresponding to that specified by pid"; sending a signal needs a real or effective user ID matching the receiver's, so `EPERM` means the process exists and is not ours. `ps` documents `lstart` as "The exact time the command started" and prints one for a live pid (`Fri Sep 25 20:39:57 2026`); the value comes from `sysctl(KERN_PROC_PID)`, `sys/sysctl.h:437`, into `kp_proc.p_starttime`, `sys/proc.h:97`, "process start time". The macOS `flock` page documents the lock/descriptor association, `EWOULDBLOCK` under `LOCK_NB` and `ENOTSUP`, and does not print the release-on-close sentence that [12] does. |

Sibling research documents. `[D n]` is cited inline above.

| # | Document | What it settles here |
|---|---|---|
| [D 1] | `docs/research/macos.md` | The macOS store holds a URL; a write to a deleted path returns `NO` with `The file doesn't exist.` and writes nothing ([V5c]); the live Space's node is what a readback returns, so a broken entry is the value the platform re-applies. Its side-effects note records the Spice cache pruning the displayed file. This is the evidence under 5.3. |
| [D 2] | `docs/research/linux.md` | GNOME persists `picture-uri` in `~/.config/dconf/user`, which belongs to GNOME and must not be written directly; deleting the file after it has been set is an open test item, not an answered question. The source of the Linux `assumption` in 5.3. |
| [D 3] | `docs/research/windows.md` | `IDesktopWallpaper` is the documented per-monitor setter; a wallpaper cannot be set from a service; the power/pipe model this document assumes for a per-user process. Whether Windows keeps its own copy of the image behind an installed wallpaper is not covered, which is why 5.3 marks it `[unverified]`. |

Grounding note: this is a specification, so most sentences are decisions this
document owns rather than facts taken from elsewhere. The sourced fraction is
deliberately the platform-convention layer plus the measurements: the
Apple, XDG and Microsoft citations are what make the paths correct rather than
invented, `[L 5]` is the measurement under the atomicity rule, and `[L 6]`,
`[L 8]` and `[L 9]` are the three draws under the byte-cap decision, quoted as
samples rather than as a reproducible distribution. Read anything without a
citation as a decision, not as an observation.

## Sources

[1] https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/FileSystemOverview/FileSystemOverview.html
[2] https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/MacOSXDirectories/MacOSXDirectories.html
[3] https://specifications.freedesktop.org/basedir-spec/latest
[4] https://learn.microsoft.com/en-us/windows/win32/shell/knownfolderid
[5] https://learn.microsoft.com/en-us/windows/win32/shell/known-folders
[6] https://learn.microsoft.com/en-us/windows/win32/shell/csidl
[7] https://learn.microsoft.com/en-us/windows/win32/ipc/pipe-names
[8] https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw
[9] https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-lockfileex
[10] https://pubs.opengroup.org/onlinepubs/9699919799/functions/rename.html
[11] https://wallhaven.cc/help/api
[12] https://man7.org/linux/man-pages/man2/flock.2.html
[13] https://man7.org/linux/man-pages/man5/proc_pid_stat.5.html
[14] https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-openprocess
[15] https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocesstimes
