# whirl milestones

M1 and M2 as shipped, and M3's exit criteria. Written 2026-09-26 from the merged record:
`development` @ `607422c`, `main` @ `b9de3ec`, and `gh pr list --state all --limit 60`.

## Why this file exists

Two milestones landed in two days: 174 cards, 47 merged pull requests, 32,820 lines changed on
`development`. Until this file, the definition of both lived in a chat and in card bodies.
`grep -ri milestone docs/` returned nothing, so a fresh agent or a new contributor could not read
what M1 or M2 were, whether they closed, or what M3 has to satisfy. M2's own closing summary flagged
the same gap one level down: there is no `docs/releases/v0.1.0.md` either.

## What a milestone is here

- A **milestone** is a batch of cards, named `M0`, `M1`, `M2`, and so on. A batch of cards is never
  "an iteration" or "a loop": milestone is the word this project uses, on the board, in card titles
  and in every summary sent to the user.
- A milestone **closes when its cards close and its exit criteria hold**, and the criteria are
  verified by a card of its own that fails closed. If one criterion fails, that card reports the
  failed criterion with its evidence and posts nothing.
- **A criterion is a capability and its proof.** It is a command run against a ref plus the value it
  must print, or an artifact that must exist at that ref. It is never a merge, a pull request's
  state, an open-PR list, or another card's status. That rule is not stylistic. M2's criterion 3
  could only be satisfied by the human merging a pull request, and it held the milestone for about
  five hours across three gate runs; see "M2's exit criteria, and how they behaved".

## M1: 2026-09-25, the design pack and the first slice of the engine

**Closed 2026-09-25.** Ten merged pull requests, 15,617 lines changed. In both tables below the last
column is the pull request's title, trimmed; the time is its `mergedAt` in UTC, to the minute; the
rows are in pull-request order, which is not merge order.

| PR | merged (UTC) | lines | what it carries |
|---|---|---|---|
| #1 | 2026-09-25 12:18 | 8,114 | the v0.1 Rust workspace: protocol, config, socket, CLI, worker, CI on three OS |
| #3 | 2026-09-25 13:11 | 281 | the gitleaks gate in CI, and a parser fixture no scanner objects to |
| #4 | 2026-09-25 13:15 | 3,303 | the daemon's read surface and the state layer, to the spec |
| #5 | 2026-09-25 13:26 | 670 | branches and releases: `development` integrates, `main` releases, a tag publishes |
| #6 | 2026-09-25 13:39 | 240 | assert the bytes of `config check` and the value of `config path` |
| #7 | 2026-09-25 14:07 | 93 | reconcile the six contradictions the build surfaced, in the docs |
| #8 | 2026-09-25 14:40 | 1,629 | the rotation plan, the worker spawn contract and supervision |
| #9 | 2026-09-25 14:58 | 969 | take the startup lock, and report `lock_mode` truthfully |
| #10 | 2026-09-25 15:07 | 155 | the escalation test pins SIGTERM to the deadline, not just the grace |
| #11 | 2026-09-25 15:24 | 163 | the spec for what releases the `excl_file` lock, and what a stale lock means |

Eleven pull requests were opened in this window. One of them was closed unmerged, and its successor is
the merge that carries its change; the successor is not numbered here either, because this file names
only pull requests that are merged.

### M1's exit criteria, as met

M1 closed on a report, not on a gate: no card on the board carried M1's criteria, and the milestone
was announced to the user in a message rather than checked against a card. The first milestone card
with criteria in its body is M2's gate, and that is the gap this file exists to close. The rows below
are therefore the criteria M1 is **closed on**, each written as the command or the artifact that
proves it at `development` @ `607422c`. The artifacts were read at that ref while this file was
written; the commands are the ones to run to reproduce them. They are checkable; they are not a
quotation, because there is nothing to quote.

