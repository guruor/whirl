# whirl design docs

Everything in the index below is written before the code it describes, and each
document names the evidence it rests on. Nothing is final until it has been
reviewed against its own acceptance criteria: "written" means the document
exists, "reviewed" means someone who did not write it has reproduced its
evidence and said so, in `reviews/`.

| document | status | what it decides |
|---|---|---|
| `research/macos.md` | reviewed | how macOS stores and applies wallpaper per display and per Space, what the supported API is, multi-display and hotplug behaviour, minimum OS version |
| `research/windows.md` | reviewed | the same for Windows 10/11, including whether per-monitor and per-virtual-desktop wallpaper are possible at all |
| `research/linux.md` | reviewed | the per-desktop-environment matrix (GNOME, KDE, sway, Hyprland, X11) and which compositors require a resident process |
| `research/scheduling.md` | reviewed | launchd, Task Scheduler and systemd user timers: missed-run behaviour across sleep, install/uninstall, and whether they can replace an in-daemon timer |
| `spec/features.md` | reviewed | the v0.1 feature set, the source abstraction, filters, and what is explicitly out of scope |
| `spec/state-and-cache.md` | reviewed | per-OS state and cache locations, eviction policy, atomic writes, corruption recovery |
| `architecture.md` | reviewed | process model, protocol specification, failure modes, security model, versioning, the frontend contract |
| `development.md` | reviewed | repo layout, build and test matrix, release process, contribution workflow |
| `decisions/` | one ADR recorded: `0001-macos-artifacts-ship-unsigned-in-v0.1.0.md` | one ADR per decision that changes the architecture; template in `0000-template.md` |
| `reviews/` | four reports: the pack in two rounds, the architecture set in two rounds | one report per reviewed **pack**, not one per document: `research-spec-review.md` and its round 2 cover the six-document research and spec pack, `architecture-review.md` and its round 2 cover the two architecture documents; each says what the reviewer ran, what they observed, what they could not check |

## Reading order

`research/*` first (what the platforms allow), then `spec/*` (what we will build),
then `architecture.md` (how it is structured), then `development.md` (how to work
on it). `CONTRIBUTING.md` at the repository root is the short version of the last
one, for a first pull request.

## Where the code is

`crates/` is the Cargo workspace: `whirl-core`, `whirld`, `whirl-cli` and
`whirl-worker`, four members and no third-party dependency, the four crates
`development.md` section 1 names. It landed with the first slice of the engine,
pull request #1. Which of `development.md`'s commands have been run, and against
what, is the status table at the top of that document.
