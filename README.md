# whirl

A cross-platform wallpaper manager built as a small daemon with an `mpd`-style
line protocol, a config file instead of a settings UI, and thin clients that can
be added later without touching the core.

**Status: design phase.** Nothing is released. [docs/](docs/) holds the research,
the specs, the architecture and the development guide; [prototype/](prototype/)
holds a throwaway spike that establishes the two numbers the design rests on. The
Cargo workspace under `crates/` is the next thing to land.

## Why another wallpaper app

They start small and drift. A measured example on macOS (Spice, a Go/Fyne
wallpaper manager, 2026): **883 MB resident and still climbing**, ~1% CPU
continuously, 354 MB of unbounded image cache, and a resident face-detection
model it may never use. The cause is architectural, not a leak: one immortal
process owns the UI toolkit, an in-RAM index of every image it knows about, the
scheduler, and the image pipeline, so every feature it ever gained became a
permanent memory floor. A single 4672x7008 wallpaper (125 MB decoded) raised the
floor by 128 MB for the rest of the process's life, and nothing gave it back.

## The design rule

> The resident process owns state. It never owns pixels.

Consequences, all measured in the spike rather than assumed:

| piece | resident | binary |
|---|---|---|
| daemon (Rust, no dependencies) | **1.8 MB at start, 2.3 MB after 7 rotations** | 0.69 MB |
| worker, one per rotation | 21-25 MB for ~1.5 s, then gone | 8.4 MB |
| CLI client | 0 MB (exits immediately) | 0.45 MB |
| same daemon doing the decode in-process, for comparison | **12 MB -> 140 MB, never returns** | — |

That last row is the whole argument: the identical workload, delegated instead of
owned, is flat forever.

## Intended v0.1 scope

- **Sources:** local directory and Wallhaven. Sources are data (a config entry),
  not plugins, so adding one costs no resident memory.
- **Config file only.** No settings window, no tray required to operate.
- **Scheduling:** whatever the OS already does well (launchd, Task Scheduler,
  systemd timer) or the daemon's own timer, decided per platform by the research
  in `docs/research/`.
- **Control:** one CLI over a unix socket (`0600`), named pipe on Windows. The
  protocol is the API; anything that speaks it is a valid frontend.
- **Frontends are optional and thin:** a TUI, a menu bar item, a Raycast
  extension, a shell script. None of them own state.

## Repo layout

```
crates/        the workspace: whirl-core, whirld, whirl-worker, whirl-cli (next thing to land)
docs/          research, specs, architecture, the development guide
prototype/     the throwaway spike: reference only, not shipped
```

## Working on it

Start with [CONTRIBUTING.md](CONTRIBUTING.md) for a first pull request, and
[docs/development.md](docs/development.md) for the repo layout, the toolchain and
dependency policy, the test matrix, the release process, and how to run a rotation
without changing your own wallpaper.

## Non-goals

- Being a wallpaper *browser* or a gallery application.
- Bundling a GUI toolkit into the daemon.
- Chasing platforms or sources nobody asked for.

## Licence

MIT. See [LICENSE](LICENSE).
