#!/usr/bin/env bash
# Whirl's one gate: one mode per check, and each mode is the command the
# matching job in .github/workflows/ci.yml runs, with the same flags, so there
# is one copy of every command and a local run and a CI run cannot drift apart
# (docs/development.md, "The gate" in section 3).
#
# One mode is deliberately not yet the command its job runs, and this comment is
# the whole of the difference. `test` here is `cargo nextest run --workspace
# --no-tests=fail` then `cargo test --workspace --doc`; the `test` job still
# inlines `cargo test --workspace`. Those are two harnesses, not two spellings of
# one: nextest runs one process per test, which is what the ETXTBSY class needs
# and which the shared-process harness cannot see; nextest does not run doctests,
# so the mode runs them separately; and a test that passes only while it inherits
# another test's state passes in CI and fails here. A green `test` here is
# therefore not evidence about the `test` job, and the other way round. The
# wiring pull request (card t_438e03b0; pull request #30, open and unmerged at
# this commit) points every job at these modes and installs the pinned
# cargo-nextest on the runners, and the identity then holds by construction.
#
#   ./scripts/ci.sh fmt          cargo fmt --all -- --check
#   ./scripts/ci.sh clippy       cargo clippy --workspace --all-targets -- -D warnings
#   ./scripts/ci.sh test         the test suite, one process per test, then the doctests
#   ./scripts/ci.sh msrv         the declared MSRV, 1.85.0: check and test
#   ./scripts/ci.sh guards       no third-party dependencies, and the binary size caps
#   ./scripts/ci.sh artifacts    cargo build --workspace --release
#   ./scripts/ci.sh windows      cross-check the #[cfg(windows)] code (compile only)
#   ./scripts/ci.sh local        fmt clippy test artifacts, on this machine
#   ./scripts/ci.sh linux        the ubuntu jobs in the gate's container, this machine's arch
#   ./scripts/ci.sh linux-amd64  the same, pinned to the runners' x86_64, emulated on Apple silicon
#   ./scripts/ci.sh all          local, windows, msrv, then linux
#
# Nothing here needs sudo, a paid runner tier, or an account beyond this repo.
# The tooling this file needs (python3, Docker, cargo-nextest, rustup) is not a
# crate dependency: `guards` reads Cargo.lock, and none of it can appear there.
set -eu

root=$(cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

die() { printf 'ci.sh: %s\n' "$*" >&2; exit 2; }

need() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required for the '$2' mode"
}

# The MSRV here and `rust-version` in Cargo.toml are the same number; the
# `msrv` job is what makes it a fact rather than a hope (docs/development.md 2).
MSRV="1.85.0"

