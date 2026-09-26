//! The worker's half of `state/locks/rotate.lock` as a race between two
//! processes, which is the only way that half can be checked at all
//! (docs/spec/state-and-cache.md 7.2, 7.3 step 2; docs/architecture.md 1.5).
//!
//! **The race this file exists for.** 7.2 states the lock's purpose in one line:
//! "`rotate.lock` is the same primitive, and it is what makes a sweep unable to
//! run concurrently with a download (5.5) and two rotations unable to run at
//! once." Here the cache-touching run is the worker process this test spawns, and
//! the other holder is this test process holding the same lock for the whole of
//! that run -- the daemon's sweep and the daemon's `busy` refusal are both "the
//! file says someone else has it".
//!
//! What the race costs when the worker's half is missing is not a crash and not
//! a corrupt file: it is a **second run that completes**. `--verb set` is the
//! sharpest instrument for that, because it needs no source and no network: a
//! worker that ignores the lock reaches its setter and exits 0 with a `set:` line
//! while another process is rotating. So the two directions of
//! `the_same_set_run_completes_once_the_holder_is_gone` below run the same
//! command, one with the lock held and one after it is released, and only the
//! held direction is `busy`.
//!
//! `--verb rotate` is the second case, and it pins *when* the lock is taken: the
//! refusal has to be the first thing the run does, so a held lock is `busy` and
//! not whatever stage would have failed next. With no source implemented, that
//! next stage is `no_candidates` (see `tests/argv.rs`), so "busy, and not
//! no_candidates" is the assertion that a late take fails.
//!
//! Every spawned run gets `WHIRL_BACKEND=noop`, a scratch `HOME` and explicit
//! `WHIRL_STATE_DIR`/`WHIRL_CACHE_DIR`, so nothing here reads or writes the
//! machine's own state, cache or desktop.

#![cfg(unix)]

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

/// 7.2's exclusive, non-blocking attempt, spelled as `crates/whirld/src/lock.rs`
/// and `crates/whirl-worker/src/lock.rs` spell it.
const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

/// The lock file as 7.2's ownership table names it, relative to the state
/// directory. A literal here on purpose: this test is a second reader of the
/// same path, so it must not take the name from the code under test.
const ROTATE_FILE: &str = "locks/rotate.lock";

/// The worker binary cargo just built, next to the other bins.
fn worker() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_whirl-worker"))
}

/// A directory of this test's own, short enough for any platform's limits.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "whirl-worker-rotate-lock-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    dir
}

