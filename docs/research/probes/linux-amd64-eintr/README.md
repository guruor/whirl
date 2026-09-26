# `linux/amd64`: the `EINTR` that closed a subscriber's connection

Reproduction recipes for the two reds this document rules on, and for the ruling
itself. They were tracked as `t_62920980`; the two tests are `control_socket`'s
`a_failed_rotation_is_visible_on_both_planes` and
`subscribe_streams_one_event_per_state_change`.

`probe2.rs` and `probe3.rs` next to this file are the isolated probes. Each is a
single file with no dependencies: `rustc -o probe2 probe2.rs`, then run it. They
write nothing outside their own process and a `sleep` child.

## 1. The reproduction

Image `rust:1.94.0-bookworm`, run under `--platform linux/amd64` on Apple
silicon, so through Docker Desktop's amd64 translation. OCI index digest
(verified with `docker buildx imagetools inspect`):

    sha256:365468470075493dc4583f47387001854321c5a8583ea9604b297e67f01c5a4f

The `linux/amd64` manifest inside that index is
`sha256:4673f78db88b71f09d5451bbc404734807918161241215ba0a50bbbe9b448117` and the
`linux/arm64` one is
`sha256:94aaa0b45f4d185294474343d9034f829969f6c9ff8101f348b526d105860818`.

Pin the digest **or** the tag with `--platform`, never both: once the arm64 image
for that index is in the store, `docker run --platform linux/amd64
rust:1.94.0-bookworm@sha256:<index>` is refused with `docker: cannot overwrite
digest sha256:<index>`. The command below uses the tag and lets `--platform`
choose the manifest, and `uname -m` inside the container says which one ran.

```sh
cd <checkout>
docker run --rm --platform linux/amd64 \
  -v "$PWD":/w -w /w \
  -v whirl-ci-linux-target:/tmp/target -e CARGO_TARGET_DIR=/tmp/target \
  rust:1.94.0-bookworm \
  bash -c 'uname -m; rustc -V; WHIRL_BACKEND=noop cargo test --workspace'
```

Observed (2026-09-26, at `7e22e1a`, before the fix in item 4):

    uname -m: x86_64
    rustc 1.94.0 (4a4ef493e 2026-03-02)
    running 19 tests
    test a_failed_rotation_is_visible_on_both_planes ... FAILED
    test subscribe_streams_one_event_per_state_change ... FAILED

    ---- a_failed_rotation_is_visible_on_both_planes stdout ----
    thread 'a_failed_rotation_is_visible_on_both_planes' (1543) panicked at crates/whirld/tests/control_socket.rs:911:5:
    the daemon closed the connection early

    ---- subscribe_streams_one_event_per_state_change stdout ----
    thread 'subscribe_streams_one_event_per_state_change' (1619) panicked at crates/whirld/tests/control_socket.rs:911:5:
    the daemon closed the connection early

    test result: FAILED. 17 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.82s
    error: test failed, to rerun pass `-p whirld --test control_socket`

Line 911 is `read_line`'s `assert!(read > 0, ...)`, so it is the client reading
EOF. Which read it was is in the backtrace:

```sh
docker run --rm --platform linux/amd64 -v "$PWD":/w -w /w \
  -v whirl-ci-linux-target:/tmp/target -e CARGO_TARGET_DIR=/tmp/target \
  rust:1.94.0-bookworm \
  bash -c 'cd /w && RUST_BACKTRACE=1 WHIRL_BACKEND=noop \
    cargo test -p whirld --test control_socket -- --nocapture --test-threads=1 \
    subscribe_streams_one_event_per_state_change'
```

    thread 'subscribe_streams_one_event_per_state_change' (362) panicked at crates/whirld/tests/control_socket.rs:911:5:
    the daemon closed the connection early
    stack backtrace:
       2: control_socket::read_line
                at ./tests/control_socket.rs:911:5
       3: control_socket::subscribe_streams_one_event_per_state_change
                at ./tests/control_socket.rs:1100:9

`control_socket.rs:1100` is the first `read_line` after `daemon.ask("next")`
returned `OK`: the subscriber had already been disconnected while the rotation
ran, so it never saw `event: 1 rotate_start 1`.

### Where the daemon's message actually goes: a deleted `daemon.log`

