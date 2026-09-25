# prototype/ — the throwaway spike

Reference material, **not the product**. Written in a day to answer two questions
before committing to a design:

1. Can a wallpaper rotator be built with a resident footprint in single-digit
   megabytes instead of hundreds?
2. Which part of an existing wallpaper app is what actually gets heavy?

Both answers are in the numbers below. Nothing here is meant to survive the
rewrite; it exists so the rewrite doesn't have to rediscover it.

## What was measured

Run on macOS 26 (Tahoe), Apple Silicon, 24 GB RAM, 2560x1440 display, 10 Spaces.

| approach | resident | peak per operation | disk |
|---|---|---|---|
| Spice (Go + Fyne, the app this project exists because of) | 883 MB and climbing | — | 354 MB cache, uncapped |
| `wh-rotate` worker, one process per rotation | 0 between rotations | 21-25 MB for 1.2-3.4 s | cache pruned to `keep` files (6 files, ~5 MB) |
| `whd` daemon + worker, control plane resident | **1.8 -> 2.3 MB, flat over 7 rotations** | worker peaks do not touch the daemon | tiny: history + favorites, ~8 KB |
| `whd` daemon doing the image decode **in-process** | **12 MB -> 140 MB and never returns** | 141 MB transient | — |

The last two rows are the same daemon, same workload, one flag apart
(`-inproc`). The in-process variant runs `runtime.GC()` **and**
`debug.FreeOSMemory()` after every single decode and still ratchets, because the
floor is set by the largest image ever decoded, not the average:

```
idle                                    12.0 MB
rot  1  decoded 1920x1080 (  7.9 MB RGBA)   24.7 MB
rot  2  decoded 2518x3723 ( 35.8 MB RGBA)   46.1 MB   floor moves
rot  3  decoded 1920x1080 (  7.9 MB RGBA)   46.2 MB   small image, stays
rot  6  decoded 4672x7008 (124.9 MB RGBA)  140.1 MB   floor moves again
rot 7-12 ~10 MB images                     140.3 MB   permanently
```

That is the mechanism behind the 883 MB figure, reproduced in twelve requests.

## What is here

```
wh-rotate/     Go worker: fetch a candidate, download, set the wallpaper, prune
               the cache, exit. One process per rotation, nothing resident.
               Per-OS setters behind build tags: AppKit via cgo (macOS),
               SystemParametersInfoW (Windows), per-DE (Linux).
whd/           Rust daemon: config, schedule, history, favorites, and a
               line protocol over a unix socket. Zero dependencies. Execs
               wh-rotate for every pixel-touching operation.
whctl.rs       Thin client for that protocol (the `mpc` to the daemon's `mpd`).
measure.py     Drives a running daemon and samples its RSS during rotations.
```

## What it proves

- The per-OS wallpaper API is reachable from a short-lived process; no resident
  GUI toolkit is needed. On macOS the setter must nevertheless run inside the
  Aqua session: a per-user LaunchAgent qualifies, a pre-login daemon does not
  (`docs/research/macos.md`, section 9).
- Sources-as-data works: three sources (Wallhaven query, Wikimedia Commons, local
  folder) cost ~150 lines and **zero** additional resident memory.
- A daemon can hold config, schedule, history and favorites while staying under
  3 MB, if it delegates the image work.
- `idle`/subscribe gives frontends push updates, so a TUI never polls.

## Corrected by later research

`docs/research/macos.md` was produced on this machine and falsifies four things
this spike asserted at the time. Do not build on the original claims:

- `NSWorkspace.setDesktopImageURL` with the `"allSpaces"` option **changes nothing
  observable**. The key is undocumented and inert; the "10 Space nodes rewritten"
  was `LastUse` churn, not a per-Space write.
- AppleScript `set desktop picture` is **not** a silent no-op. It works.
- `desktoppr` does **not** require sudo. Homebrew's cask installs via a `.pkg`,
  but the same release ships the bare binary in a `.zip`.
- There is no "Appearance node" in the store.

## What it does not prove

- Windows and Linux backends are **compile-verified only**. No hardware was
  available; nobody has run them. Treat those files as hypotheses.
- Windows has no per-virtual-desktop wallpaper API: `IDesktopWallpaper` does
  per-monitor wallpaper but has no virtual-desktop concept at all
  (`docs/research/windows.md`).
- The daemon wakes twice per rotation (worker start and finish), so an `idle`
  subscriber sees duplicate notifications. Cosmetic, unfixed here.
- No code signing, packaging, installer, update path, or CI. No tests worth the
  name: the measurement scripts are the only verification that exists.

## Running it

The spike expects a Wallhaven-free environment; public endpoints need no API key
(never commit one).

```sh
cd wh-rotate && CGO_ENABLED=1 go build -o wh-rotate . && ./wh-rotate -dry-run
cd ../whd && rustc -O whd.rs -o whd && rustc -O whctl.rs -o whctl
WHD_SOCKET=$PWD/state/sock ./whd &
WHD_SOCKET=$PWD/state/sock ./whctl status
```

Config for the worker is JSON; see `wh-rotate/example-config.json`.
