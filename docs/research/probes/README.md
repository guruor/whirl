# probes

Throwaway probes kept so the claims in `../macos.md` can be re-run instead of taken on trust.
Nothing here is product code, and nothing here writes to `Index.plist` except `wp_set`, which asks
macOS to set the wallpaper like any other client would.

Build both binaries into a scratch directory so no binary lands in the repository:

```sh
clang -fobjc-arc -framework AppKit -framework Foundation -framework CoreGraphics -o /tmp/wp_probe wp_probe.m
clang -fobjc-arc -framework AppKit -framework Foundation -framework CoreGraphics -o /tmp/wp_set   wp_set.m
```

`wp_probe` and `wp_set` are build outputs, not committed files: every script below builds them into
`/tmp` from the committed `.m` sources if they are not already there. The `[Vn]` row in
`../macos.md` names the script to run, not the binary it builds.

## Files

| File | What it is |
|---|---|
| `wallpaper_store.py` | Reads `~/Library/Application Support/com.apple.wallpaper/Store/Index.plist` with the nested binary plists decoded. `layers` for the four-layer view and per-Space variation, `dump [space-uuid]` for a decoded Space node, `current <space-uuid>` or `current --holds <path>` for one node's `Desktop` slot as `key=value` lines, `diff a.plist b.plist` for which slots changed. Read-only. |
| `wp_probe.m` | Read-only AppKit probe: per screen, the CGDisplay UUID, frame, `desktopImageURLForScreen`, the option dictionary, and whether the file still exists. |
| `wp_set.m` | Write probe: sets an image on every screen, optionally with the undocumented `@"allSpaces"` option, then reads back. Reports the `NSError`, which is the point. |
| `allspaces_test.sh` | [V5]: does `allSpaces` change which nodes are written? Writes A with the option, B without, diffs the store both ways, restores the live image. Snapshot dir: `w0.plist`, `w1.plist`, `w2.plist`. |
| `churn2.py` | [V5b]: counts `LastSet` and `LastUse` changes between two snapshots (`LastSet: 2 of 2409`, `LastUse: 21 of 2409`). Read-only. |
| `lastuse_groups.py` | [V5b]: groups the `LastUse` churn by timestamp and prints the spread between them. Read-only. |
| `inode_test.sh` | [V5d]: snapshots the store's inode, does one write, snapshots it again, and lists the store dir around the write to show no temp file is visible. |
| `live_spaces.py` | [V6]: `LastUse` age buckets for every Space node in the store. Read-only. |
| `raw_node.py` | [V7]: raw `LastSet` / `LastUse` for the live Space node and its display sub-nodes, from one or more snapshots, no blob decoding. Read-only. |
| `probe-wrapper.sh` | [V7]: runs `/tmp/wp_probe` under `launchctl submit`. |
| `set-wrapper.sh` | [V7]: runs `/tmp/wp_set` under `launchctl submit`. Pass an image that differs from the live one to see the store change. |
| `sandbox_test.sh` | [V9]: builds `wp_set.m` with `com.apple.security.app-sandbox`, ad-hoc signs it (expected: `Trace/BPT trap: 5`), then builds the same source with no entitlement as the control. Deletes nothing; the trap leaves a crash report under `~/Library/Logs/DiagnosticReports`. |

## Recipes

Capture a snapshot before anything you might want to undo:

```sh
cp ~/Library/Application\ Support/com.apple.wallpaper/Store/Index.plist /tmp/before.plist
```

`[V5]` end to end. The script writes into a scratch directory it prints (`$OUT`); `w0.plist`,
`w1.plist` and `w2.plist` are the three snapshots it takes, and they are not committed because they
carry this machine's wallpaper history:

```sh
sh allspaces_test.sh            # prints the work dir, e.g. /var/folders/.../allspaces.XXXXXX
OUT=/var/folders/.../allspaces.XXXXXX
python3 churn2.py $OUT/w0.plist $OUT/w1.plist          # [V5b] LastSet / LastUse counts
python3 lastuse_groups.py $OUT/w0.plist $OUT/w1.plist  # [V5b] timestamp groups + spread
```

