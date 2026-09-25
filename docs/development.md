# whirl: development guide

How to build whirl, test it, ship it, and change it without breaking the rules the
architecture sets. Written for someone who has just cloned the repository and has
never spoken to anyone on the project.

Read `docs/architecture.md` first if you have not. This document assumes its
vocabulary (daemon, worker, control socket, noop backend) and repeats none of its
reasoning. Every rule below is stated as a rule, and each one names where it comes
from: a measured prototype number, a research finding, or this document's own
decision.

## Status, and what in here has actually been executed

| thing | state |
|---|---|
| `docs/`, `prototype/` | exist |
| the Cargo workspace (`crates/`) | exists on pull request #1: `whirl-core`, `whirld`, `whirl-cli`, `whirl-worker`, four members and no third-party dependency |
| `.github/workflows/ci.yml` | written here, lints clean, and **has run**: green on macOS, Windows and Linux, run `36126085459`, twelve jobs |
| `.github/workflows/release.yml` | written here, lints clean, and **has run**: a throwaway prerelease tag `v0.0.1-rc.test`, run `36140056903`, four jobs green, the release published with its three platform archives attached and then deleted together with its tag |
| a running daemon reachable from a checkout | yes: the section 7 sequence below, driven from a fresh clone of pull request #1 on a scratch socket, no wallpaper touched |
| anything that sets a real wallpaper in CI | never, by design (see "What CI cannot prove") |

**How the commands in this document were verified.** The workspace did not exist
when this document was written, so the local-development sequence, the guard
scripts and the CI command set were run against a throwaway scaffold kept outside
the repository: same crate names, same binary names, same environment variable
names, `cargo build`, `cargo test`, `cargo fmt`, `cargo clippy -D warnings`, a
daemon on a scratch socket with the noop backend, and a real worker spawned per
rotation. That scaffold is not product code and is not committed; it exists to
prove the guide's commands before the guide is merged. The exact sequence, and the
observed output, are in "Local development" and in the card's handoff note.

What that means for you: the commands are correct as written, and the first thing
the scaffold card owes this document is the same sequence passing in the real
tree. If a command here does not work after the scaffold lands, that is a bug in
one of the two, and this document is the one to fix.

**Executed on this machine (macOS), for this document:** `cargo build`, `cargo
test`, `cargo fmt --check` and `cargo clippy -- -D warnings` on the verification
scaffold; the daemon on a scratch socket with `WHIRL_BACKEND=noop`, answering
`status`, `next`, `config check` and `config path`; the worker both spawned by the
daemon and run by hand for `rotate` and `check`; `printf 'ping\n' | nc -U
<socket>` with the `nc` that ships on macOS; the two `guards` checks (dependency
scan, binary size caps) as standalone scripts; `actionlint 1.7.11` on the workflow
plus a real YAML parse of it; `gh run list -R guruor/whirl` (empty); `rustc -O` on
the prototype's `whd.rs` and `whctl.rs` in a scratch directory (727,072 and
469,496 bytes, the sizes `docs/architecture.md` cites as `[L 3]`); and the two
vendor pages in Sources, with their prices, retrieved 2026-09-25.

**Not executed, and never claimed as executed:** anything on Windows or Linux; the
Go build of `wh-rotate` (it needs CGO and the macOS SDK); any real wallpaper set;
signing or notarization; any CI job on any runner; the packaged artifact outside a
checkout.

## 1. Repo layout

The final tree. Crate names and binary names are fixed by `docs/architecture.md`
1.2; nothing here may be renamed without an ADR (section 6).

```
Cargo.toml                  workspace root: members, edition, rust-version, shared keys
Cargo.lock                  committed (see section 2)
rust-toolchain.toml         the pinned toolchain and its components
rustfmt.toml                formatting rules, so `cargo fmt` is not a matter of taste
README.md                   what the project is, and where to start
CONTRIBUTING.md             the short version of sections 6 and 7
LICENSE                     MIT
crates/
  whirl-core/               shared types, no I/O, no platform code
    src/lib.rs              module list
    src/protocol.rs         the line protocol: verbs, the request line, response and
                            error records, protocol version
    src/config.rs           the config schema, its validation and its error messages
    src/state.rs            state records: current, history ring, favorites, cache index
    src/source.rs           the source trait and the candidate type
  whirld/                   the daemon binary (`whirld`)
    src/main.rs             argument and environment handling, startup order
    src/socket.rs           the control socket: bind in a directory it creates 0700,
                            with the umask restricted around bind and an fchmod to
                            0600 (no window), accept loop, connection limit
                            (docs/architecture.md 2.1, Pr4)
    src/plan.rs             the scheduler and the rotation plan
    src/state.rs            state reads and atomic writes
    src/sweep.rs            cache sweep against both caps
    src/worker.rs           spawning and supervising the worker (the contract in
                            docs/architecture.md 1.6)
  whirl-worker/             the worker binary (`whirl-worker`), one process per rotation
    src/main.rs             argv, scrubbed environment, stage pipeline
    src/sources/            local directory and Wallhaven
    src/pipeline.rs         filters, dedupe, cache writes
    src/backend/            the only place platform code lives: macos.rs, windows.rs,
                            linux.rs, noop.rs
  whirl-cli/                the CLI binary (`whirl`)
    src/main.rs             verb parsing and exit codes
    src/render.rs           turning protocol records into text
docs/
  architecture.md           process model, protocol, platform verdict, resident rules
  development.md            this file
  README.md                 index and status of every document
  spec/                     features, state and cache layout
  research/                 per-platform findings, with the real-hardware checklists
  decisions/                ADRs: NNNN-title.md, template in 0000-template.md
  reviews/                  review reports, one per reviewed document
  releases/                 one file per release: vX.Y.Z.md, the notes the tag
                            workflow publishes, written before the tag (section 5)
.github/workflows/ci.yml    the gate: push to `main` or `development`, pull request
                            into either, by hand (section 4)
.github/workflows/release.yml   a tag `v*` becomes a release: same artifacts, notes,
                                and a GitHub Release (section 5)
.github/release-notes-template.md   the shape of a release's notes, and the sections
                                    a release may not publish without (section 5)
prototype/                  the throwaway spike: read-only, never shipped, never built by CI
```

Where things live, in one line each:

- **The protocol types** are in `whirl-core::protocol`. The daemon, the worker and
  the CLI all link `whirl-core`, so the grammar has exactly one implementation.
  A second parser anywhere is a defect.