fn json_string(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// The config of docs/development.md section 7, with the one local source, as
/// `tests/argv.rs` writes it.
fn write_config(dir: &Path) -> PathBuf {
    let path = dir.join("config.json");
    let body = format!(
        "{{\n  \"config_schema\": 1,\n  \"backend\": \"noop\",\n  \"sources\": [\n    \
         {{ \"id\": \"pictures\", \"kind\": \"local\", \"weight\": 1, \"paths\": [{}] }}\n  ]\n}}\n",
        json_string(&dir.join("pictures").display().to_string())
    );
    std::fs::write(&path, body).expect("the config is written");
    path
}

/// The state directory every run in this file resolves, and the directory whose
/// `locks/rotate.lock` this test holds.
fn state_dir(dir: &Path) -> PathBuf {
    dir.join("state")
}

/// One worker run, with the two path knobs set to this test's scratch space: the
/// daemon always sets both (docs/architecture.md 1.6), so this is the resolution
/// a spawned worker really gets and the lock lands where the test can hold it.
fn run(dir: &Path, config: &Path, args: &[&str]) -> Output {
    Command::new(worker())
        .arg("--config")
        .arg(config)
        .args(args)
        .env("WHIRL_BACKEND", "noop")
        .env("HOME", dir)
        .env("WHIRL_STATE_DIR", state_dir(dir))
        .env("WHIRL_CACHE_DIR", dir.join("cache"))
        .output()
        .expect("the worker runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// One `set` run: the verb that reaches a setter without a source and without the
/// network, so the only thing between argv and the result line is the lock.
fn set_run(dir: &Path, config: &Path, target: &Path, id: u64) -> Output {
    let target = target.display().to_string();
    let id = id.to_string();
    run(
        dir,
        config,
        &["--verb", "set", "--target", &target, "--run", &id],
    )
}

/// The other holder of 7.2's lock: this test process, `flock`ed on the same file
/// the worker takes, for as long as the guard lives. It is the daemon's sweep
/// from the worker's point of view, and it is a real second process because the
/// worker is spawned, not called.
struct Held {
    _file: File,
}

fn hold(dir: &Path) -> Held {
    let locks = state_dir(dir).join("locks");
    std::fs::create_dir_all(&locks).expect("the locks directory");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(locks.join("rotate.lock"))
        .expect("the lock file");
    let result = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
    assert_eq!(
        result,
        0,
        "the test holds the lock before the worker starts: errno {}",
        std::io::Error::last_os_error()
    );
    Held { _file: file }
}

/// 7.3 step 2, held direction, on the verb that has an observable result without
/// a source: a `set` that meets a held lock is refused with `busy` and sets
/// nothing, so the wallpaper does not change twice under one rotation.
///
/// Red without the worker's half: with no lock taken, this same command exits 0
/// and prints its `set:` line -- the second rotation of 7.2's race, completing.
#[test]
fn a_set_run_meets_a_held_lock_as_busy_and_sets_nothing() {
    let dir = scratch("set-held");
    let config = write_config(&dir);
    let target = dir.join("picture.jpg");
    std::fs::write(&target, b"a file the user already has").expect("the target file");
    let held = hold(&dir);

    let output = set_run(&dir, &config, &target, 1);

    assert_eq!(
        output.status.code(),
        Some(1),
        "a refused run is a failure: {}{}",
        stdout(&output),
        stderr(&output)
    );
    let errors = stderr(&output);
    assert!(
        errors.contains("stage=lock code=busy"),
        "7.3 step 2: the worker fails the non-blocking lock and exits `busy`: {errors}"
    );
    assert_eq!(
        stdout(&output),
        "",
        "and it set nothing: no `set:` line, because the setter was never reached"
    );

    drop(held);
}

/// The same command with the holder gone: it runs, reaches its setter and
/// reports its result. Together with the test above this is the whole behaviour
/// -- the same input, two answers, decided by who holds 7.2's lock.
#[test]
fn the_same_set_run_completes_once_the_holder_is_gone() {
    let dir = scratch("set-free");
    let config = write_config(&dir);
    let target = dir.join("picture.jpg");
    std::fs::write(&target, b"a file the user already has").expect("the target file");

    let held = hold(&dir);
    let refused = set_run(&dir, &config, &target, 1);
    assert_eq!(
        refused.status.code(),
        Some(1),
        "held: {}{}",
        stdout(&refused),
        stderr(&refused)
    );
    assert!(stderr(&refused).contains("stage=lock code=busy"));
    drop(held);

    let output = set_run(&dir, &config, &target, 2);
    assert!(
        output.status.success(),
        "free: {}{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("set: "),
        "the run reached its setter and reported it: {}",
        stdout(&output)
    );
}

/// 7.3 step 2, held direction, on the rotation itself, and the *moment* the lock
/// is taken: a held lock is answered before the pipeline starts, so the failure
/// is `lock`/`busy` and not the `source`/`no_candidates` a run that got past the
/// lock would report (no `kind` has an implementation in this build).
#[test]
fn a_rotation_meets_a_held_lock_as_busy_before_any_stage() {
    let dir = scratch("rotate-held");
    let config = write_config(&dir);
    let held = hold(&dir);

    let output = run(&dir, &config, &["--verb", "rotate", "--run", "1"]);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let errors = stderr(&output);
    assert!(
        errors.contains("stage=lock code=busy"),
        "the refusal is the lock's: {errors}"
    );
    assert!(
        !errors.contains("no_candidates"),
        "and it happens before the source stage, not after it: {errors}"
    );
    assert!(
        errors.contains(ROTATE_FILE),
        "the message names the file 7.2's table names, so an operator knows what to look at: {errors}"
    );
    assert_eq!(
        stdout(&output),
        "",
        "nothing was downloaded and nothing was set"
    );

    drop(held);
}

/// 2.5's `config check` row gives that verb `bad_config`, `worker_failed` and
/// `timeout`, and no `busy`; a check writes nothing into the cache and races
/// nothing, so a held rotation lock does not gate it. A check that answered
/// `busy` would be a code that row does not have.
#[test]
fn a_check_is_not_gated_by_the_rotation_lock() {
    let dir = scratch("check-held");
    let config = write_config(&dir);
    let held = hold(&dir);

    let output = run(&dir, &config, &["--verb", "check", "--run", "1"]);

    assert!(
        output.status.success(),
        "a check still answers while the lock is held: {}{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("plan: "),
        "with the plan record 2.6 fixes: {}",
        stdout(&output)
    );

    drop(held);
}

/// 8.7's record, in the file 8.8's daemon-side exception reads: the worker names
/// itself as the holder, pid and start time, in `daemon.lock`'s own format. A
/// worker that took the lock without writing its record would leave the fallback
/// case of 8.8 with nothing to match the worker it has just reaped against.
#[test]
fn the_lock_file_records_the_worker_that_took_it() {
    let dir = scratch("record");
    let config = write_config(&dir);
    let target = dir.join("picture.jpg");
    std::fs::write(&target, b"a file the user already has").expect("the target file");

    let output = set_run(&dir, &config, &target, 1);
    assert!(
        output.status.success(),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );

    let text = std::fs::read_to_string(state_dir(&dir).join(ROTATE_FILE))
        .expect("the lock file the run created");
    let mut lines = text.lines();
    let pid = lines
        .next()
        .and_then(|line| line.strip_prefix("pid: "))
        .expect("the holder's pid, in daemon.lock's format");
    pid.parse::<u32>().expect("a pid is a number");
    let start = lines
        .next()
        .and_then(|line| line.strip_prefix("start: "))
        .expect("the holder's start time");
    assert!(
        start.ends_with('Z') && start.contains('T'),
        "an RFC 3339 UTC timestamp, as 8.7 asks: {start:?}"
    );
}