The script itself diffs `w0 -> w1` (with `allSpaces`) and `w1 -> w2` (without it) and prints
`changed (choice or LastSet): 2` on the same two nodes both ways, plus the `LastUse` churn. Sleep
between writes: a wallpaper write is not synchronous, and a second call issued immediately can be
dropped (the maintainers of `desktoppr` document the same thing).

Read the store, not the screen:

```sh
python3 wallpaper_store.py dump                 # the most recently used Space, decoded
python3 wallpaper_store.py dump <space-uuid>    # a specific one
python3 wallpaper_store.py current <space-uuid> # one node's Desktop slot, key=value
python3 live_spaces.py                          # [V6] LastUse age buckets, store wide
python3 live_spaces.py /tmp/before.plist        # the same, on a snapshot
python3 raw_node.py /tmp/before.plist /tmp/after.plist   # [V7] raw node timestamps
```

Which Space is frontmost? The store does not say, and the proxy this README used to give (the Space
whose `LastUse` is newest) is not it: measured 2026-09-26 07:00 UTC, the newest-`LastUse` node held
`wallhaven-28y8x9.jpg` while `wp_probe` returned `wh-5dzd11.jpg` from a different node. Every in-use
node carries the same write's `LastUse` within about a millisecond, so the newest of them is
whichever one the wallpaper agent touched last. Take the image from `wp_probe`, which reflects the
live Space's node, and ask the store which node holds it:

```sh
/tmp/wp_probe | sed -n 's/^  desktopImageURL: //p'
python3 wallpaper_store.py current --holds '/System/Library/Desktop Pictures/Mac Yellow.heic'
```

`--holds` falls back to the most recently used holder when several nodes hold the same file (dead
Spaces keep the last picture painted into them, and this machine has 13 nodes on one deleted Spice
temp file), which is a heuristic again. `scripts/desktop-snapshot.sh` and
`scripts/desktop-restore.sh` are the version of this that has to be right: the restore refuses to
write unless the store says the image on screen belongs to the Space node the snapshot names, and
it proves itself by reading the store back afterwards.

One write, and whether the store survives in place `[V5d]`:

```sh
sh inode_test.sh          # inode before, one write, inode after; no temp file in the dir listing
```

Is the process in a GUI session? `launchctl submit` reports the session it picked `[V7]`:

```sh
launchctl submit -l com.example.probe -- /bin/sh "$PWD/probe-wrapper.sh"
launchctl list com.example.probe      # LimitLoadToSessionType = "Aqua" means it can reach the window server
launchctl remove com.example.probe
```

Because `launchctl submit` discards stdout, wrap the redirect in when you want the probe output:

```sh
launchctl submit -l com.example.probe -- /bin/sh -c "$PWD/probe-wrapper.sh > /tmp/probe.read.txt 2>&1"
launchctl submit -l com.example.probe -- /bin/sh -c "$PWD/set-wrapper.sh '/System/Library/Desktop Pictures/Mac Yellow.heic' > /tmp/probe.write.txt 2>&1"
launchctl remove com.example.probe
```

`set-wrapper.sh` with no argument re-sets the image that is already live, which is a no-op in the
store (no `LastSet` bump). Pass a different image when you want the write to register.

Sandbox, for the record. This traps at launch on an ad-hoc signature, which is the finding `[V9]`:

```sh
sh sandbox_test.sh
```

## Two ways to misread this store

- `LastUse` is not a write marker. Every in-use slot gets its `LastUse` bumped by any write, and it
  also moves when the frontmost Space changes, so a naive plist diff shows about twenty changed
  lines for a one-image change. `LastSet` is the write marker; the `Desktop` choice is the image.
- Timestamps are UTC. `LastSet = 2026-09-25 07:28:57` in the plist is `12:58:57` in IST.