- **The platform backends** are in `crates/whirl-worker/src/backend/` and nowhere
  else. `whirld` must not depend on `whirl-worker`, must not link a GUI toolkit,
  and its binary stays under a megabyte (`docs/architecture.md` R2). The CI
  `guards` job enforces the size.
- **The config schema** is in `whirl-core::config`, and it is one implementation
  used by the daemon, the worker, and `whirl config check`, because a config error
  the user must fix by hand cannot have two different messages
  (`docs/architecture.md` 4.3).
- **The scheduler** is in `whirld::plan`. The worker never schedules itself.

### Why `prototype/` stays

It is kept, not deleted, and it is not a starting point for new work:

1. **It is the evidence behind the design.** The resident-cost table in
   `docs/architecture.md` 1.3 and the README (1.8 MB idle, 2.3 MB after 7
   rotations, 12 MB to 140 MB in-process) cite `prototype/whd/whd.rs` and
   `prototype/README.md` by file and line. Deleting it would leave those rows
   uncited and un-reproducible.
2. **It is the record of the protocol the architecture supersedes.** Section 2.12
   of the architecture lists what changed from protocol 1 to protocol 2 and why.
   A reader who wants to check that claim needs the old implementation.
3. **It documents the bugs the design exists to avoid.** The unconditional socket
   unlink, the worker inheriting the daemon's whole environment, the in-process
   decode ratchet: each is named in the architecture with a line in `prototype/`.

Rules for `prototype/`: read it, build it in a scratch directory if you want to
reproduce a measurement (see "Local development"), and do not add features to it,
do not wire it into CI, and do not ship it. It is excluded from the workspace, so
`cargo build --workspace` never touches it. The prototype's `whd`, `whctl` and
`wh-rotate` names are retired; new work uses `whirld`, `whirl` and `whirl-worker`.

## 2. Toolchain and dependency policy

These are rules. A pull request that breaks one is rejected, not discussed.

### The toolchain

- **The toolchain is pinned in `rust-toolchain.toml`, and that file is the single
  source of the version.** It arrives with the workspace scaffold; its contents
  are fixed as:

  ```toml
  [toolchain]
  channel = "1.94.0"
  components = ["rustfmt", "clippy"]
  profile = "minimal"
  ```

  Reason for pinning rather than tracking `stable`: `rustfmt` output changes
  between releases, so an unpinned toolchain means "my formatting is wrong on your
  machine". One version, everywhere, and a one-line pull request when it moves.

- **The MSRV is 1.85.0, and it is declared in `[workspace.package]` as
  `rust-version`.** 1.85 is the first release with edition 2024, and edition 2024
  is what the workspace uses, so the MSRV is the edition's floor rather than a
  guess. The CI `msrv` job builds and tests with exactly `1.85.0`; that job is
  what makes the number a fact instead of a hope.

- **Raising the MSRV is a minor release, never a patch.** A pull request that
  raises it must do all four of these, in the same commit:
  1. change `rust-version` in the workspace manifest and the MSRV in the `msrv`
     job of `.github/workflows/ci.yml` (the `toolchain` input, the job name and
     the `+1.85.0` commands, all three),
  2. name in the commit body the language or standard library feature that needs
     the newer compiler,
  3. keep the previous MSRV working unless removing it is the point, and
  4. remove the last reference to the old MSRV from this document.
  If the raise is more than two releases ahead of the current MSRV, it also needs
  an ADR (section 6): that is a change to who can build the project. Raising the
  MSRV "to be current" is not a reason and will be rejected.

- **The nightly toolchain is never required.** Nothing in the workspace may depend
  on a nightly-only feature, and no CI job runs nightly.

### Formatting and lints

- **`cargo fmt --all -- --check` is the formatting gate.** `rustfmt.toml` fixes
  `max_width = 100`, `newline_style = "Unix"` and `use_field_init_shorthand =
  true`. Within those, formatting is not a review topic: run `cargo fmt --all`
  before you push.
- **Warnings are denied in CI, and only in CI.** The gate is
  `cargo clippy --workspace --all-targets -- -D warnings`, run on all three
  operating systems. Locally, plain `cargo build` is allowed to warn: a warning
  that only appears on Windows is still a failure, but you should not have to
  guess at it while you work. `--all-targets` matters, because a warning inside
  `#[cfg(test)]` is a warning: tests are code.
- **Lint suppressions are per-site and carry a reason.** `#[allow(...)]` without a
  comment saying why, or at file or crate level, is rejected. Prefer fixing the
  code; if that is genuinely wrong, put the `#[allow]` on the smallest item that
  works and say why in the same comment.

### The lock file

**`Cargo.lock` is committed, for the whole workspace.** whirl ships binaries;
binaries are applications; an application build that resolves dependencies
differently on two machines is not reproducible. The same rule means `cargo
update` is a deliberate, reviewable commit and never something a feature branch
does quietly. (Today the lock file is four lines of our own crates, because of
the rule below; the rule stays in place for the day it is not.)

### Dependencies: zero, and a PR is the only way in

The prototype shipped with **no third-party dependencies**, and the daemon's
binary size (0.69 MB in the prototype, and the reason the daemon stays under a
megabyte) is a direct consequence. The rule:

- **v0.1 has zero third-party dependencies in the workspace.** `cargo build` needs
  nothing but the standard library, and the CI `guards` job fails if any crate
  other than the four workspace members appears in `Cargo.lock`. This is checked
  mechanically so that it is a rule and not an aspiration.
- **A dependency is added by pull request, with four things stated in the body.**
  (1) which crate, at which version, and what it replaces; (2) the size change in
  each binary it lands in, measured with `cargo build --release` before and after;
  (3) confirmation that it is not a GUI toolkit, not an async runtime, and not an
  image decoder, since those three are the things this project exists to avoid
  (`README.md`, "Why another wallpaper app"); (4) what happens when it is
  unmaintained in two years, meaning who would remove it and what the replacement
  is.
- **A dependency that changes a decision in `docs/architecture.md`** (a transport,
  the config format, the process model) needs an ADR as well.
- **The platform backends are the standing exception in spirit, not in rule.**
  `whirl-worker` is the only crate allowed to grow platform code, and on Windows
  and macOS that means calling the OS, which is not a dependency in the size
  sense. Nothing there licenses a new crate in `whirl-core` or `whirld`.

## 3. Build and test matrix

What each CI job exercises, and how to run the same thing locally.