The log is not empty. `spawn_daemon` sends the daemon's stderr to
`dir.join("daemon.log")`, and `Daemon::drop` then deletes that whole directory
with `std::fs::remove_dir_all`, so by the time the test has panicked the file is
gone. Snapshot `$TMPDIR/whirl-t*` while the suite runs and the daemon's own
account of the failure is one line:

    whirld: wrote the default config at /tmp/whirl-t359-af77ada0/config.json
    whirld: config /tmp/whirl-t359-af77ada0/config.json
    whirld: state /tmp/whirl-t359-af77ada0/state cache /tmp/whirl-t359-af77ada0/cache
    whirld: backend noop
    whirld: listening on /tmp/whirl-t359-af77ada0/run/whirl.sock
    whirld: connection ended: Interrupted system call (os error 4)

`os error 4` is `EINTR`. The line comes from `socket::serve`'s thread body,
which logs every `handle` error that is not `BrokenPipe` or `ConnectionReset`:
the connection thread did not fail to read a request, it was *interrupted*, and
`socket.rs` treated that as the end of the connection.

Recipe, run inside the same container: keep the last copy of every
`/tmp/whirl-t*` directory in a loop with a few milliseconds of sleep, run the
test, then read the preserved `daemon.log`. The whole directory is smaller than
16 KiB, so the copy is cheap; nothing else is needed, and no test file has to be
touched.

## 2. The native counterparts

The same image tag without `--platform`, so on this machine the arm64 guest, and
the same command on macOS. Same checkout, same commit as section 1.

```sh
docker run --rm --platform linux/arm64 \
  -v "$PWD":/w -w /w \
  -v whirl-ci-arm-target:/tmp/target -e CARGO_TARGET_DIR=/tmp/target \
  rust:1.94.0-bookworm \
  bash -c 'uname -m; rustc -V; WHIRL_BACKEND=noop cargo test --workspace'
```

    uname -m: aarch64
    rustc 1.94.0 (4a4ef493e 2026-03-02)
    running 4 tests   ... test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
    running 43 tests  ... test result: ok. 43 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
    running 6 tests   ... test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
    running 36 tests  ... test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.01s
    running 19 tests  ... test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.51s
    running 3 tests   ... test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
    test a_failed_rotation_is_visible_on_both_planes ... ok
    test subscribe_streams_one_event_per_state_change ... ok
    cargo test exit=0

```sh
cd <checkout>
WHIRL_BACKEND=noop cargo test --workspace   # macOS 26.5.2, arm64, rustc 1.94.0
```

    running 4 tests   ... test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.94s
    running 43 tests  ... test result: ok. 43 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
    running 6 tests   ... test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.94s
    running 36 tests  ... test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.07s
    running 19 tests  ... test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.12s
    running 3 tests   ... test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
    test a_failed_rotation_is_visible_on_both_planes ... ok
    test subscribe_streams_one_event_per_state_change ... ok
    cargo test exit=0

One commit, the same two tests, three environments: red only in the translated
amd64 guest.

## 3. Does CI run these two tests on x86_64? Yes, and it is green

The workflow says so. `.github/workflows/ci.yml:73-77` is the `test` job,
`runs-on: ${{ matrix.os }}` with `os: [ubuntu-latest, macos-latest,
windows-latest]`, and line 92 is its only test step:

    - name: cargo test --workspace
      run: cargo test --workspace

Both tests are plain `#[test]`s in `crates/whirld/tests/control_socket.rs`
(`subscribe_streams_one_event_per_state_change` at 1059,
`a_failed_rotation_is_visible_on_both_planes` at 1160): no `#[ignore]`, no
`#[cfg]`, so that step runs them, and `ubuntu-latest` is x86_64.

A recent green run says the same. `gh run list` picks run `36221809231`:
workflow `ci`, event `push` on `development`, head
`91498bbadb0c086142116d0de95098f5847b8bd5`, conclusion `success`, all thirteen
jobs `success`.

```sh
gh run view 36221809231 -R guruor/whirl --log > run.log
grep "^test (ubuntu-latest)" run.log
```

    1698: test (ubuntu-latest)  Install the pinned toolchain ...  Default host: x86_64-unknown-linux-gnu
    1701: test (ubuntu-latest)  Install the pinned toolchain ...  info: syncing channel updates for 1.94.0-x86_64-unknown-linux-gnu
    1937: test (ubuntu-latest)  cargo test --workspace  ...  test a_failed_rotation_is_visible_on_both_planes ... ok
    1944: test (ubuntu-latest)  cargo test --workspace  ...  test subscribe_streams_one_event_per_state_change ... ok

The job that runs them is x86_64, it runs both of them, and it is green. So
there is no CI hole: the platform is not what is wrong, the translation is.
