//! `state/locks/daemon.lock`, end to end: two real daemons, one state directory,
//! and a check of the lock that does not trust the daemon's own report.
//!
//! docs/architecture.md 1.5 step 1 takes the lock "exclusively and non-blocking"
//! and holds it for the daemon's lifetime; `docs/spec/state-and-cache.md` 7.2
//! says "a second daemon exits with a message naming the first daemon's pid"; and
//! 2.10's `lock_mode` row makes the value it reports the primitive actually
//! holding the lock.
//!
//! The lock is the operating system's, so the assertions are the operating
//! system's too: `independent` below takes the same lock the daemon took, which
//! is a fact the daemon cannot produce and cannot fake. What the daemon *says*
//! about it is asserted separately, through the protocol.
//!
//! Unix only, like the daemon itself in this build: 2.1's Windows transport is a
//! named pipe and a later card, so `whirld` has no Windows startup path yet.
//!
//! Run it with `cargo test --workspace`: the daemon looks for `whirl-worker` next
//! to its own executable, and that is where a test build lays the two binaries.

#![cfg(unix)]

use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// 7.2's lock file, relative to the state directory (`WHIRL_STATE_DIR`, which is
/// `dir/state` in this file).
const LOCK: &str = "state/locks/daemon.lock";

/// `LOCK_EX|LOCK_NB` of 7.2 ([L 3]); macOS and Linux agree on both bits.
const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

/// `EWOULDBLOCK`/`EAGAIN`: what `flock(LOCK_EX|LOCK_NB)` returns when someone
/// else holds the lock.
#[cfg(target_os = "macos")]
const EWOULDBLOCK: i32 = 35;
#[cfg(not(target_os = "macos"))]
const EWOULDBLOCK: i32 = 11;

/// The signal the supervisor stops the daemon's job with
/// (docs/development.md, "Upgrading, and what happens to a running daemon").
const SIGTERM: i32 = 15;

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
}

/// A temporary tree of this test's own: never the user's config, state, cache or
/// socket (docs/development.md section 7).
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("whirl-lock-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    dir
}

fn socket(dir: &Path) -> PathBuf {
    dir.join("run").join("whirl.sock")
}

/// The daemon of docs/development.md section 7, with every path redirected and
/// `WHIRL_BACKEND=noop` so no wallpaper is ever set.
fn command(dir: &Path) -> Command {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_whirld"));
    let worker = executable
        .parent()
        .expect("the executable has a directory")
        .join("whirl-worker");
    assert!(
        worker.exists(),
        "{} is missing; build the workspace first (`cargo test --workspace`)",
        worker.display()
    );
    let mut command = Command::new(executable);
    command
        .env("WHIRL_CONFIG", dir.join("config.json"))
        .env("WHIRL_SOCKET", socket(dir))
        .env("WHIRL_STATE_DIR", dir.join("state"))
        .env("WHIRL_CACHE_DIR", dir.join("cache"))
        .env("WHIRL_BACKEND", "noop")
        .stdin(Stdio::null());
    command
}