| job | runner(s) | command | what it actually exercises |
|---|---|---|---|
| `fmt` | ubuntu | `cargo fmt --all -- --check` | formatting only |
| `clippy` | ubuntu, macos, windows | `cargo clippy --workspace --all-targets -- -D warnings` | all three `cfg` paths compile clean, including the three transports and the Windows named-pipe code |
| `test` | ubuntu, macos, windows | `cargo test --workspace` | unit tests plus the integration tests; `WHIRL_BACKEND=noop` |
| `msrv` | ubuntu | `cargo +1.85.0 check --workspace --all-targets` then `cargo +1.85.0 test --workspace` | the declared MSRV is real |
| `guards` | ubuntu | `cargo metadata` plus a `Cargo.lock` scan, then a release build and a size check | zero third-party dependencies, and the binary size caps |
| `artifacts` | ubuntu, macos, windows | `cargo build --workspace --release` plus `upload-artifact` | the release build produces `whirld`, `whirl`, `whirl-worker` on every platform |

The local equivalents are the same commands, in this order, and they are what to
run before opening a pull request:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
WHIRL_BACKEND=noop cargo test --workspace
cargo build --workspace --release
```

`WHIRL_BACKEND=noop` is not optional in the test command if any test rotates; the
workflow sets it for the whole job so that an added test cannot forget it.

### What CI cannot prove, and who proves it instead

Continuous integration runs on headless machines. It can prove that the code
compiles and that the parts that never touch a desktop behave. It **cannot** prove
any of the following, and a release that claims them without a human check is
lying:

| not provable in CI | how a human proves it |
|---|---|
| that a real wallpaper changes, at all, on each platform | run the real backend on each platform, once, by hand |
| per-monitor and per-Space behaviour | the macOS checklist in `docs/research/macos.md` (section "Verified on this machine" and the unverified list) |
| hotplug, resolution and DPI changes, virtual desktops, slideshow and Spotlight interaction | the 13-item list in `docs/research/windows.md`, "What must be tested on real Windows before release" |
| each Linux desktop environment, including the session-detection signals | the per-environment lists in `docs/research/linux.md`, one under each of GNOME, KDE, sway, Hyprland and generic X11 |
| login-item behaviour after a reboot, which is where a wallpaper rotator actually lives | the launchd, Task Scheduler and systemd user-timer procedures in `docs/research/scheduling.md` |
| consent prompts, sandbox and notarization behaviour | `docs/research/macos.md` section 8, on a machine that has never run whirl before |
| that the packaged artifact works outside a checkout | install the built package on a clean machine or VM |

The rule that ties this together: **a release is blocked until the checklist for
each platform it supports has been run on real hardware by someone who has that
hardware, and the release notes record who ran it.** CI green is a precondition,
not the evidence.

## 4. Continuous integration

`.github/workflows/ci.yml` is the gate. Seven jobs: `fmt`, `clippy` (three OS),
`test` (three OS), `msrv`, `guards`, `secrets`, `artifacts` (three OS). It runs
on pushes to `main` **and to `development`**, on every pull request whose base is
either, and by hand with `workflow_dispatch`. It declares
`permissions: contents: read`, so a compromised step cannot write to the
repository, and it cancels superseded runs on the same ref.

The pull request whose base is `main` is a promotion, and it runs the whole
matrix like any other change: the promotion is the last look `development` gets
before the tag (section 5). A **tag** does not run this workflow at all; a tag
runs `.github/workflows/release.yml`, which is the release itself and the only
workflow here that is allowed to write to the repository (section 5).

`secrets` runs gitleaks over the commits the change adds, not over the tree and
not over the whole history: gitleaks' git mode reads a commit's added lines, so a
change that *removes* a value cannot trip it, and a value already in history needs
no exemption. It installs the release binary with the version and the sha256 both
pinned in the workflow, and it fails when the scan reads no commits, because a
scan that checked nothing and a scan that found nothing print the same `no leaks
found`. The scanner's own rules stay on; `.gitleaks.toml` only adds exemptions,
each with a reason that the job prints on every run.

The rule that keeps it honest: **every command a contributor is expected to run
before opening a pull request appears in the workflow, verbatim.** If a check is
not in the file, it is a preference, not a gate. Adding a check means adding it in
both places in the same pull request.

Caching covers `~/.cargo/registry`, `~/.cargo/git` and `target`, keyed by runner
OS and the `Cargo.lock` hash. With zero dependencies there is little to cache
today; the cache exists because the compiled `target` directory and the pinned
toolchain download are the two costs that grow the first time a dependency or a
platform backend arrives.

No untrusted input reaches a shell: nothing in the workflow interpolates an event
payload into a `run:` step. The `secrets` job is the one place that reads two of
them (the pull request's base commit and `github.sha`); it passes them in through
`env:`, never as script text, and the script refuses any value that is not hex
before it is used as a range.

### What has and has not been verified about the workflows

- **Verified on this machine:** `actionlint` (1.7.11) reports no problems for this
  file and for `.github/workflows/release.yml`, and a real YAML parse finds 7 jobs
  and the three triggers here, and the 2 jobs of the release workflow with its one
  trigger. Both commands are in the handoff note.
- **Verified when this guide was written, and now superseded:** `gh run list -R
  guruor/whirl` returned an empty list, so no CI run had executed. **The first run
  is pull request #1's:** run `36126085459`, green, twelve jobs. A run named by its
  id is the only green-pipeline claim this repository may make, including this
  document.
- **Not verified, and marked as such:** that each job passes on its runner. The
  equivalent commands were run locally on macOS only, and that held until the first
  push. The first push is pull request #1's: run `36126085459` passed `fmt`,
  `clippy`, `test`, `msrv`, `guards` and `artifacts` on `ubuntu-latest`,
  `macos-latest` and `windows-latest`.
- **Ordering, stated plainly:** the workflow goes green only once the Cargo
  workspace exists. Until the scaffold lands, a push of this file will fail
  `fmt`, `clippy`, `test`, `msrv`, `guards` and `artifacts` at the first `cargo`
  command, because there is no workspace to build. Merge the scaffold with, or
  before, the first push that triggers this workflow.
- **The `secrets` job was made to fail before it was trusted.** Run `36137663507`
  is that job red with the other twelve jobs green, on a single probe commit that
  added a credential-shaped file: the log line is
  `##[error]gitleaks rule generic-api-key matched ci-red-check.txt:5 in commit 8e133a179b8d`.
  Run `36137447410` is the same job green on the change itself: `1 commits
  scanned`, `no leaks found`. The probe commit was dropped from the branch rather
  than reverted, because a revert leaves the commit that adds the value inside the
  scanned range and the job stays red (section 6, `Secrets`).