# The gate's Linux environment is one image, built from scripts/gate.Dockerfile:
# the pinned toolchain plus the pinned cargo-nextest, so the container can run
# the same `test` mode CI runs. Building it rather than using the stock rust
# image is not optional: the stock image has no cargo-nextest, so a container
# mode built on it could not run the test suite at all.
# The tag names both pins, so a change to either builds a new image instead of
# reusing the old one. Both are read from their single homes (rust-toolchain.toml
# and .config/nextest.toml; docs/development.md section 4 is the rule) rather
# than restated here.
GATE_DOCKERFILE="scripts/gate.Dockerfile"
GATE_TOOLCHAIN="$(sed -n 's/^channel = "\([^"]*\)"$/\1/p' rust-toolchain.toml)"
GATE_NEXTEST="$(sed -n 's/^nextest-version = .*"\([^"]*\)".*$/\1/p' .config/nextest.toml)"
if [ -z "$GATE_TOOLCHAIN" ] || [ -z "$GATE_NEXTEST" ]; then
  die "cannot read the toolchain channel from rust-toolchain.toml or the nextest version from .config/nextest.toml"
fi
GATE_IMAGE_BASE="whirl-gate:${GATE_TOOLCHAIN}-${GATE_NEXTEST}"

# `linux` runs the container on this machine's own architecture, and pins it
# explicitly. A bare `docker run rust:1.94.0-bookworm` resolves to whichever
# manifest is in the local store, and on this machine that has been both: the
# same command printed `x86_64` with a platform warning at one point and
# `aarch64` at another. Pinning makes the run deterministic. An Intel Mac gets
# the runners' x86_64 from `linux` already; an Apple silicon Mac gets arm64,
# which is the fast and green run.
case "$(uname -m)" in
  arm64 | aarch64) LOCAL_PLATFORM="linux/arm64" ;;
  *)               LOCAL_PLATFORM="linux/amd64" ;;
esac

# `linux-amd64` pins the runners' x86_64. On Apple silicon that means emulation,
# which costs wall clock and which cannot pass two of the tests, so it is an
# opt-in mode rather than part of `local` or `all`. It is the mode to run when a
# change touches something architecture decides: pointer width, atomics, SIMD,
# endianness, or anything that assumes `usize` is 8 bytes.
LINUX_PLATFORM="linux/amd64"

# Three tests fail under linux/amd64 emulation and pass on real x86_64,
# deterministically over repeats, with the daemon child logging nothing:
#
#   whirld::control_socket a_failed_rotation_is_visible_on_both_planes
#   whirld::control_socket subscribe_streams_one_event_per_state_change
#       both "the daemon closed the connection early"
#   whirld::bin/whirld worker::tests::a_spawn_that_finds_the_script_busy_is_retried
#       "a script that is busy for 100 ms is not a failed spawn:
#        Err(Failed { code: WorkerFailed, message: 'the worker exited non-zero
#        with no message' })"
#
# Measured 2026-09-26 on this arm64 host, three runs, the same three every time:
#
#   ./scripts/ci.sh linux-amd64
#   -> exit 100, 155 tests run (with .config/nextest.toml's fail-fast = false),
#      152 passed, 3 failed
#
# while all three pass in the same image without --platform (arm64), on macOS,
# and in CI's own jobs on real x86_64 (run 36222311613: ubuntu-latest,
# macos-latest and msrv all report them ok). t_62920980 owns the diagnosis and
# the fix for the two control_socket tests; the worker test arrived with
# t_7e9836df's new test, after that card measured "exactly two". Until those
# land, this mode exits non-zero on a clean tree and those three failures are
# expected: nothing is filtered out and nothing is skipped, and the mode prints
# the names, the reason and the card on every run, so a red is understood rather
# than ignored.

usage() {
  # The mode table above, printed from this file rather than repeated here.
  sed -n '/^#   \.\/scripts\/ci\.sh fmt/,/^#   \.\/scripts\/ci\.sh all/p' "$0" >&2
  exit 2
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

msrv() {
  # Exactly what the `msrv` job does, in the same order, including the
  # WHIRL_BACKEND the job sets on its test step. rustup and the toolchain are
  # the design's choice over a rust:1.85.0 container: the job's own command is
  # `cargo +1.85.0`, the failure class is a compiler-version difference so the
  # architecture does not matter, and this works with the daemon stopped.
  need rustup msrv
  if ! rustup toolchain list | grep -q "^${MSRV}-"; then
    die "the $MSRV toolchain is not installed: rustup toolchain install $MSRV --profile minimal"
  fi
  cargo "+$MSRV" check --workspace --all-targets
  WHIRL_BACKEND=noop cargo "+$MSRV" test --workspace
}

artifacts() { cargo build --workspace --release; }

windows() {
  # Compile only, and only for the target CI builds. `cargo check` does not
  # link, so this needs no Windows toolchain and no linker, and it catches
  # `#[cfg(windows)]` code that does not compile. It cannot run a test, which is
  # why `test (windows-latest)` stays the only thing that runs Windows code.
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

# One container image per platform, and one target volume per platform: a shared
# volume means every switch between arm64 and amd64 invalidates the fingerprints
# and rebuilds the workspace from scratch, which is the cost the volume exists
# to avoid.
gate_image() { printf '%s-%s\n' "$GATE_IMAGE_BASE" "${1##*/}"; }

target_volume() { printf 'whirl-gate-target-%s\n' "${1##*/}"; }

ensure_gate_image() {
  local platform="$1" image
  image="$(gate_image "$platform")"
  if docker image inspect "$image" >/dev/null 2>&1; then return 0; fi
  printf 'ci.sh: building %s for %s from %s\n' "$image" "$platform" "$GATE_DOCKERFILE" >&2
  docker build --platform "$platform" -t "$image" -f "$GATE_DOCKERFILE" .
}

in_the_container() {
  # $1: the platform to pin. $2: the mode to run there.
  local platform="$1" mode="$2"
  need docker "$mode"
  ensure_gate_image "$platform"
  docker run --rm --platform "$platform" \
    -v "$root:/w" -w /w \
    -v "$(target_volume "$platform"):/tmp/target" \
    -e CARGO_TARGET_DIR=/tmp/target \
    "$(gate_image "$platform")" \
    bash ./scripts/ci.sh "$mode"
}

amd64_note() {
  printf 'ci.sh: this run is emulated x86_64, and three tests are expected to fail in it:\n' >&2
  printf '          whirld::control_socket a_failed_rotation_is_visible_on_both_planes\n' >&2
  printf '          whirld::control_socket subscribe_streams_one_event_per_state_change\n' >&2
  printf '          whirld::bin/whirld worker::tests::a_spawn_that_finds_the_script_busy_is_retried\n' >&2
  printf '        all three pass on real x86_64 (CI run 36222311613) and natively here; card t_62920980 owns the\n' >&2
  printf '        two control_socket ones, and the worker one came with t_7e9836df after that card measured\n' >&2
  printf '        "exactly two". Nothing is skipped: the run is the whole suite and it exits non-zero until\n' >&2
  printf '        those land, which is why this mode is opt-in and not part of local or all.\n' >&2
}

on_the_runner() {
  clippy
  test_suite
  artifacts
  guards
}

everything() {
  on_this_machine
  # windows is a sub-second compile-only check and it catches #[cfg(windows)]
  # code that nothing else on a Unix host compiles; msrv is the failure class
  # that reaches CI last and costs the most. Neither is left out of the one
  # command a worker is expected to run before pushing.
  windows
  msrv
  in_the_container "$LOCAL_PLATFORM" ubuntu
}

case "${1:-}" in
  fmt)       fmt ;;
  clippy)    clippy ;;
  test)      test_suite ;;
  msrv)      msrv ;;
  guards)    guards ;;
  artifacts) artifacts ;;
  windows)   windows ;;
  local)     on_this_machine ;;
  linux)       in_the_container "$LOCAL_PLATFORM" ubuntu ;;
  linux-amd64) amd64_note; in_the_container "$LINUX_PLATFORM" ubuntu ;;
  ubuntu)    on_the_runner ;;   # the mode the container calls; not for people
  all)       everything ;;
  *)         usage ;;
esac
