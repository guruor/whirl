# Contributing to whirl

Thanks for looking. This file is the short version. `docs/development.md` is the
long version and is the one to read if you are going to change code: repo layout,
toolchain and dependency policy, the test matrix, the release process, and how to
run a rotation without touching your own wallpaper.

## Before you open a pull request

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
WHIRL_BACKEND=noop cargo test --workspace
cargo build --workspace --release
```

These are exactly the commands CI runs. If a check is not in
`.github/workflows/ci.yml`, it is a preference rather than a gate, so adding a
check means adding it in both places in the same pull request.

Put the commands you ran and what you observed in the pull request body. "Tests
pass" without the output is not evidence, and a reviewer who cannot reproduce your
result will send it back.

## The rules that are not negotiable

- **Nothing merges without review by someone who did not write it.** Not a typo
  fix, not a one-line change, not on the strength of a green pipeline. If you
  wrote it, you do not approve it.
- **v0.1 has zero third-party dependencies.** A new dependency is a pull request
  that argues its case: which crate, what it replaces, the binary size change it
  causes, and confirmation that it is not a GUI toolkit, an async runtime, or an
  image decoder. `docs/development.md` section 2 has the full rule.
- **The daemon never owns pixels and never links a platform backend.** That is the
  whole design (`README.md`, `docs/architecture.md` section 9). A pull request
  that moves image work into `whirld`, or adds a platform call outside
  `crates/whirl-worker/src/backend/`, is rejected on sight.
- **Nothing that sets a real wallpaper runs in a test.** Use the noop backend:
  `WHIRL_BACKEND=noop`, or `--backend noop`.
- **Never commit a credential.** Not a key, not a token, not a `.env`, not an
  `auth.json`. The config stores credentials by reference to the platform's own
  store (`docs/architecture.md` 6.3). No test may require a real key, and
  `gitleaks git --config .gitleaks.toml --redact --log-opts="$(git merge-base
  origin/main HEAD)..HEAD"` reproduces the `secrets` job locally (docs/development.md
  section 6 has the details, including the one git setting that makes the scan read
  nothing). An exemption in `.gitleaks.toml` needs a written reason and is printed
  on every CI run.
- **A decision change gets an ADR first.** `docs/decisions/NNNN-title.md` from
  `docs/decisions/0000-template.md`. When it is required is in
  `docs/development.md` section 6; when in doubt, write two paragraphs and be
  done.

## Branches, commits, scope

- One branch per change, from `main`, named `<area>/<short-topic>`, for example
  `daemon/stale-socket-probe` or `worker/wallhaven-pagination`. Delete it after
  it merges.
- One change per pull request. A formatting sweep and a behaviour change are two
  pull requests.
- Commit subject in the imperative, 72 characters or fewer, area prefix when it
  helps: `worker: retry a failed download once`. The body says why, and what you
  tried that did not work. Wrap at 72 columns.
- Keep unrelated cleanups out. A diff that mixes them cannot be reviewed properly,
  and that costs more than the cleanup saves.
- Work in progress is fine: open the pull request as a draft and say what you are
  unsure about. That is faster than being stuck alone.

## Documentation changes

The documents in `docs/` are the specification the code is written against, so a
change to one is a change to the contract, reviewed the same way. If the code and
a document disagree, say so in the pull request rather than quietly editing one to
match the other. `docs/research/` and `docs/spec/` record findings from sources
outside this repository: an edit there needs the evidence, not a preference.

## Where things are

- `docs/architecture.md` decides the process model, the protocol, the config and
  the platform verdict. Read section 1 and section 2 before writing code.
- `docs/spec/` is the feature set and the state and cache layout.
- `docs/research/` is what each platform actually does, including the
  real-hardware checklists a release has to pass.
- `docs/development.md` is how to build, test, ship and change all of it.
- `prototype/` is the throwaway spike that measured the design. Read it, build it
  in a scratch directory if you want to reproduce a number, and do not add
  features to it.

## Licence

MIT. By contributing you agree your work is licensed under it, as in `LICENSE`.