- **The release workflow was made to publish, and to clean up after itself, before
  it was trusted.** A throwaway prerelease tag `v0.0.1-rc.test` at commit
  `86d90319b4bb`: run `36140056903`, four jobs green (`build` on `ubuntu-latest`,
  `macos-latest` and `windows-latest`, then `release`). `gh release view
  v0.0.1-rc.test` reported `prerelease: true` and three assets; the linux archive
  was downloaded *from the release*, and it holds `./whirld`, `./whirl` and
  `./whirl-worker`, ELF 64-bit x86-64 binaries. The release and the tag were then
  deleted, and both are verified gone: `gh release view v0.0.1-rc.test` fails with
  `release not found`, `gh release list` is empty and `git ls-remote --tags origin`
  prints nothing. Two things about that proof are stated rather than implied: the
  tag was pushed at the commit that carries the workflow, on the card's branch,
  because the workflow reaches `main` only through the promotion that card opens,
  and a tag on `main` today would run nothing; and the rendered notes carry the
  template's "on `main`" line, which a tag at a branch head does not satisfy, which
  is a second reason the throwaway tag was deleted rather than kept.
- **The release notes gate was exercised outside CI, against the step's own
  python,** in six cases before the tag was pushed: a prerelease with no notes file
  (renders the template and publishes), a prerelease whose template is missing a
  required section (fails), a final release with no notes file (fails), a final
  release with a filled notes file (publishes), a final release with a placeholder
  left (fails, naming it), and a tag that is not release-shaped (fails). The probe,
  its output and the case list are in the card's handoff note.
- **One annotation on both runs, recorded because it is not a failure:** `Node.js
  20 is deprecated. The following actions target Node.js 20 but are being forced to
  run on Node.js 24: actions/checkout@11d5960a, actions/upload-artifact@ea165f8d`.
  `ci.yml` uses `actions/checkout@v4`, which resolves to the same commit today, so
  the annotation is not new to the release workflow. The pins move to v5 by a
  deliberate pull request when the v4 line stops running (the table below).

### What rots, and where the single copy of it lives

Version numbers and URLs age, so each one has exactly one home, and this document
does not repeat the value anywhere else:

| rots | single home | what to do when it moves |
|---|---|---|
| the toolchain version | `rust-toolchain.toml` | a one-line pull request; CI picks it up through `rustup show` |
| the MSRV | `rust-version` in the workspace manifest, plus the `msrv` job | the four steps above, in one commit |
| the pinned action versions, and the gitleaks version and its digest | `.github/workflows/ci.yml` and `.github/workflows/release.yml` | Dependabot or a deliberate pull request. An action added from now on is pinned to a full commit SHA, never to a tag or a branch: a tag can be moved under the repository between two runs |
| the two vendor prices and their URLs | the Sources list at the end of this document, each with its retrieval date | re-fetch, and correct the date, before repeating the number as a fact |
| the platform facts | `docs/research/*`, each with its own retrieval date and provenance | a research card, not an edit here |

A price or a version quoted in this document without its retrieval date is a bug:
the number is the vendor's to change and ours to re-check.

## 5. Release process

### Versions

- **Semantic versioning, `0.x` until all three platforms have passed their
  real-hardware checklists.** `1.0` is the point at which the protocol and the
  config schema are declared frozen.
- **The protocol version is not the release version.** `PROTOCOL_VERSION` in
  `whirl-core::protocol` is negotiated at runtime (`docs/architecture.md` 2.4) and
  moves on its own schedule. The greeting carries both, so a stale client fails
  fast with a message instead of behaving strangely.
- **A release is a git tag `vX.Y.Z` on `main`,** plus the artifacts the tag
  workflow builds from it (below). No branch is a release. The notes are written
  *before* the tag, not after it, because the workflow publishes them from the
  tagged commit: they must include what changed, the per-platform checklist
  results (section 3), and any config key added, removed or defaulted differently.
- **A patch release fixes; a minor release adds.** Anything that raises the MSRV,
  changes a config key's meaning or changes the protocol is a minor release.

### The promotion: how a commit reaches `main`

`development` integrates and `main` releases, and the step between them is one
pull request. It is the last look before the tag, so it has five parts and no
shortcuts:

1. **`development` is green.** The *tip* of `development` has a green `ci.yml` run,
   named by its run id in the pull request. "The last change was green" is a
   different claim, and it is not the one that matters.
2. **The release-blocking checklists have been run on real hardware,** for every
   platform this release supports, by people who have that hardware. The results
   are the notes' required sections (step 3). CI green is a precondition, not the
   evidence (section 3, "What CI cannot prove").
3. **The notes are written on `development`**, at `docs/releases/vX.Y.Z.md`, from
   `.github/release-notes-template.md`. They travel with the promotion because the
   tag has to point at a commit that already contains them: the workflow refuses
   to publish a final release whose notes file is missing or still holds a
   placeholder.
4. **The promotion pull request is opened and merged:**
   `gh pr create --base main --head development`. It runs the full matrix, because
   `ci.yml` triggers on a pull request whose base is `main` as well as
   `development` (section 4), and it is reviewed like any other pull request, by
   someone who did not open it. Nothing else rides in it: a change that did not
   land on `development` first does not enter `main` through the promotion.
5. **`main` is back-merged into `development` immediately** (section 6, the
   back-merge rule), before anything else and before the tag.

Then, and only then, the tag: "Cutting a release, step by step" below.

### Making the tag do the work

`.github/workflows/release.yml` runs on a tag push (`on: push: tags: ["v*"]`) and
is this document's procedure in executable form:

| step | what happens |
|---|---|
| build | `cargo build --workspace --release` on `ubuntu-latest`, `macos-latest` and `windows-latest`: the command the `artifacts` job runs, and the same three binaries per platform |
| package | one archive per platform, `whirl-<tag>-<os>-<arch>.<ext>`, and the run fails if the runner's architecture is not the one the archive name claims, because a mislabelled artifact is worse than a missing one |
| notes | `docs/releases/<tag>.md` from the tagged commit. A `vX.Y.Z-rc.N` tag with no such file is rendered from `.github/release-notes-template.md`, placeholders and all, which is what the prerelease flag says out loud. A final release with no notes file is refused, and so is a file that is missing a required section or still holds a placeholder |
| publish | `gh release create <tag>` with the three archives attached, and `--verify-tag` so a typo cannot create a tag quietly. A tag with a prerelease suffix (`v0.2.0-rc.1`) is published as a GitHub prerelease; `vX.Y.Z` is not |

