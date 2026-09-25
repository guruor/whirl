# probes

Throwaway probes kept so the claims in `../macos.md` can be re-run instead of taken on trust.
Nothing here is product code, and nothing here writes to `Index.plist` except `wp_set`, which asks
macOS to set the wallpaper like any other client would.

Build both binaries into a scratch directory so no binary lands in the repository:

```sh
clang -fobjc-arc -framework AppKit -framework Foundation -framework CoreGraphics -o /tmp/wp_probe wp_probe.m
clang -fobjc-arc -framework AppKit -framework Foundation -framework CoreGraphics -o /tmp/wp_set   wp_set.m
```

| File | What it is |
|---|---|
| `wallpaper_store.py` | Reads `~/Library/Application Support/com.apple.wallpaper/Store/Index.plist` with the nested binary plists decoded. `layers` for the four-layer view and per-Space variation, `dump [space-uuid]` for a decoded Space node, `diff a.plist b.plist` for which slots changed. Read-only. |
| `wp_probe.m` | Read-only AppKit probe: per screen, the CGDisplay UUID, frame, `desktopImageURLForScreen`, the option dictionary, and whether the file still exists. |
| `wp_set.m` | Write probe: sets an image on every screen, optionally with the undocumented `@"allSpaces"` option, then reads back. Reports the `NSError`, which is the point. |

## Recipes

Capture a snapshot before anything you might want to undo:

```sh
cp ~/Library/Application\ Support/com.apple.wallpaper/Store/Index.plist /tmp/before.plist
```

Does `allSpaces` change which nodes are written? Write image A with the option, image B without it,
and diff:

```sh
python3 wallpaper_store.py layers            # record the starting state
/tmp/wp_set "/System/Library/Desktop Pictures/Mac Yellow.heic" allspaces
sleep 3; cp ~/Library/Application\ Support/com.apple.wallpaper/Store/Index.plist /tmp/a.plist
/tmp/wp_set "/System/Library/Desktop Pictures/Mac Pink.heic"
sleep 3; cp ~/Library/Application\ Support/com.apple.wallpaper/Store/Index.plist /tmp/b.plist
python3 wallpaper_store.py diff /tmp/before.plist /tmp/a.plist
python3 wallpaper_store.py diff /tmp/a.plist /tmp/b.plist
```

Expect `changed (choice or LastSet): 2` both times, on the same two nodes, plus
`LastUse: changed on 21`. Sleep between writes: a wallpaper write is not synchronous, and a second
call issued immediately can be dropped (the maintainers of `desktoppr` document the same thing).

Read the store, not the screen:

```sh
python3 wallpaper_store.py dump                 # the most recently used Space, decoded
python3 wallpaper_store.py dump <space-uuid>    # a specific one
```

Which Space is frontmost? The store does not say directly. The proxy used in the note: the Space
whose `LastUse` is newest is the one on screen, and a `wp_set` run lands on it.

Is the process in a GUI session? `launchctl submit` reports the session it picked:

```sh
launchctl submit -l com.example.probe -- /tmp/wp_probe
launchctl list com.example.probe      # LimitLoadToSessionType = "Aqua" means it can reach the window server
launchctl remove com.example.probe
```

Sandbox, for the record. This traps at launch on an ad-hoc signature, which is the finding:

```sh
printf '<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict><key>com.apple.security.app-sandbox</key><true/></dict></plist>\n' > /tmp/sandbox.entitlements
clang -fobjc-arc -framework AppKit -framework Foundation -framework CoreGraphics -o /tmp/wp_set_sb wp_set.m
codesign --force --sign - --identifier com.example.sandboxprobe --entitlements /tmp/sandbox.entitlements /tmp/wp_set_sb
/tmp/wp_set_sb /System/Library/Desktop\ Pictures/Mac\ Yellow.heic   # Trace/BPT trap: 5
log show --last 2m --predicate 'process == "wp_set_sb"'             # AMFI: adhoc signed, not valid
```

## Two ways to misread this store

- `LastUse` is not a write marker. Every in-use slot gets its `LastUse` bumped by any write, and it
  also moves when the frontmost Space changes, so a naive plist diff shows about twenty changed
  lines for a one-image change. `LastSet` is the write marker; the `Desktop` choice is the image.
- Timestamps are UTC. `LastSet = 2026-09-25 07:28:57` in the plist is `12:58:57` in IST.
