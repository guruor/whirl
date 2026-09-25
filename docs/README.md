# whirl design docs

Everything here is written before the code it describes, and each document names
the evidence it rests on. Nothing is final until it has been reviewed against its
own acceptance criteria: "written" means the document exists, "reviewed" means
someone who did not write it has reproduced its evidence and said so, in
`reviews/`.

| document | status | what it decides |
|---|---|---|
| `research/macos.md` | reviewed | how macOS stores and applies wallpaper per display and per Space, what the supported API is, multi-display and hotplug behaviour, minimum OS version |
| `research/windows.md` | reviewed | the same for Windows 10/11, including whether per-monitor and per-virtual-desktop wallpaper are possible at all |
| `research/linux.md` | reviewed | the per-desktop-environment matrix (GNOME, KDE, sway, Hyprland, X11) and which compositors require a resident process |
| `research/scheduling.md` | reviewed | launchd, Task Scheduler and systemd user timers: missed-run behaviour across sleep, install/uninstall, and whether they can replace an in-daemon timer |
| `spec/features.md` | reviewed | the v0.1 feature set, the source abstraction, filters, and what is explicitly out of scope |
| `spec/state-and-cache.md` | reviewed | per-OS state and cache locations, eviction policy, atomic writes, corruption recovery |
| `architecture.md` | reviewed, defects being fixed | process model, protocol specification, failure modes, security model, versioning, the frontend contract |
| `development.md` | reviewed, defects being fixed | repo layout, build and test matrix, release process, contribution workflow |
| `decisions/` | template in place, no decisions recorded yet | one ADR per decision that changes the architecture; template in `0000-template.md` |
| `reviews/` | three reports: the pack (two rounds) and the architecture set | one review report per reviewed document: what the reviewer ran, what they observed, what they could not check |

## Reading order

`research/*` first (what the platforms allow), then `spec/*` (what we will build),
then `architecture.md` (how it is structured), then `development.md` (how to work
on it). `CONTRIBUTING.md` at the repository root is the short version of the last
one, for a first pull request.

## Where the code is

`crates/` does not exist yet; the workspace scaffold is the next thing to land,
and `development.md` section 1 is the tree it has to produce. Until it lands,
`development.md`'s commands are verified against a throwaway scaffold rather than
against this repository, and it says so.