Three consequences worth stating:

- **The tag is the trigger, not a note to yourself.** Nothing else has to be
  clicked, run or remembered after `git push origin vX.Y.Z`.
- **The notes are part of the promotion,** not something typed into the release
  page afterwards, because the workflow reads them out of the tagged commit.
- **A failed run is fixable without re-tagging.** Fix the notes, land them through
  `development` and a promotion, then `gh run rerun <run id>`. Re-tagging means
  deleting a tag that someone may already have fetched, which is the kind of
  manual repair this workflow exists to remove.

The workflow does not run the test suite, and does not need to: the tagged commit
is `main`'s tip, which is a promotion's merge commit, and `ci.yml` has already run
the whole matrix on it, both on the promotion pull request and on the push to
`main` the merge produced. The tag is the last step of a procedure that starts
with a green `development`, not a substitute for it.

### Cutting a release, step by step

For someone who has never done it here:

1. **Decide the version.** SemVer (above): a patch fixes, a minor adds, and `0.x`
   holds until all three platforms have passed their checklists. The protocol
   version is not the release version and moves on its own schedule.
2. **Bump `version` in `[workspace.package]` in `Cargo.toml`, on `development`,**
   as its own pull request, one change per pull request, and let it go green.
3. **Write the notes on `development`**, at `docs/releases/vX.Y.Z.md`, from
   `.github/release-notes-template.md`, with the checklist results from step 2 of
   the promotion. Same route: a pull request into `development`.
4. **Promote:** the five steps above, ending with the back-merge.
5. **Tag `main`'s tip and push it:**

   ```sh
   git fetch origin
   git switch main && git pull --ff-only
   git tag --annotate vX.Y.Z --message "whirl vX.Y.Z"
   git push origin vX.Y.Z
   ```

   Annotated rather than lightweight: a release has an author, a date and a
   message, and `git describe` should say what a lightweight tag cannot.
6. **Watch the run and the release:**

   ```sh
   gh run list --workflow release.yml --limit 1
   gh release view vX.Y.Z
   ```

   If it failed on the notes gate, fix the notes, land them through `development`
   and a promotion, and rerun the failed run. Do not delete the tag to push it
   again.
7. **A prerelease is the same procedure with `vX.Y.Z-rc.N`,** except that step 3 is
   optional and the run marks it as a prerelease. That is the only shape to use for
   a rehearsal: a tag that looks like a final release is not a rehearsal, and
   deleting one afterwards leaves a release in every clone that fetched it.

### What a release artifact is, per platform

What the tag workflow produces today, and therefore what `gh release view` shows
for a release:

| platform | artifact | notes |
|---|---|---|
| macOS | `whirl-vX.Y.Z-macos-arm64.tar.gz`: the three binaries | built on `macos-latest`, which is arm64; an Intel build is a deliberate addition to the matrix, not something to assume |
| Windows | `whirl-vX.Y.Z-windows-x86_64.zip`: `whirld.exe`, `whirl.exe`, `whirl-worker.exe` | per-user install, no service: the setter needs the user's session (`docs/research/windows.md` 4) |
| Linux | `whirl-vX.Y.Z-linux-x86_64.tar.gz`: the three binaries | distribution packages are deferred; see below |

The archive is the three binaries and nothing else, because that is what the
workflow builds and what it can prove it built. Three things a release will have
to carry and does not yet, each its own piece of work rather than a line here:

- **The supervisor's own files:** a launchd plist, a Task Scheduler registration
  script, a systemd user unit. `docs/research/scheduling.md` has the text of each;
  until they are in the artifact, the notes say what installing the release
  involves.
- **A `.pkg`, or any installer.** The tarball and the zip are the install.
- **Signing and notarization.** The macOS artifacts are unsigned today, so the
  release notes have to say what a first run looks like (below).

Binary size is part of the promise, not an accident:

| binary | cap, enforced by the `guards` job | measured on the verification scaffold |
|---|---|---|
| `whirld` | 1.00 MiB | 0.57 MiB |
| `whirl` | 2.00 MiB | 0.41 MiB |
| `whirl-worker` | 12.00 MiB | 0.44 MiB (noop backend only, so this is the floor, not the shipped figure) |

The daemon's cap comes from `docs/architecture.md` R2: the daemon must never link
a platform backend. If `whirld` approaches a megabyte, someone linked one.

### Signing, notarization and packaging: what the research settled

- **macOS: signing is not required for it to work; notarization is required to
  distribute it.** A binary you built yourself, or one installed by a package
  manager, runs without a Gatekeeper prompt. A Developer ID signed bundle
  distributed over the internet must be notarized, or the user gets a first-run
  prompt, and there is no entitlement that avoids it
  (`docs/research/macos.md` 8, quoting Apple's notarization documentation). The
  annual cost of the Developer ID route is the Apple Developer Program
  membership: **99 USD per membership year** (Apple, "Membership Details",
  retrieved 2026-09-25). Sandboxing is the one thing not to do: adding
  `com.apple.security.app-sandbox` broke the wallpaper setter in the research
  probe with `Trace/BPT trap: 5` (`docs/research/macos.md` 8). Decision: **v0.1
  ships signed and notarized for the tarball and `.pkg`**, because the whole point
  of a wallpaper rotator is that it runs at login without a dialog; ad-hoc signing
  is enough for contributors building locally.
- **Windows: the research documents no signing requirement and no cost.** The
  verification levels in `docs/research/windows.md` cover the COM interface, the
  monitor model and the hosting model, and never reach distribution. So the honest
  statement is that this repository has not researched Windows signing, and
  therefore cannot price it. Two facts from the vendor, retrieved 2026-09-25, for
  whoever picks that up: Microsoft's own signing service (Azure Artifact Signing,
  formerly Trusted Signing) is **9.99 USD per month for the Basic plan, 5,000
  signatures per month**, requires a paid Azure subscription, and **does not buy
  instant SmartScreen trust**: it warns that reputation builds over time and that
  early releases should expect the "unknown publisher" warning
  (Microsoft, "Code signing options for Windows app developers"). Decision for
  v0.1: **ship unsigned, and put the SmartScreen first-run warning in the release
  notes**, including the "More info, Run anyway" step. Buying trust that a
  reputation system still has to grant, at a recurring cost, is not worth it
  before there are users. Revisit it as a research card if download warnings
  become a support load, not before.
