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

## 5. What the gate should say now

`scripts/ci.sh` had not landed in the repository when this was written, so this
card has no gate file to change. With the cause fixed there is no exclusion to
add for the two tests this card rules on: the mode runs them and they pass
(section 6). The mode was not green as a whole when this was written, because a
third emulation red arrived with `development` after this card's base; that red
is closed too (section 6), so the amd64 mode carries no exclusion, skip or
known-red label for any test. Two things the gate's text must not inherit from
the design it comes from:

1. `design.md` and the proposed `ci.sh` record "under emulation `cargo test
   --workspace` is red on two tests ... That is why the pin is not the
   default". The measurement was right; the cause is now fixed, so the amd64
   mode must not carry an exclusion, a skip or a known-red label for these two
   tests. They pass, and a red there is a real finding again.

2. Pin **one platform manifest digest per mode**, not the index digest together
   with `--platform`. Measured on this machine: once the store holds the arm64
   image for the index, `docker run --platform linux/amd64
   rust:1.94.0-bookworm@sha256:<index>` is refused with `docker: cannot
   overwrite digest sha256:<index>`, so the gate's second mode cannot start
   after its first one has run. The three digests are in section 1: index
   `sha256:36546847...`, `linux/amd64` `sha256:4673f78d...`, `linux/arm64`
   `sha256:94aaa0b4...`.

## 6. Re-verification on the merged tree, the third red it turned up, and its resolution

`development` moved 37 commits while this was being written, so the branch
merges it (`git merge origin/development`, no conflicts) and every measurement
above is repeated on the merged tree, `da555be`. The runs quoted below are from
that tree unless they say otherwise. The claims about the present state were
re-taken at this head, `0cd385c` (this branch with `development` at `399da9c`
merged in, and the doc-only commit this section's correction arrives in changes
nothing but this file); the third-red bullet also records what closed that red.

* The two tests this document rules on are green under emulation, then and now.
  In the emulated container, `cargo test --workspace --no-fail-fast` at
  `da555be`:

      test result: ok. 0 passed ... test result: ok. 4 passed ... test result: ok. 47 passed
      test result: ok. 21 passed ... test result: ok. 7 passed
      test result: FAILED. 52 passed; 1 failed
      test result: ok. 22 passed; 0 failed   <- control_socket
      test result: ok. 3 passed ... test result: ok. 0 passed
      test a_failed_rotation_is_visible_on_both_planes ... ok
      test subscribe_streams_one_event_per_state_change ... ok

  The one red there is the third red below, in `whirld`'s own unit binary. The
  same command at this head, `0cd385c`:

      test result: ok. 0 passed ... test result: ok. 4 passed ... test result: ok. 47 passed
      test result: ok. 21 passed ... test result: ok. 7 passed
      test result: ok. 54 passed; 0 failed   <- the binary the third red was in
      test result: ok. 22 passed; 0 failed   <- control_socket
      test result: ok. 3 passed ... test result: ok. 0 passed
      test a_failed_rotation_is_visible_on_both_planes ... ok
      test subscribe_streams_one_event_per_state_change ... ok
      cargo test exit=0

  That binary holds 53 tests at `da555be` and 54 at this head; the one added test
  is the kernel-free half of the third red's fix below.

* Natively the same merged tree is green, and re-checked at this head: `cargo
  test --workspace` exit 0 on macOS arm64 (0, 4, 47, 21, 7, 54, 22, 3 and 0
  passed, the emulated run's own counts), `cargo fmt --all -- --check` clean,
  `cargo clippy --workspace --all-targets -- -D warnings` clean.
* Line numbers moved with the merge, and did not move after it: the `read_line`
  helper whose empty read panics is at `control_socket.rs:1081` (911 in section
  1's run at `7e22e1a`), and the two tests are at 1230 and 1349 (1059 and 1160 at
  `7e22e1a`). Checked at this head, and still those. Section 3's `ci.yml`
  references were checked too and still hold: 73-77 is the `test` job's header
  with the three-OS list, and 92 is its only test step.
* The emulated mode was **not** green as a whole at `da555be`.
  `worker::tests::a_spawn_that_finds_the_script_busy_is_retried`, added to
  `development` by `2deceea` ("worker: retry a spawn the kernel refused with
  ETXTBSY") after this card's base, failed there deterministically: 3 runs of
  that test alone, 3 failures, each in 0.14 s. It is not the mechanism in
  section 4 and not reachable from this branch's diff, which touches `socket.rs`
  and this directory.

  The test holds a script open for write and expects `execve` to be refused with
  `ETXTBSY` until the handle closes, so that the daemon's 10 x 50 ms retry rides
  it out. In the translated guest the refusal never reaches the caller: the child
  starts, exits 127, and writes nothing to stdout or stderr, which is why the
  daemon reports its fallback `the worker exited non-zero with no message`.
  Standalone probe (`rustc -O`, hold the file open, spawn it, print the result):

      linux/amd64 guest:  held open: spawn Ok, status=ExitStatus(unix_wait_status(32512)) stdout="" stderr="" in 1.7ms
                          after close: spawn Ok, status=ExitStatus(unix_wait_status(0)) stdout="set: ccc\n" in 11.7ms
      macOS, arm64:       held open: spawn Ok, status=ExitStatus(unix_wait_status(0)) stdout="set: ccc\n" in 469.6ms

  So on real x86_64 Linux the rule bites and the test's premise holds; in the
  translated guest it does not, and the retry the test exists to exercise never
  fires there. That is a test whose premise the emulation does not satisfy, not a
  second `EINTR`.

  **Resolved by `c7e7a12`**, the merge of pull request #29: its commit
  `dec136e` ("worker: the busy-spawn test asks the guest instead of assuming
  ETXTBSY") rewrote the test, and it landed on `development` after this section
  was written. The test no longer assumes the answer: it asks the guest first,
  in `why_the_busy_exec_is_not_refused`, which attempts the exec
  and, on a guest that cannot report the refusal through `Command::spawn`, prints
  the reason and returns without asserting. The retry itself is pinned by a
  second test that needs no kernel,
  `a_spawn_refused_with_etxtbsy_is_retried_until_it_succeeds`, which hands the
  retry loop `errno 26` directly and asserts on the attempt counts, so the
  behaviour is covered on every platform. At this head the emulated run of the
  test prints the skip and passes, in 0.02 s:

      note: this guest cannot report an exec failure through Command::spawn (the emulated amd64 translation loses glibc's posix_spawnp errno), so the refusal never reaches the caller; the retry is not exercised end to end here, and a_spawn_refused_with_etxtbsy_is_retried_until_it_succeeds covers it
      test worker::tests::a_spawn_that_finds_the_script_busy_is_retried ... ok

  The skip is not a hole where the kernel does answer. On `linux/arm64` (a real
  kernel, no translation) the same test takes the asserting path: no note, and
  0.11 s against the emulated run's 0.02 s. A copy of this tree with the retry
  disabled (`SPAWN_ATTEMPTS` 10 -> 1) fails there, so the premise is doing its
  work and the test is not vacuous:

      thread 'worker::tests::a_spawn_that_finds_the_script_busy_is_retried' (244) panicked at crates/whirld/src/worker.rs:1151:22:
      a script that is busy for 100 ms is not a failed spawn: Err(Failed { code: WorkerFailed, message: "cannot spawn /tmp/whirl-worker-test-busy-243/busy.sh: Text file busy (os error 26)" })

  On macOS the test takes the other skip branch, because that kernel does not
  enforce the rule at all (the probe above), so the asserting run is a Linux one
  and the emulated guest is not one.

One reproduction detail, unchanged: plain `cargo test --workspace` stops at the
first red binary, which is why every run in this section passes
`--no-fail-fast`. On the merged tree at `da555be` that meant the emulated run
reported the third red and never reached `control_socket`; at this head there is
no red to stop at.