| criterion | proof, and what it shows |
|---|---|
| The design pack is in the repo, and every document in it was reviewed by someone who did not write it. | `ls docs/reviews/` prints four reports (`research-spec-review.md`, `research-spec-review-round2.md`, `architecture-review.md`, `architecture-review-round2.md`), and `docs/README.md`'s table marks `research/*`, `spec/*`, `architecture.md` and `development.md` as `reviewed`. |
| The workspace builds, and the gate is green on three OS. | `./scripts/ci.sh local` is the command (fmt, clippy, test, artifacts); `.github/workflows/ci.yml` is seven jobs, and `clippy` and `test` each run on `ubuntu-latest`, `macos-latest` and `windows-latest`, which is the three-OS claim. Carried by #1, which created `crates/` (31 files). |
| The daemon's read surface and its state layer answer to the spec. | `crates/whirld/src/socket.rs`, `state.rs` and `statefile.rs`; `whirl config check` and `whirl config path` assert their bytes and their value in `crates/whirl-cli/tests/argv.rs` (#4, #6). |
| The rotation plan and the worker spawn contract exist, with supervision. | `crates/whirld/src/plan.rs` (`Flags::parse`, `Flags::resolve`, `source_records`, `sources_enabled`) and `crates/whirld/src/worker.rs` (#8). |
| The daemon takes the startup lock and reports `lock_mode` truthfully. | `crates/whirld/src/lock.rs`, and `crates/whirld/tests/daemon_lock.rs`, which compares the reported `lock_mode` with the primitive actually holding the lock (#9). The escalation test pins SIGTERM to the deadline rather than the grace (#10). |
| What releases the `excl_file` lock, and what a stale lock means, are written down. | `docs/spec/state-and-cache.md` 7.2, 8.7 and 8.8, and `docs/architecture.md` `[D 6 §8.8]` (#11). |
| `development` integrates and `main` releases, and a tag is what publishes. | `.github/workflows/ci.yml`, `.github/workflows/release.yml` and `docs/development.md` section 5 (#5). |
| A secret cannot reach the tree unnoticed. | `.gitleaks.toml` and the `secrets` job, which scans the commits a change adds and fails when the scan reads no commits (#3). |

## M2: 2026-09-26, make it real: the sources and the macOS setter

**Closed 2026-09-26**, at `development` = `607422c` (`Merge pull request #14`), 11:56:40Z.
Thirty-seven merged pull requests, 17,203 lines changed.

At the close: `main` = `b9de3ec`; `git rev-list --count origin/main..origin/development` = `217`;
`git tag` = no tags; `docs/releases/v0.1.0.md` = absent; one pull request open, the worker-tests branch
at `a787ddf`, MERGEABLE/CLEAN, 14 of 14 checks green, unmerged because merging on this repo is the
human's action. It is not numbered here: this file names only merged pull requests.

| PR | merged (UTC) | lines | what it carries |
|---|---|---|---|
| #12 | 2026-09-26 04:45 | 120 | enforce the tag guard, and pin `ci.yml`'s eleven actions |
| #13 | 2026-09-26 04:57 | 3,146 | the worker's stage pipeline, the cache writes and the dedupe |
| #14 | 2026-09-26 11:56 | 728 | the macOS backend really sets the wallpaper, and reads it back |
| #15 | 2026-09-26 06:03 | 74 | 5.4's cap checks ran on a config 4.3 refuses |
| #16 | 2026-09-26 05:17 | 47 | the `daemon_lock` citation points at section 8 item 7, and `NotAnImage` gets a direct test |
| #17 | 2026-09-26 05:36 | 1,999 | the cache sweep, both caps and the index |
| #18 | 2026-09-26 05:46 | 444 | `WHIRL_CACHE_DIR` is ignored by the worker the daemon reported it to |
| #19 | 2026-09-26 11:21 | 356 | the recent window: the daemon and the worker read different state directories |
| #20 | 2026-09-26 05:39 | 128 | retry a spawn the kernel refused with `ETXTBSY` |
| #21 | 2026-09-26 06:06 | 90 | send SIGTERM without needing a kill binary, so the term test is green in a container |
| #22 | 2026-09-26 05:57 | 20 | spec 1.6: the two path knobs are the daemon's resolved directories, always set |
| #23 | 2026-09-26 06:10 | 6 | 7.2's readers column names every reader of `history.json`, the cache index and the config |
| #24 | 2026-09-26 07:51 | 729 | an interrupted socket read is retried, not the end of the connection |
| #25 | 2026-09-26 06:19 | 7 | spec 7.2: the index row names no command surface, and no surface is a reader |
| #26 | 2026-09-26 06:26 | 141 | reach `kill(2)` directly, so a missing binary cannot stop the signal |
| #27 | 2026-09-26 08:13 | 239 | the local gate: one script whose modes are the commands CI runs |
| #28 | 2026-09-26 06:43 | 38 | 7.2 gains a Surface column, the verb that reaches each file |
| #29 | 2026-09-26 06:37 | 216 | the busy-spawn test asks the guest instead of assuming `ETXTBSY` |
| #30 | 2026-09-26 07:04 | 575 | make every CI job call the runner a developer runs, and keep the pins |
| #31 | 2026-09-26 06:51 | 10 | 6.5 marks `whirl reset` as absent from the v0.1 surface |
| #32 | 2026-09-26 08:17 | 873 | the worker's half of `rotate.lock`, and the race test for it |
| #33 | 2026-09-26 09:02 | 2,249 | the `local` source, so a rotation can produce a cache file |
| #34 | 2026-09-26 06:52 | 2 | 7.2's `current.json` readers cell names the human, not `cat` |
| #35 | 2026-09-26 07:06 | 18 | spec 2.5.1: the CLI carries a `whirl ping` arm, so the docs gain it |
| #36 | 2026-09-26 07:01 | 85 | the control socket is created 0600 in one step, so nothing ever observes it 0700 |
| #37 | 2026-09-26 08:07 | 19 | 5.4's citations follow `architecture.md`, and its caveat is now true |
| #38 | 2026-09-26 08:50 | 484 | the daemon's side of the lock: 8.8's removal under the fallback |
| #39 | 2026-09-26 08:36 | 16 | give the second daemon in the refusals test its own roots |
| #40 | 2026-09-26 07:58 | 165 | make `help` a verb, since USAGE advertises it |
| #41 | 2026-09-26 08:25 | 7 | the secrets guard counts commits, so ten is not zero |
| #42 | 2026-09-26 08:35 | 245 | the harness waits on the daemon for 300 s, not 30 s |
| #43 | 2026-09-26 08:36 | 581 | the testing guide: snapshot and restore the live desktop, so a real-hardware check leaves no trace |
| #44 | 2026-09-26 09:27 | 413 | the deadline's kill is a reap, so the sweep removes that worker's `rotate.lock` |
| #45 | 2026-09-26 09:58 | 211 | the slow-worker deadline test measures its own premise, and reports the pid its kill reaped |
| #46 | 2026-09-26 10:54 | 2,441 | the `wallhaven` source, search and collection, with the floor enforced locally |
| #47 | 2026-09-26 10:05 | 206 | the record names a path only where whirl owns the bytes |
| #48 | 2026-09-26 11:15 | 75 | `Enumerated` carries the candidates only, the walk counters leave |

### M2's exit criteria, and how they behaved

Quoted here as history. The first of the four is the shape this project stopped using after M2, and
"M3's exit criteria" below are written without it.

| # | what M2's gate checked | verdict at the close |
|---|---|---|
| 1 | RETIRED SHAPE: the open pull-request list is empty, or every remaining one is named with the reason it is still open. | PASS. One was open, named with its reason and its head sha, and nothing in its own state blocked a merge. The criterion is a status report rather than a gate, and it is the one to read as an anti-pattern. |
| 2 | `crates/whirl-worker/src/sources/mod.rs` `build()` returns a real source for `SourceKind::Local` and for `SourceKind::Wallhaven`, not `None`. | PASS. Blob `e1a43470`, line 84 `Some(local::source(source))`, line 85 `Some(wallhaven::source(source))`, no arm answers `None`. |
| 3 | Every platform setter that v0.1's scope claims is real, read from the function body. Every setter it does not claim refuses with a named error. | PASS, after it held the milestone. `crates/whirl-worker/src/backend/macos.rs`, blob `300d8ce2`, 616 lines, no "not implemented yet": `set()` at line 398 sends `setDesktopImageURL:forScreen:options:error:` per screen through `objc_msgSend` and maps a `NO` return to `SetFailed`. Linux and Windows are stubs that fail loudly with `set_failed`, which is M3's work, not a failure here. |
| 4 | Report `git rev-list --count origin/main..origin/development` and whether `docs/releases/v0.1.0.md` exists. | Reported: `217`, and the file does not exist. |

**The one thing that held M2.** Criterion 3 could not be satisfied by the fleet: it needed #14
merged, and merging on this repo is the human's action, not a card's. #14 was green and approved from
2026-09-25 16:15, went `CONFLICTING` against `development` on one file
(`.github/workflows/ci.yml`), was reconciled by its own card, and merged at 2026-09-26 11:56:40Z,
which is `development`'s tip and the moment the milestone closed. Criterion 2 held it earlier the same
way, first on #33 and then on #46. The gate refused to post three times, named the failed criterion
each time, and published no false summary; that behavior is the one to protect in M3. The lesson
written into the criteria below is not "gate less", it is "gate on the capability, and report the
landing numbers in the summary instead of gating on them".

## M3: as planned

M3 is the other two platforms and the release. Four parts, each its own set of cards. Every criterion
below is a command against the branch under review plus the value it must print, or an artifact that
must exist. None of them is satisfied by a landing state or a board state: each is a capability with
its own proof.

### M3a: Linux adapters

One adapter per desktop, plus the session detection that chooses between them:
GNOME (`gsettings set org.gnome.desktop.background picture-uri`), KDE (`qdbus`),
sway (`swaymsg output <name> bg <file> <mode>`), generic X11 (`feh` / `xwallpaper` / `hsetroot`),
and Hyprland (`hyprctl hyprpaper wallpaper`) where it is claimed. The tool per desktop, and whether
it is one-shot or needs a resident helper, are `docs/research/linux.md`'s decision table. The file
they land in is `crates/whirl-worker/src/backend/linux.rs`, which today is a stub.

| criterion | proof |
|---|---|
| Each of the four desktops has its own adapter, and the session signal selects it. | `git show <branch>:crates/whirl-worker/src/backend/linux.rs` names one command per desktop (the strings above), and the selection test asserts the mapping from the session signals in `docs/architecture.md` 1.6 to the adapter chosen. Artifact: the file, plus the test that pins the mapping. |
| The code compiles and the suite passes on Linux, in the container the gate uses. | `./scripts/ci.sh linux` exits 0 on the branch (clippy, test, artifacts and guards, in the gate's container), and the `test (ubuntu-latest)` and `clippy (ubuntu-latest)` jobs are green at that head. |
| Every adapter refuses what it cannot serve with a named error, never a silent success. | On a session with `gsettings` but no `gsettings-desktop-schemas`, the built worker prints the `No such schema` error and exits non-zero; with `qdbus` or `swaymsg` absent, the message names the missing binary. Proof: the test that asserts that string, and its output. |
| The part is labelled honestly until a real Linux desktop proves it. | The header of `crates/whirl-worker/src/backend/linux.rs` and the release notes carry `unverified on real hardware`, with the date and the reason (this machine has no Linux desktop session). Removing the label requires the per-environment walk-through in `docs/research/linux.md`, run on that desktop, with the output recorded in the release notes. |

### M3b: Windows setter

`crates/whirl-worker/src/backend/windows.rs`, which today is a stub, in two modes:
`SystemParametersInfoW(SPI_SETDESKWALLPAPER, ...)` for `display.mode = "all"`, and
`IDesktopWallpaper::SetWallpaper(monitorID, path)` for `"per-display"`. `docs/research/windows.md`
holds the API answer and the thirteen-item real-hardware list.

| criterion | proof |
|---|---|
| The setter calls the documented API and reads it back, for both modes. | `git show <branch>:crates/whirl-worker/src/backend/windows.rs` calls `SPI_SETDESKWALLPAPER` for `all` and `SPI_GETDESKWALLPAPER` for the readback, and `IDesktopWallpaper::SetWallpaper` with a monitor id from `GetMonitorDevicePathAt` for `per-display`. Artifact: the function bodies, not the file's presence. |
| The `#[cfg(windows)]` code compiles, and the cross-check runs here. | `./scripts/ci.sh windows` exits 0, and the `test (windows-latest)` job is green at that head. |
| A set is proved against a real Windows desktop, not a compile. | On the windows runner, one test sets a wallpaper from the cache and the platform's own readback returns that path. Artifact: the test's name and output line, from the `test (windows-latest)` job's run for that head. |
| Per-virtual-desktop is refused, not faked. | `whirl config check` on a config asking for per-virtual-desktop wallpaper on Windows exits non-zero and names the absent public API (`IDesktopWallpaper` does not model virtual desktops). Proof: the command and the printed reason. |
| The part is labelled honestly: a CI runner is a real Windows, but not a real user session. | The backend header and the release notes say which checks ran on a runner and which need a desktop, and the label is removed only by the thirteen-item list in `docs/research/windows.md` being run and recorded. |

### M3c: per-display matrix

This is the one part that needs hardware the fleet does not have, so it is the one part whose criteria
may legitimately block on it. `docs/spec/features.md` 1.3 is the policy: `"per-display"` is honoured
only where a platform's research answered yes, and rejected at `whirl config check` with that
platform's reason elsewhere.

| criterion | proof |
|---|---|
| The matrix is reproduced per platform, not asserted. | For each of macOS, Windows and Linux: `whirl config check` on a config with `display.mode = "per-display"` prints the verdict the matrix claims, which is accept on Windows (`IDesktopWallpaper::SetWallpaper` takes a monitor id) and on sway, and reject with the platform's reason on macOS (no API names a Space) and on GNOME (one image on all monitors, a hard limitation). Artifact: the command, the exit code and the printed line, once per platform. |
| On this machine, a per-display set touches one screen and leaves the other alone. | With a second display attached: `scripts/desktop-snapshot.sh` first, then a rotation naming one screen, then the readback of both screens with `docs/research/probes/wp_probe.m` (it prints `desktopImageURLForScreen` per screen); the named screen shows the new image and the other shows what it showed before. Artifact: the two readbacks, following `docs/development.md` "When a check needs a real set". This criterion cannot pass on a one-display machine and is expected to block there. |
| The desktop is left exactly as it was found. | `scripts/desktop-restore.sh` after the check, and the restore proved by reading the store back and printing the image line. A test fixture left as the desktop picture is a failed check, not a pass. |
| `whirl status` reports the mode that actually ran. | `whirl status` prints `display_mode_effective: per-display` where the platform answered yes, `display_mode_effective: all` plus one warning line where the research has no answer yet, per 1.3's third fallback. Artifact: the status line. |

### The release path

Not part of any earlier milestone, and the thing M2's summary named as still missing before v0.1.0.
`docs/development.md` section 5 is the procedure and `.github/workflows/release.yml` is it in
executable form (guard, build, package, notes, publish). The promotion from `development` to `main`
is a pull request reviewed by someone who did not open it, and `main` is back-merged into
`development` immediately after; those are procedure, not criteria.

| criterion | proof |
|---|---|
| The notes exist on the ref that will be tagged, and hold every required section. | `git cat-file -e origin/main:docs/releases/v0.1.0.md` exits 0, and the file's headings cover what changed, the per-platform checklist results, and every config key added, removed or defaulted differently. Artifact: the file. |
| `main` carries everything `development` carries. | `git rev-list --count origin/main..origin/development` prints `0`. |
| The tag exists and points at `main`'s promoted commit. | `git ls-remote --tags origin v0.1.0` prints the tag with its commit, and `git merge-base --is-ancestor <tag> origin/main` exits 0, which is the check the `guard` job in `.github/workflows/release.yml` runs. |
| The tag published a release with one archive per platform, and the run is green. | `gh release view v0.1.0 --json assets` lists three archives named `whirl-v0.1.0-<os>-<arch>.<ext>`, and `gh run list --workflow release.yml --limit 1` shows that tag's run as `success`, `guard` included. |
| The notes state what is unverified. | The labels section of `docs/releases/v0.1.0.md`: the Linux and Windows setters labelled unverified on real hardware, with the reason, and who ran the macOS checklist. |

## What is deliberately not in v0.1.0, and why

| not in v0.1.0 | why |
|---|---|
| A Linux or Windows setter verified on real desktop hardware. | This machine has no Linux desktop session, so the Linux adapters ship compile- and container-verified and labelled `unverified on real hardware`; the Windows setter can be exercised on the CI runner but not in a real user session. `docs/development.md` section 3 is the list of what CI cannot prove, and each row's human check is where these are settled. That checklist, not the code, is the real release gate. |
| `"per-display"` on a platform whose research answered no. | macOS: a `forScreen:` set covers the frontmost Space only and no public API names a Space, so per-display is rejected at `whirl config check` with that reason (`docs/spec/features.md` 1.3). The key stays in the schema so the config does not have to change later. |
| Per-virtual-desktop wallpaper on Windows or Linux. | No public API: `IDesktopWallpaper` does not model virtual desktops, and the Windows 11 per-desktop path is undocumented shell COM. |
| `whirl reset`. | Absent from the v0.1 surface; `whirl reset` answers with the CLI's usage rather than acting (`docs/spec/state-and-cache.md` 6.5). |
| Packaging, code signing, notarization, installers, auto-update. | Release engineering rather than v0.1 product scope. What ships today is what `.github/workflows/release.yml` builds: three unsigned archives from `cargo build --workspace --release`. |
| The rest of the non-goals: a settings GUI, a tray icon as a requirement, video and live wallpapers, cloud photo accounts, on-the-fly editing, tagging and curation, multi-user or remote control, a browser or preview UI, dynamic plugin loading, colour extraction, scheduling rules beyond a fixed interval. | One line each in `docs/spec/features.md` Part 3, each with its cost. |

## How this file was written, and how to check it

- The pull request list and every date in it come from
  `gh pr list --state all --limit 60 --json number,state,mergedAt,additions,deletions`. "lines" is
  `additions + deletions` for that pull request. Every number in this file is merged:
  `gh pr view <n> --json number,mergedAt` is the check, and a `mergedAt` of `null` would make the
  document a claim rather than a record.
- The ref facts come from `git fetch origin --prune` then `git rev-parse origin/main origin/development`,
  `git rev-list --count origin/main..origin/development`, `git tag` and
  `git cat-file -e origin/development:docs/releases/v0.1.0.md`.
- **Three things in the record disagree with this file, or sit behind it, and all three are named
  rather than smoothed:**

  1. **Where the M1/M2 line falls.** The milestone report posted to the Whirl Telegram topic on
     2026-09-26 12:14 IST used "M1" for the wider batch it was reporting on at that moment: "the
     design pack and the engine", 78 cards and 25 merged pull requests, closed 2026-09-26. Read that
     way, the pull requests this file puts in M2 up to about #25 belong to M1, and M2 is "make it
     real": the sources and the macOS setter. This file cuts at the merge date instead, because that
     cut is what `gh pr list` reproduces in one command: M1 is everything merged on 2026-09-25, M2 is
     everything merged from 2026-09-26 through #14, which is `development`'s tip. Nothing about what
     shipped is in dispute, only where the line falls, and a reader who wants the older cut has that
     message in the topic.
  2. **`docs/README.md` is behind the tree, not this file.** Its last section still says
     "`crates/` does not exist yet; the workspace scaffold is the next thing to land", which stopped
     being true when #1 merged on 2026-09-25. Left as found here, and carded separately.
  3. **`docs/development.md` section 7 is behind the tree too.** Its "When a check needs a real set"
     paragraph still says the macOS setter is a stub on `development` "until PR #14 lands", which
     stopped being true when #14 merged at 2026-09-26 11:56:40Z, the tip this file is written against.
     Left as found here, and carded with the previous one. A reader who starts at `docs/README.md`,
     or at that paragraph, should know the tree is ahead of them.

  Nothing in this file contradicts `docs/spec/` or `docs/architecture.md`: where those documents
  state M1 and M2 behavior (`state-and-cache.md` 6.5, 7.2 and 8.8, `architecture.md` 1.6, 2.5.1 and
  `[D 6 §8.8]`, `features.md` 1.3 and Part 3) this file cites them rather than restating them.
