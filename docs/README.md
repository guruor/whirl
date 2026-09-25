# whirl design docs

Everything here is written before the code it describes, and each document names
the evidence it rests on. Nothing is final until it has been reviewed against its
own acceptance criteria.

| document | status | what it decides |
|---|---|---|
| `research/macos.md` | to be written | how macOS stores and applies wallpaper per display and per Space, what the supported API is, multi-display and hotplug behaviour, minimum OS version |
| `research/windows.md` | to be written | the same for Windows 10/11, including whether per-monitor and per-virtual-desktop wallpaper are possible at all |
| `research/linux.md` | to be written | the per-desktop-environment matrix (GNOME, KDE, sway, Hyprland, X11) and which compositors require a resident process |
| `research/scheduling.md` | to be written | launchd, Task Scheduler and systemd user timers: missed-run behaviour across sleep, install/uninstall, and whether they can replace an in-daemon timer |
| `spec/features.md` | to be written | the v0.1 feature set, the source abstraction, filters, and what is explicitly out of scope |
| `spec/state-and-cache.md` | to be written | per-OS state and cache locations, eviction policy, atomic writes, corruption recovery |
| `architecture.md` | written 2026-09-25 | process model, protocol specification, failure modes, security model, versioning, the frontend contract |
| `development.md` | to be written | repo layout, build and test matrix, release process, contribution workflow |

## Reading order

`research/*` first (what the platforms allow), then `spec/*` (what we will build),
then `architecture.md` (how it is structured), then `development.md` (how to work
on it).