- **Linux: nothing to sign, and no package format to invent.** What the research
  decides is the runtime side, not the packaging side. GNOME and KDE need no
  extra package beyond the session itself; sway needs `swaybg`, which the
  compositor owns and whirl must never start or kill; generic X11 needs one of
  `feh`, `xwallpaper` or `hsetroot`; on every supported target whirl supervises
  nothing (`docs/research/linux.md`, "The resident helper" and "Recommendation for
  v0.1"). Decision: **ship a tarball plus a systemd user unit, and add a
  distribution package only when someone asks for one.** A `.deb` or an AUR
  package is a maintenance commitment with a second build system, and the tarball
  is what the install instructions already describe. Deferred deliberately, not
  forgotten: distribution packages, Flatpak, and Snap.
- **Never bundle a helper.** `swaybg`, `hyprpaper`, `feh` and friends belong to
  the session or the compositor. A release that ships one has broken the design
  rule in the README.

### Upgrading, and what happens to a running daemon

The daemon is started and restarted by the platform supervisor (launchd,
Task Scheduler, systemd user unit), never by a client. An upgrade is therefore
always: **stop, replace, start.** The specific sequence:

1. Stop the supervisor's job, with the commands `docs/research/scheduling.md`
   Part 3 documents: `launchctl bootout gui/$(id -u)/com.guruor.whirl` on macOS,
   `schtasks /delete /tn whirl /f` on Windows, `systemctl --user stop
   whirl.service` on Linux. The daemon releases `state/locks/daemon.lock` on
   exit.
2. Replace the binaries in place.
3. Start the job again (`bootstrap`, `schtasks /create`, `systemctl --user start`).
   `whirl status` should show a new `pid` and an `uptime_s` near zero, with
   `history_count`, `last_digest` and `last_via` unchanged, because those come
   from the state files rather than from the process.

What is deliberately not supported:

- **No hot swap.** Replacing `whirld` while it runs leaves the old process holding
  the lock, and the new one refuses to start with the holder's pid in the message
  (`docs/architecture.md` 1.5 step 1). That refusal is the feature: two daemons
  rotating the same wallpaper is worse than a failed upgrade.
- **No client-triggered restart.** `daemon start|stop|restart` is not in the verb
  set (`docs/architecture.md` 1.5), because a second supervisor competes with the
  first.
- **A worker in flight during an upgrade is allowed to die.** The daemon records
  the failure, and the next rotation is a fresh process
  (`docs/architecture.md` 1.7). Nothing is left half-written: cache writes are
  temp-file plus rename, so a killed worker leaves a `.part` file for the sweep,
  not a truncated image.
- **State is never overwritten by guesswork.** A state file that fails to parse is
  quarantined in place as `<name>.corrupt-<UTC timestamp>`, the daemon rebuilds
  that file, and it reports `state_corrupt` plus `state_quarantined` rather than
  starting quiet and empty. A corrupt `favorites.json` is the one case that stops
  writing instead of rebuilding, because guessing about pins could delete
  protected cache files. A state file written by a **newer** build is left exactly
  as it is, becomes read-only for this build, and is reported as
  `state_schema_newer` (`docs/spec/state-and-cache.md` 6.4). That is why a
  downgrade is "run older code against the newer files and read the warning",
  not "start clean": nothing is destroyed to make an older binary happy.

## 6. Contribution flow

### Branches: `development` integrates, `main` releases

Three kinds of branch, one line each:

- **`development` integrates.** Every change lands here first, and it is the only
  branch a working branch is opened against.
- **`main` releases.** A commit reaches `main` only as a promotion (section 5), and
  a release is a tag on it. It is always meant to be production-ready, so it never
  carries an intermediate failure.
- **A working branch is short-lived.** One branch per change, cut from
  `development`, named `<area>/<short-topic>`, for example
  `daemon/stale-socket-probe`, `worker/wallhaven-pagination`,
  `docs/development-guide`. Delete it after its pull request merges, and never
  reuse it after that.

The rules that follow from that:

- **Every change arrives as a pull request into `development`.** Open it with
  `gh pr create --base development`. **`main` never receives a direct commit:**
  not from a maintainer, not from an agent, and not to fix a release in a hurry.
  A hotfix is a change like any other, so it goes through `development` and then
  through a promotion.
- **Cut the working branch from the tip of `development`,** and keep it there with
  `git pull --ff-only origin development`. It does not rebase and it does not
  merge: a branch whose diff is only its own change is the branch a reviewer can
  read.
- **The back-merge rule.** Anything that reaches `main`, a promotion or a later
  hotfix, is merged back into `development` immediately, before any other work
  lands:

  ```sh
  git fetch origin
  git switch development && git pull --ff-only
  git merge --no-ff --no-edit origin/main
  git push origin development
  ```

  **Why it exists:** it keeps `main`'s tip an ancestor of `development`. That is
  what lets the next working branch fast-forward onto `development` instead of
  rebasing (which rewrites commits a reviewer has already read) or merging (which
  buries the change in a merge commit and makes the next diff unreadable). Skip
  the back-merge and the damage is not in `main`, it is in the next branch.
- **The rule is enforced, not remembered.** Branch protection on `main` requires
  a pull request and passing checks, so a direct commit, a force push or a merge
  with a red pipeline is refused by the host rather than by good intentions;
  `development` requires passing checks too, so a red tip is visible before a
  promotion reads it. This document describes the expectation; the repository
  owner applies the settings, because a screenshot of a setting is not a rule.
- One change per pull request. A formatting sweep and a behaviour change in the
  same diff is two pull requests.
- The pull request body states: what changed, what you ran, what you observed, and
  what you deliberately did not touch. "Tests pass" without the command and its
  output is not evidence.
- A pull request that changes anything in `docs/architecture.md` either quotes the
  decision it changes or adds an ADR first (below).

### The review rule

**Nothing merges without review by someone who did not write it.** No exceptions
for typo fixes to code, no self-merge, and no merging on the strength of a green
pipeline. The reviewer's job is to reproduce, not to trust: run the commands from
the pull request body, read the diff against the acceptance criteria, and answer
with either "done" or a numbered list of specific changes. A review that could
have been written by reading the diff alone has not been done.

Reviews of a document use the same rule. `docs/reviews/` holds one report per
reviewed document: what the reviewer ran, what they observed, and what they could
not check, marked as unverified rather than assumed either way.

### Commit messages

- Subject in the imperative, 72 characters or fewer: `worker: retry a failed
  download once`. Prefix with the area when it is not obvious from the diff.
- Body: why the change is needed, and what you tried that did not work if that is
  useful to the next person. Wrap at 72 columns.
- Reference the issue when there is one (`Fixes #12`). Never reference a private
  tracker.
- No secrets, no hostnames from a private network, no personal paths.

### Decisions: ADRs

A change to the project's *decisions* is recorded before the code, in
`docs/decisions/NNNN-title.md`, numbered from 1, with the next free number, using
the template in `docs/decisions/0000-template.md`. An ADR is required when a
change:

- renames a crate, a binary or a config key,
- changes the protocol grammar or the error code set,
- changes an item in `docs/architecture.md` sections 1, 2, 4, 6 or 9 (process
  model, protocol, config, security, resident-process rules),
- adds a third-party dependency that the architecture has to know about,
- raises the MSRV by more than two releases, or
- decides something the specs left open.

An ADR is not required for a bug fix, a test, a comment, or a change that only
makes existing behaviour match the documents. When in doubt: if a reviewer would
have to ask "why this way", write two paragraphs in an ADR and be done.

An ADR is never edited after it is accepted. It is superseded by a later ADR that
names it.

**`docs/decisions/0000-template.md`:**

```markdown
# NNNN. <short decision, imperative>

- **Status:** proposed | accepted | superseded by NNNN
- **Date:** YYYY-MM-DD
- **Deciders:** who agreed
- **Supersedes:** NNNN, or "nothing"

## Context

What forced the decision: the constraint, the measurement, the user report. Name
the document and section this decision touches. Numbers, not adjectives.

## Decision

The rule, in the imperative, in one or two sentences. If it can be read two ways,
it is not finished.

## Alternatives considered

Each with the reason it lost. "Not tried" is an acceptable reason; "it is
unpopular" is not.

## Consequences

What becomes easier, what becomes harder, what this now forbids, and what would
have to be true for this decision to be reversed. Name the test or check that
enforces it, if one exists.
```

### Secrets

Never commit a credential, a token, an API key, a `.env` file or an `auth.json`.
The configuration file stores a credential *by reference*: a keychain item, a
`secret-tool` lookup, a Credential Manager entry, or the name of an environment
variable (`docs/architecture.md` 6.3). A key in a config file is a key in every
backup of that file, and the architecture refuses to start rather than accept one.
No test in the repository may need a real credential: the Wallhaven tests run
against fixtures, or are marked as requiring a key and skipped when it is absent.

Run the scanner before you push. It is the same tool, the same config file and the
same range the `secrets` job scans: the commits your branch adds to `development`
(which is the branch a working branch is cut from, section 6).

```sh
git fetch origin development
gitleaks git --config .gitleaks.toml --redact \
  --log-opts="$(git merge-base origin/development HEAD)..HEAD"
```

`gitleaks` is not a build dependency; take it from the project's releases (CI pins
8.30.1, and the job verifies the release digest) or from your package manager. If
it prints `0 commits scanned` then it read nothing, its `no leaks found` means
nothing, and the cause is your git config: `color.ui` or `color.diff` of `always`
makes `git log -p` colourise a pipe, and gitleaks cannot read that. The CI job
pins both off for this reason, and the same two overrides work locally:

```sh
git fetch origin development
GIT_CONFIG_COUNT=2 GIT_CONFIG_KEY_0=color.ui GIT_CONFIG_VALUE_0=false \
GIT_CONFIG_KEY_1=color.diff GIT_CONFIG_VALUE_1=false \
gitleaks git --config .gitleaks.toml --redact \
  --log-opts="$(git merge-base origin/development HEAD)..HEAD"
```

An exemption from the scanner's rules lives in `.gitleaks.toml`, needs a
`description` saying why the hit is not a secret, and is printed by the `secrets`
job on every run. An exemption a reviewer cannot see is an unwritten rule, and the
next person widens it.

The scan reads the commits, not the tree, so removing a credential in a later
commit does not clear it: the commit that added it is still in the range and the
job stays red. That is the job working. Rotate the credential, then drop the
commit from the branch (`git rebase --interactive`, or `git reset`) and push the
rewritten branch.

## 7. Local development

### From a fresh clone to a running daemon

This is the sequence, verified end to end against the scaffold described in the
status section. No daemon installed, no wallpaper touched.

```sh
git clone https://github.com/guruor/whirl
cd whirl
cargo build --workspace

mkdir -p run
cat > run/config.json <<'JSON'
{
  "config_schema": 1,
  "backend": "noop",
  "sources": [
    { "id": "pictures", "kind": "local", "weight": 1, "paths": ["~/Pictures"] }
  ]
}
JSON

export WHIRL_CONFIG="$PWD/run/config.json"
export WHIRL_SOCKET="$PWD/run/whirl.sock"
export WHIRL_STATE_DIR="$PWD/run/state"
export WHIRL_CACHE_DIR="$PWD/run/cache"
export WHIRL_BACKEND=noop

WHIRL_BACKEND=noop cargo run -p whirld        # terminal 1: the daemon, in the foreground
cargo run -p whirl-cli -- status              # terminal 2
cargo run -p whirl-cli -- next
cargo run -p whirl-cli -- config check
```

Expected, and observed (abridged: the scaffold prints a subset of the key set,
and the full contract, which clients may rely on, is the table in
`docs/architecture.md` 2.10):

```
$ cargo run -p whirl-cli -- status
daemon_version: whirl 0.1.0
protocol: 2
platform: macos
pid: 53924
seq: 0
uptime_s: 0
rss_kb: 1872
paused: 0
rotating: 0
rotation_count: 0
interval_s: 1800
last_digest: -
last_origin_key: -
last_via: -
last_at: -
last_error: -
history_entries: 50
sources: 1
```

```
$ cargo run -p whirl-cli -- next
queued
set: 05fcfcea6e028590562d144725a514b136342ec76690ffedcd1795dd4b8832bf pictures:<sha256 of that path> source <path>
```

`<path>` is the scaffold's placeholder for the candidate it selected: the scaffold's
worker reports that name without downloading it, which is a shortcut the real noop
backend does not take (the real one runs every stage except the platform setter,
including the download and the cache write). A real worker prints the absolute path
it set. The second field is the `origin_key`, `<source id>:<source-scoped id>`
(`docs/architecture.md` 2.5): the source is `pictures`, the `id` the config above
gives it, and for a `local` source the source-scoped half is the sha256 of the
absolute path, so it is a placeholder here exactly as `<path>` and the digest are.
The shape is the part to check: the source `id` is the prefix, never the source
*kind*.

The `queued` line arrives before the work is done, on purpose: the control plane
never waits on a worker (`docs/architecture.md` 1.8), which is why `next` returns
in one round trip even while a rotation runs, and why the `set:` line is written
when it finishes.

