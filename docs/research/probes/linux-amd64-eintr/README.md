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

## 4. The ruling, and the mechanism

**Ruling.** An emulation artifact that exposed a real defect. The translation is
what delivers the signal; the defect is that the daemon treated a retryable
`EINTR` as the end of a live connection. So the fix belongs in the daemon, and
the emulated mode then passes without an exclusion.

1. Every connection sets `SO_RCVTIMEO`: `handle` sets 2.8's 300 s idle timeout,
   and `subscribe` replaces it with `STREAM_POLL` (100 ms). signal(7) lists a
   socket read with a timeout in the interfaces that are **never** restarted
   after a signal handler returns: it fails with `EINTR` instead. In this
   daemon, then, any signal that reaches a thread parked in
   `read_request_line` does not restart the read, it surfaces as `Interrupted`.
2. Under amd64 translation, forking a child delivers such a signal. `probe2.rs`
   parks a thread in exactly that read (`SO_RCVTIMEO` = 100 ms, looping on
   `WouldBlock` as `subscribe` does) and spawns and reaps a child from another
   thread, as `Worker::run` does when it forks `whirl-worker`:

       linux/amd64, two runs:  reader: fill_buf -> Interrupted (raw_os_error=Some(4)) at 324ms / 331ms
                               result: interrupted=1 timed_out_polls=13
       linux/arm64, two runs:  result: interrupted=0 timed_out_polls=14
       macOS,       two runs:  result: interrupted=0 timed_out_polls=15 / 16

   `probe3.rs` moves the spawn from 300 ms to 1200 ms and the interruption moves
   with it, 320 ms → 1228 ms, so what interrupts the read is the fork and not a
   clock. Drop the `set_read_timeout` line from either probe and the read is
   restarted on all three platforms: the timeout is the ingredient that turns
   the signal into an error.
3. `read_request_line` propagated that error (`reader.fill_buf()?`), `handle`
   returned it, and `serve` logged `whirld: connection ended: Interrupted system
   call` and dropped the socket. The subscriber's next read got EOF, which is
   the assertion at `control_socket.rs:911`.
4. The two tests are exactly the ones that hold a subscribed connection parked
   in that read while a rotation forks the worker. That is why they and not
   others are red, and why the red is deterministic rather than flaky.

The emulator is not qemu. `/proc/cpuinfo` inside the amd64 container says
`model name: VirtualApple @ 2.50GHz`, which is Rosetta 2 (Docker Desktop's
Rosetta translation). The translated guest also catches one signal a native
guest does not: `SigCgt: 0000000000000450` against `0000000000000440`, bit 4 =
signal 5, `SIGTRAP`. Which signal interrupts the read is **not** identified:
ptrace across Rosetta produced an unusable trace (`syscall_0x...` lines instead
of syscall names), and blocking `SIGTRAP` in the parked thread did not change
the outcome. Ruling and fix do not need its name: `EINTR` is retryable whatever
raised it.

### The fix

`crates/whirld/src/socket.rs`'s `read_request_line` retries
`ErrorKind::Interrupted` on `fill_buf` instead of returning it, with the reason
and this card's id in the comment above it. Two unit tests pin the behaviour:
`socket::tests::an_interrupted_read_retries_instead_of_ending_the_connection`
reads the line behind the interruption, and
`socket::tests::a_read_error_that_is_not_an_interruption_is_still_reported`
shows the retry cannot swallow a real failure.

Which platform's behaviour changed: none. On real x86_64 (CI) and natively the
read is never interrupted, so nothing observable changes there. What changed is
that an interrupted read no longer ends a live connection: the connection
survives a signal, which is what a system call that transferred nothing
requires.

After the fix, in the same emulated container, `cargo test --workspace`:

    running 38 tests  ... test result: ok. 38 passed; 0 failed; ... finished in 8.12s
    running 19 tests  ... test result: ok. 19 passed; 0 failed; ... finished in 1.85s
    test a_failed_rotation_is_visible_on_both_planes ... ok
    test subscribe_streams_one_event_per_state_change ... ok
    cargo test exit=0
