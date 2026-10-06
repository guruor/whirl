# AGENTS.md

The conventions an agent holds before it changes anything here. Each one has
already cost this project real work. Where a rule is already in
[CONTRIBUTING.md](CONTRIBUTING.md) or [README.md](README.md), this file cites it
rather than restating it, and `docs/README.md` is the reading order for the rest.

Every rule below is a rule. Where a rule is checkable by a command, the command is
named.

## 1. Record the change where the work is tracked

- A change to a machine rather than to this repository is recorded exactly as a
  commit is: which file, the value it had, the value it now has, and why. A config
  file, a state directory, a supervisor unit, the desktop and a shell file are all
  in that class.
- Take a backup of the old value outside the repository before you change the
  machine, and re-run the validator that covers the change.
- The record goes where the work is tracked: the pull request or commit body when
  the change is part of one, and a comment on the pull request or issue that tracks
  it when it is not. A change whose only record is the session that made it is not
  recorded, because the next reader gets the machine and not the session.
- Being right is not the same as being recorded. A change made at the user's
  request, with a backup taken and the validator re-run, still has to say so where a
  later reader can find it without a session database.
- This repository holds one instance of the rule already: the real-wallpaper
  procedure ends with the store line and the image the desktop was left on going
  into the pull request or commit body (`docs/development.md`, "When a check needs a
  real set"). The rest of the machine gets the same treatment or the change is not
  finished.

## 2. A person's own data never enters the repository

- A fixture that comes from a person's own account (a collection, a dataset, a
  private URL) lives only in that person's local config: never in the repository,
  never in `docs/`, never a CI fixture, and no test may reach the network for it.
- Reach it through the environment, the way the sandbox recipe does: the collection
  is copied into a scratch directory, and the one value that differs between
  machines is `WALLS` (`docs/development.md` section 7, "Installing whirl on your
  own machine").
- A credential is the same class of thing: the config holds a reference to the
  platform's own store and never a value (`docs/architecture.md` 6.3), and no file
  in the repository carries one. CONTRIBUTING.md and `docs/development.md` section 6
  own that rule and the scanner that enforces it.

## 3. The change the task asks for, and nothing else

- Implement what the task asks. No drive-by refactors, no unrequested files, no
  reformatting outside the change: that is already a rule in CONTRIBUTING.md,
  "Branches, commits, scope".
- If the task is ambiguous, or its acceptance test is missing, say so in the pull
  request and stop rather than choosing for the author.
- A change that has to reach outside the worktree is still inside the change: keep
  it to what the task names, and record it under rule 1.

## 4. A second party, and evidence over claims

- The default branch is the integration branch: a change lands by pull request
  reviewed by someone who did not write it, and a release is a tag. Nothing lands
  unreviewed. A claim in a commit message, a pull request body or a comment is
  input rather than evidence; evidence is the command and its output, pasted
  (CONTRIBUTING.md, "The rules that are not negotiable"; `docs/development.md`
  section 6, "The review rule").
- Provenance in a comment is the measurement and the command that produced it,
  pasted, not a pointer to whatever tracked the work: a reader with the repository
  and not the session can only check what was printed.
- Do not review your own work. When you are the only party, open the pull request
  and say what you could not check.
- Say what you could not verify as unverified: `docs/reviews/` holds reports written
  that way, and `docs/architecture.md` section 12 is the map for a reviewer of that
  document.

## 5. Where the truth lives

- Behaviour: `docs/spec/`.
- Decisions and constraints: `docs/architecture.md`.
- A change to a decision: an ADR in `docs/decisions/`, before the code. When one is
  required is `docs/development.md` section 6; CONTRIBUTING.md states the rule.
- Build, test and ship: `docs/development.md`. What is real rather than planned is
  its status table at the top, and `docs/README.md`'s; do not restate either.
- The documents are the specification the code is written against. If the code and a
  document disagree, say so in the pull request rather than editing one to match the
  other (CONTRIBUTING.md, "Documentation changes").

## 6. Build and check through the gate

- One script, one mode per check: `./scripts/ci.sh <mode>`, each mode the command
  the matching CI job runs, so a local run and CI cannot drift. A check that is not
  a mode of that script is a preference, not a gate (CONTRIBUTING.md, "Before you
  open a pull request"; `docs/development.md` section 3 owns the mode table, the
  prerequisites and what `all` leaves unproven).
- While you iterate, `./scripts/ci.sh local` is fmt, clippy, test and artifacts
  without the container, the Windows cross-check and the MSRV.
- Before you push, `./scripts/ci.sh all`: `local`, then `windows`, `msrv`, then
  `linux`. The container suite runs as the user who invoked the gate, so run it as
  that user: as root, one test fails on a clean tree
  (`crates/whirl-worker/src/sources/local.rs` plants a mode-`0o000` file and asserts
  that reading it fails). Any other red from `all` is real
  (`docs/development.md` section 3, "The gate's container, and what it leaves
  behind").
- `cargo --version` must print the channel `rust-toolchain.toml` pins before a fmt
  or clippy result means anything: a toolchain manager that exports
  `RUSTUP_TOOLCHAIN` overrides that file, and a clippy newer than the pin then fails
  the gate on lint that the pin accepts. To make the pin win for one command, unset
  `RUSTUP_TOOLCHAIN` and put rustup's own `bin` directory (`$HOME/.cargo/bin`)
  first on `PATH` (`docs/development.md` section 2, "The toolchain").
- No test may set a real wallpaper: the gate exports `WHIRL_BACKEND=noop` for the
  whole of `test`, and nothing may undo it (`docs/development.md` section 7,
  "Rotating without touching your own wallpaper").