Two notes on the sequence above:

- **The config is given explicitly, and so is everything else.** With no
  `WHIRL_CONFIG`, the daemon writes an annotated default config at the platform
  path and uses that; the explicit variables keep a developer's real config, real
  state and real cache untouched. `whirl config path` prints the path in use.
- **`WHIRL_BACKEND=noop` is set twice, which is not redundant.** The first is for
  the CLI's own environment, the second for the daemon's. Both are shown because
  copying a line out of a shell history is how this goes wrong.

### The five environment variables

Precedence is config file, then environment, then daemon flags
(`docs/architecture.md` 4.3). The flags exist for tests.

| variable | effect |
|---|---|
| `WHIRL_CONFIG` | path to the config file the daemon reads; with none, the default path is used and written if missing |
| `WHIRL_SOCKET` | the control socket path, overriding `socket` in the config |
| `WHIRL_STATE_DIR` | where `current.json`, `history.json`, `favorites.json` and the locks live |
| `WHIRL_CACHE_DIR` | where candidate images and `index.json` live |
| `WHIRL_BACKEND` | `native` or `noop`; `noop` runs every stage except the platform setter |

`WHIRL_WALLHAVEN_API_KEY` is read by the worker only when the daemon's own
environment sets it, and is never written to a file (`docs/architecture.md` 6.3).

### Rotating without touching your own wallpaper

**`WHIRL_BACKEND=noop`, or `whirld --backend noop`.** The noop backend runs the
whole rotation, meaning candidate selection, download, filters, cache write and
the `set:` report, and skips exactly one stage: the platform setter
(`docs/architecture.md` 4.2). Consequences worth knowing:

- `whirl next` is safe to run all day. It exercises everything but the two lines
  of platform code.
- `cargo test --workspace` and the CI `test` job set it, so a test cannot set a
  real wallpaper by accident.
- What noop does *not* cover is the platform setter itself. That is what the
  real-hardware checklists in section 3 are for, by design.

When you build a backend, make that split explicit in the code: the setter call
should be the only thing behind the backend boundary, so the noop path and the
native path differ in one function.

### Running the pieces by hand

The worker is a program, not a library. Its argv, its environment and its stdout
are a fixed contract (`docs/architecture.md` 1.6):
`whirl-worker --config <abs path> --verb <rotate|set|check> [--target <path|id>]
--run <rotation id>`. The daemon spawns it exactly like this, and you can too:

```sh
target/debug/whirl-worker --config "$WHIRL_CONFIG" --verb rotate --run 1
target/debug/whirl-worker --config "$WHIRL_CONFIG" --verb check --run 1
```

When it rotates, its stdout is at most two lines, `downloaded: <digest> <abs path>`
and `set: <digest> <origin_key> <abs path>`; the daemon reads the last non-empty
line as the result. Running it by hand is the fastest way to see which stage
failed, because stderr carries the failing stage and the exit code is non-zero
(`docs/architecture.md` 1.6). `--verb check` prints the source and plan lines that
`whirl config check` forwards, which is what the scaffold does; the architecture
fixes the rotate contract, not the check output.

The control socket is line-oriented text, so the daemon can be driven without the
CLI (verified with the `nc` that ships on macOS):

```sh
printf 'ping\n'   | nc -U "$WHIRL_SOCKET"
printf 'status\n' | nc -U "$WHIRL_SOCKET"
```

The daemon speaks first: every connection opens with
`OK whirl <semver> protocol <n>`, for example
`OK whirl 0.1.0 protocol 2` (`docs/architecture.md` 2.4), so read a line
before you write one. `ping` answers a bare `OK`; an unset scalar prints as `-`.

### Reproducing a prototype measurement

`prototype/` is outside the workspace, so `cargo` never builds it. It is built by
hand, in a scratch directory, with the two commands `prototype/README.md` gives
(lines 97-98): `cd wh-rotate && CGO_ENABLED=1 go build -o wh-rotate .` for the Go
part, and `rustc -O whd.rs -o whd && rustc -O whctl.rs -o whctl` for the two Rust
binaries. Do not build into the repository, and never build or run it while a
daemon is installed: the prototype's unconditional socket unlink is the bug the
architecture names in 1.5 step 5. Treat it as a read-only artefact you may run, not
a tool you may improve.

## 8. Good first issues

Each of these is small, self-contained, and does not require the whole design in
your head. All of them need the workspace scaffold to have landed.

1. **A test that the socket is `0600`, and that the directory above it is
   `0700`.** Assert the mode, do not trust the code to have set it
   (`docs/architecture.md` 2.1).
2. **A test for the greeting and version negotiation:** `hello 1` must fail with
   `ERR bad_protocol`, `hello 2` must succeed, and the greeting must carry
   `protocol 2` (`docs/architecture.md` 2.4).
3. **A test that every verb in the grammar is answered by a defined token.**
   Section 2.11 of the architecture did this by hand for 19 verbs; make it a test
   that fails when a verb is added and left unanswered (`docs/architecture.md`
   2.5, 2.6).
4. **Config validation messages that name the key and the line.** The parser has
   one error type and the message shape is specified; implement the unknown-key
   warning and an out-of-range refusal with the key and both values
   (`docs/architecture.md` 4.3).
5. **The `guards` job's dependency check, as a local script.** It is currently a
   `python3` heredoc inside the workflow; a `scripts/` entry would let a
   contributor run it before pushing and would let the workflow call one thing
   (this document, section 4).
6. **The Linux GNOME adapter, light and dark keys.** One `gsettings` call per
   key, the failure mode where the schema is absent, and the detection signal that
   is not "`gsettings` exists" (`docs/research/linux.md`, GNOME section 4 and
   "Detecting the environment"; `docs/spec/features.md` for the backend contract).
7. **A README quickstart that matches section 7 of this document,** including the
   `WHIRL_BACKEND=noop` line, so the front door tells the same story as the guide.

## Sources

- Apple, "Membership Details", `https://developer.apple.com/programs/whats-included/`
  (99 USD per membership year), retrieved 2026-09-25.
- Apple, "Notarizing macOS software before distribution", quoted in
  `docs/research/macos.md` section 8.
- Microsoft, "Code signing options for Windows app developers",
  `https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options`
  and "Azure Artifact Signing (formerly Trusted Signing)",
  `https://azure.microsoft.com/en-us/products/artifact-signing` (Basic plan
  9.99 USD per month, 5,000 signatures; SmartScreen reputation builds over time),
  retrieved 2026-09-25.
