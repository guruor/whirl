#!/usr/bin/env bash
# Whirl's one gate: one mode per check, and each mode is the command the
# matching job in .github/workflows/ci.yml runs, with the same flags. A local
# run and a CI run cannot drift apart, because there is one copy of every
# command (docs/development.md, "The gate" in section 3).
#
#   ./scripts/ci.sh fmt          cargo fmt --all -- --check
#   ./scripts/ci.sh clippy       cargo clippy --workspace --all-targets -- -D warnings
#   ./scripts/ci.sh test         the test suite, one process per test, then the doctests
#   ./scripts/ci.sh guards       no third-party dependencies, and the binary size caps
#   ./scripts/ci.sh artifacts    cargo build --workspace --release
#   ./scripts/ci.sh windows      cross-check the #[cfg(windows)] code (compile only)
#   ./scripts/ci.sh local        fmt clippy test artifacts, on this machine
#
# Nothing here needs sudo, a paid runner tier, or an account beyond this repo.
# The tooling this file needs (python3, cargo-nextest) is not a crate
# dependency: `guards` reads Cargo.lock, and none of it can appear there.
set -eu

root=$(cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

usage() {
  # The mode table above, printed from this file rather than repeated here.
  sed -n '/^#   \.\/scripts\/ci\.sh fmt/,/^#   \.\/scripts\/ci\.sh local/p' "$0" >&2
  exit 2
}

die() { printf 'ci.sh: %s\n' "$*" >&2; exit 2; }

need() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required for the '$2' mode"
}

fmt() { cargo fmt --all -- --check; }

clippy() { cargo clippy --workspace --all-targets -- -D warnings; }

test_suite() {
  need cargo-nextest test
  # A test that rotates must never reach a real desktop; CI sets this for the
  # whole job so that a new test cannot forget it, and so does this file.
  export WHIRL_BACKEND=noop
  # One process per test, and the whole workspace is built first, which is what
  # the integration tests need: they spawn the whirl-worker binary, and a
  # package-scoped build never produces it (a `cargo test -p whirld` run fails
  # for that reason, unrelated to the code under test). A test cannot inherit
  # another test's open descriptors either, which is the cross-test half of the
  # ETXTBSY class (t_7e9836df), and each test gets the timeout in
  # .config/nextest.toml instead of the runner's.
  # --no-tests=fail is the guard against a run that reports success having run
  # nothing, which is how a wrong package name hides: `-p whirl-worker` runs
  # none of the whirld worker tests at all.
  cargo nextest run --workspace --no-tests=fail
  # nextest does not run doctests: "Doctests are currently not supported
  # because of limitations in stable Rust. For now, run doctests in a separate
  # step with `cargo test --doc`." (https://nexte.st/, 2026-09-26)
  cargo test --workspace --doc
}

artifacts() { cargo build --workspace --release; }

windows() {
  # Compile only, and only for the target CI builds. `cargo check` does not
  # link, so this needs no Windows toolchain and no linker, and it catches
  # `#[cfg(windows)]` code that does not compile. It cannot run a test, so it
  # is not a substitute for test (windows-latest).
  cargo check --workspace --all-targets --target x86_64-pc-windows-msvc
}

guards() {
  need python3 guards
  cargo metadata --format-version 1 > /dev/null
  python3 - <<'PY'
import re
import sys

ours = {"whirl-core", "whirld", "whirl-worker", "whirl-cli"}
names = re.findall(r'^name = "([^"]+)"', open("Cargo.lock").read(), re.M)
extra = sorted(set(names) - ours)
if extra:
    sys.exit("third-party dependencies appeared: " + ", ".join(extra))
print(f"Cargo.lock holds {len(names)} entries, all ours: {sorted(set(names))}")
PY
  cargo build --workspace --release
  # The caps in docs/architecture.md R2. `CARGO_TARGET_DIR` is honoured rather
  # than assumed: the container mode sets it, and a hardcoded target/ would
  # make this check fail on a tree that has no target/ where it looked.
  target="${CARGO_TARGET_DIR:-target}"
  export CI_TARGET_DIR="$target"
  python3 - <<'PY'
import os
import sys

caps = {"whirld": 1.0, "whirl": 2.0, "whirl-worker": 12.0}
target = os.environ["CI_TARGET_DIR"]
suffixes = ["", ".exe"]
failed = []
for name, cap in caps.items():
    path = next(
        (f"{target}/release/{name}{s}" for s in suffixes
         if os.path.exists(f"{target}/release/{name}{s}")), None,
    )
    if path is None:
        failed.append(f"{name}: no release binary in {target}/release")
        continue
    size = os.path.getsize(path) / (1024 * 1024)
    print(f"{name}: {size:.2f} MiB (cap {cap:.2f} MiB)")
    if size > cap:
        failed.append(f"{name}: {size:.2f} MiB over the {cap:.2f} MiB cap")
if failed:
    sys.exit("; ".join(failed))
PY
}

on_this_machine() {
  fmt
  clippy
  test_suite
  artifacts
}

case "${1:-}" in
  fmt)       fmt ;;
  clippy)    clippy ;;
  test)      test_suite ;;
  guards)    guards ;;
  artifacts) artifacts ;;
  windows)   windows ;;
  local)     on_this_machine ;;
  *)         usage ;;
esac