/// One running daemon, waited for by its socket: a daemon that has taken the lock
/// of 1.5 step 1, loaded its state and bound 2.1's socket.
fn start(dir: &Path) -> Child {
    let log = std::fs::File::create(dir.join("daemon.log")).expect("a log file");
    let child = command(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .expect("the daemon starts");
    let deadline = Instant::now() + Duration::from_secs(15);
    while UnixStream::connect(socket(dir)).is_err() {
        assert!(
            Instant::now() < deadline,
            "the daemon did not bind {} in 15 s",
            socket(dir).display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

/// One daemon run, waited for and captured: the refusal cases exit before they
/// can bind anything.
fn run(dir: &Path) -> Output {
    command(dir).output().expect("the daemon runs")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// The independent check: this test takes the same lock the daemon took, with the
/// same non-blocking exclusive call, and reports what the OS said. A test that
/// held the lock is a test in which the daemon is not holding it.
fn try_lock(path: &Path) -> Result<(), i32> {
    let file = std::fs::File::open(path).expect("the lock file exists");
    let result = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }
}

fn stop(child: &mut Child) {
    unsafe { kill(child.id() as i32, SIGTERM) };
    child.wait().expect("the daemon exits on SIGTERM");
}

/// 1.5 step 1 and 7.2: the second daemon refuses, names the holder's pid, and
/// exits with the code the documents give a refused daemon. The first one keeps
/// serving, so what kept the second out is the lock and not a live socket.
#[test]
fn a_second_daemon_refuses_to_start_and_names_the_holder_pid() {
    let dir = scratch("second-daemon");
    let mut first = start(&dir);

    let output = run(&dir);
    let stderr = stderr(&output);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a daemon that refuses to start exits 1 (2.11 item 7: 1 is \"the daemon refused\"): {stderr}"
    );
    assert!(
        stderr.contains(&format!("pid {}", first.id())),
        "the holder's pid is named: {stderr}"
    );
    assert!(stderr.contains("daemon.lock"), "{stderr}");
    assert!(stderr.contains("1.5 step 1"), "{stderr}");

    assert!(
        UnixStream::connect(socket(&dir)).is_ok(),
        "the first daemon is still serving"
    );
    stop(&mut first);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 1.5 step 1 holds the lock for the daemon's lifetime, 7.2 has the kernel
/// release it when the holder exits, and the record in the file is the running
/// daemon's. Every assertion here is made with the OS or with the file, never
/// with the daemon's opinion of its own `lock_mode`.
#[test]
fn the_lock_is_held_while_the_daemon_runs_and_free_once_it_is_gone() {
    let dir = scratch("held-while-running");
    let mut daemon = start(&dir);
    let lock = dir.join(LOCK);
    assert!(lock.is_file(), "1.5 step 1 created {}", lock.display());

    assert_eq!(
        try_lock(&lock).expect_err("the running daemon holds the lock"),
        EWOULDBLOCK,
        "the OS refuses this test's own exclusive attempt"
    );

    let record = std::fs::read_to_string(&lock).expect("the record 7.2 asks the file to carry");
    assert!(
        record.contains(&format!("pid: {}", daemon.id())),
        "the holder's pid, not a leftover: {record}"
    );
    assert!(record.contains("start: "), "{record}");

    stop(&mut daemon);
    assert!(try_lock(&lock).is_ok(), "released once the holder is gone");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The error path: 1.5 step 1 runs before steps 2 and 3, so a startup that fails
/// afterwards has already taken the lock and must not leave it taken. The
/// documents' ordering is what makes this reachable at all: an illegal config
/// (5.1's ordering rule) is refused by `Effective::resolve`, which runs after the
/// lock.
///
/// The second half is 7.2's "no stale-lock recovery": the file that is left
/// behind holds nothing, so a legal daemon starts on the same state directory.
#[test]
fn a_failed_startup_leaves_the_lock_free() {
    let dir = scratch("failed-startup");
    let config = dir.join("config.json");
    std::fs::write(
        &config,
        "{\n  \"filters\": {\n    \"max_bytes\": 41943040\n  },\n  \"cache\": {\n    \"max_bytes\": 1048576\n  }\n}\n",
    )
    .expect("an illegal config");

    let output = run(&dir);
    let stderr = stderr(&output);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a config that fails validation is a refusal to start (8.7): {stderr}"
    );
    assert!(stderr.contains("cache.max_bytes"), "{stderr}");

    let lock = dir.join(LOCK);
    assert!(
        lock.is_file(),
        "step 1 ran before the refusal: {}",
        lock.display()
    );
    assert!(
        try_lock(&lock).is_ok(),
        "the failed startup released the lock"
    );

    std::fs::remove_file(&config).expect("the illegal config");
    let mut daemon = start(&dir);
    assert_eq!(
        try_lock(&lock).expect_err("the new daemon holds it"),
        EWOULDBLOCK
    );
    stop(&mut daemon);
    assert!(try_lock(&lock).is_ok(), "and releases it on the way out");
    let _ = std::fs::remove_dir_all(&dir);
}
