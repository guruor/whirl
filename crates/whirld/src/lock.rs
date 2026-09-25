//! `state/locks/daemon.lock`: the lock the daemon takes before it does anything
//! else, and holds for its lifetime (docs/architecture.md 1.5 step 1,
//! `docs/spec/state-and-cache.md` 7.2 and 8.7).
//!
//! 1.5 step 1: "Take `state/locks/daemon.lock` exclusively and non-blocking.
//! Held, exit with the holder's pid if not." 7.2 gives the primitive:
//! `flock(LOCK_EX|LOCK_NB)` on POSIX, `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK|
//! LOCKFILE_FAIL_IMMEDIATELY)` on Windows, "a second daemon exits with a message
//! naming the first daemon's pid". 8.7 gives the one fallback: where the
//! filesystem cannot `flock` (`ENOTSUP`), "fall back to an `O_CREAT|O_EXCL` lock
//! file that names the holder pid and start time, and report `lock_mode:
//! excl_file`".
//!
//! Two consequences of those three rules shape this module.
//!
//! - **There are two primitives, and `status` has to name the one actually
//!   held** (docs/architecture.md 2.10's `lock_mode` row: "the primitive actually
//!   holding `state/locks/daemon.lock` ... There is no third value"). So the
//!   value is read from [`DaemonLock::mode`], which [`select`] decides from what
//!   the OS did, and never from a constant. What the OS did is passed in as a
//!   probe, which is the only way both directions are testable on a machine
//!   whose filesystems all support `flock`.
//! - **A daemon that cannot take the lock does not run** (1.5 step 1, 2.10's
//!   row). There is no third mode and no "continue without a lock": a refusal is
//!   the only other outcome, and it names the holder.

use crate::plan::create_private_dir;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// The lock file, relative to the state directory: 7.2's ownership table lists
/// `state/locks/daemon.lock`, written by "daemon (held for its lifetime)" and
/// read by "a second daemon, at startup".
pub const LOCK_FILE: &str = "locks/daemon.lock";

/// The directory 1.5 step 1 creates the lock file in, split out so that the
/// directory and the file it holds cannot drift apart.
pub const LOCK_DIR: &str = "locks";

/// The exclusive lock of 7.2, `LOCK_EX|LOCK_NB`: macOS and Linux agree on both
/// bits ([L 3]).
const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

/// `EWOULDBLOCK`, which is `EAGAIN` on both targets: `flock`'s "already held by
/// someone else" (7.2 and [L 3]).
#[cfg(target_os = "macos")]
const EWOULDBLOCK: i32 = 35;
#[cfg(not(target_os = "macos"))]
const EWOULDBLOCK: i32 = 11;

/// `ENOTSUP`, which 8.7 names as the fallback's trigger. macOS keeps `ENOTSUP`
/// (45) and `EOPNOTSUPP` (102) apart, so both are accepted; on Linux both are 95.
#[cfg(target_os = "macos")]
const ENOTSUP: i32 = 45;
#[cfg(target_os = "macos")]
const EOPNOTSUPP: i32 = 102;
#[cfg(not(target_os = "macos"))]
const ENOTSUP: i32 = 95;
#[cfg(not(target_os = "macos"))]
const EOPNOTSUPP: i32 = 95;

/// `ENOSYS`: a kernel that does not implement `flock` at all is the same fact as
/// a filesystem that refuses it, and 8.7's answer is the same.
#[cfg(target_os = "macos")]
const ENOSYS: i32 = 78;
#[cfg(not(target_os = "macos"))]
const ENOSYS: i32 = 38;

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

/// The primitive holding the lock. These are the only two values 2.10's
/// `lock_mode` row allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `flock(LOCK_EX|LOCK_NB)`: this platform's own exclusive lock. Windows's
    /// `LockFileEx` is this value too, "because it is that platform's own
    /// exclusive lock rather than a weaker one" (2.10's row).
    Flock,
    /// 8.7's fallback: an `O_CREAT|O_EXCL` file whose existence is the lock.
    ExclFile,
}

impl Mode {
    /// The `lock_mode` value of 2.10. One place, so the status key and a refusal
    /// message cannot disagree about the name.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Flock => "flock",
            Mode::ExclFile => "excl_file",
        }
    }
}

/// What one non-blocking exclusive attempt on an open descriptor did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    /// The lock is ours.
    Acquired,
    /// Someone else holds it: the daemon does not run (1.5 step 1).
    Held,
    /// This filesystem cannot `flock`: 8.7's `excl_file` fallback applies.
    Unsupported,
}

/// The primitive [`Attempt`] selects, or `None` when the daemon does not run.
///
/// This is the whole of the derivation `status` reports: 2.10's row is satisfied
/// only if the value comes from here, which is why the probe that produces the
/// [`Attempt`] is an argument and not a call.
pub fn select(attempt: Attempt) -> Option<Mode> {
    match attempt {
        Attempt::Acquired => Some(Mode::Flock),
        Attempt::Unsupported => Some(Mode::ExclFile),
        Attempt::Held => None,
    }
}

/// The lock this process holds for its lifetime, plus what took it.
#[derive(Debug)]
pub struct DaemonLock {
    path: PathBuf,
    /// The descriptor the `flock` lives on. Closing it releases the lock ([L 3]:
    /// "locks are on files rather than descriptors and are released when the
    /// descriptor is closed"), which is why it is held open for the daemon's
    /// lifetime rather than closed after the `flock` call.
    file: Option<File>,
    mode: Mode,
}

impl DaemonLock {
    /// What is actually holding the lock, for `status` (2.10 `lock_mode`). The
    /// value is read from here and never written as a literal, which is what the
    /// row's "the primitive actually holding" asks for.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The record 8.7 asks the fallback file to carry: the holder's pid and start
    /// time, which is what a refused second daemon names.
    fn record(pid: u32) -> String {
        format!(
            "pid: {pid}\nstart: {}\n",
            whirl_core::protocol::rfc3339_utc(crate::events::unix_seconds())
        )
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        // 8.7's file *is* the lock, so it has to go when the lock goes.
        if self.mode == Mode::ExclFile {
            let _ = std::fs::remove_file(&self.path);
        }
        // Closing the descriptor releases an `flock` ([L 3]). Done here rather
        // than left to process exit so that a startup step that fails after the
        // lock was taken releases it on its way out.
        self.file.take();
    }
}

/// Take `state/locks/daemon.lock` exclusively and non-blocking (1.5 step 1).
///
/// `state_dir` is the directory 1.5 step 2 opens, and it need not exist yet:
/// both it and its `locks` subdirectory are created here with the mode 2.1 gives
/// the directories this daemon owns, because step 1 may be the first thing to
/// touch either of them.
pub fn take(state_dir: &Path) -> Result<DaemonLock, String> {
    take_with(state_dir, flock_attempt)
}

/// The same, with the capability probe supplied: the only way to test 8.7's two
/// directions on a machine whose filesystems all support `flock`.
fn take_with(
    state_dir: &Path,
    probe: impl Fn(&File) -> Result<Attempt, std::io::Error>,
) -> Result<DaemonLock, String> {
    // The state directory first, then `locks`: `create_private_dir` restricts
    // only the directory it creates itself, so asking it for the nested path
    // first would leave the state directory at the umask's mode.
    create_private_dir(state_dir)?;
    create_private_dir(&state_dir.join(LOCK_DIR))?;
    let path = state_dir.join(LOCK_FILE);
    let pid = std::process::id();

    // 8.7's fallback is `O_CREAT|O_EXCL`, so the file is created exclusively
    // first. A file this process created is a file no other process can be
    // holding, which is what makes the fallback's "existence is the lock" case
    // distinguishable from "a `flock` holder exists" without a second file.
    match open_exclusive(&path) {
        Ok(file) => {
            let attempt = probe(&file).map_err(|error| cannot_lock(&path, &error))?;
            match select(attempt) {
                Some(mode) => {
                    // Nobody else can hold a file this process just created, so
                    // the record is written before anything can read it.
                    write_record(&file, pid)?;
                    Ok(DaemonLock {
                        path,
                        file: Some(file),
                        mode,
                    })
                }
                // A descriptor we created exclusively cannot be held, so an OS
                // that says otherwise is not a state to keep running in.
                None => Err(held_message(&path, &file)),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // 7.2 has no stale-lock recovery: with `flock` the file may exist
            // from any earlier daemon and still be free, because the lock is the
            // kernel's, not the file's; with `excl_file` the existence is the
            // lock, so an existing file is a refusal.
            let file = open_existing(&path)?;
            let attempt = probe(&file).map_err(|error| cannot_lock(&path, &error))?;
            match select(attempt) {
                Some(Mode::Flock) => {
                    write_record(&file, pid)?;
                    Ok(DaemonLock {
                        path,
                        file: Some(file),
                        mode: Mode::Flock,
                    })
                }
                Some(Mode::ExclFile) | None => Err(held_message(&path, &file)),
            }
        }
        Err(error) => Err(format!("cannot create {}: {error}", path.display())),
    }
}

/// The real probe: `flock(LOCK_EX|LOCK_NB)` on this descriptor ([L 3]).
fn flock_attempt(file: &File) -> Result<Attempt, std::io::Error> {
    let result = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
    if result == 0 {
        return Ok(Attempt::Acquired);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == EWOULDBLOCK => Ok(Attempt::Held),
        Some(code) if code == ENOTSUP || code == EOPNOTSUPP || code == ENOSYS => {
            Ok(Attempt::Unsupported)
        }
        _ => Err(error),
    }
}

fn open_exclusive(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

fn open_existing(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))
}

fn write_record(file: &File, pid: u32) -> Result<(), String> {
    let mut writer = file;
    writer
        .seek(SeekFrom::Start(0))
        .and_then(|_| writer.set_len(0))
        .and_then(|_| writer.write_all(DaemonLock::record(pid).as_bytes()))
        .map_err(|error| format!("cannot write the daemon lock record: {error}"))
}

/// The `pid` and `start` of 7.2's record, as the holder wrote them.
#[derive(Debug, Default, PartialEq, Eq)]
struct Record {
    pid: Option<u32>,
    start: Option<String>,
}

fn read_record(file: &File) -> Record {
    let mut text = String::new();
    let mut reader = file;
    if reader.seek(SeekFrom::Start(0)).is_err() || reader.read_to_string(&mut text).is_err() {
        return Record::default();
    }
    let mut record = Record::default();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("pid: ") {
            record.pid = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("start: ") {
            record.start = Some(value.trim().to_string());
        }
    }
    record
}

/// 7.2's refusal: "A second daemon exits with a message naming the first
/// daemon's pid", so the pid is the part that must be there. The window in which
/// a holder has the lock but has not yet written its record is one line of code
/// wide and is named instead of guessed at.
fn held_message(path: &Path, file: &File) -> String {
    let record = read_record(file);
    let holder = match record.pid {
        Some(pid) => format!(
            "pid {} (started {})",
            pid,
            record.start.as_deref().unwrap_or("-")
        ),
        None => "another daemon that has not recorded its pid yet".to_string(),
    };
    format!(
        "{} is held by {holder}: a second daemon on this state directory does not start (docs/architecture.md 1.5 step 1)",
        path.display()
    )
}

fn cannot_lock(path: &Path, error: &std::io::Error) -> String {
    format!("cannot take the daemon lock at {}: {error}", path.display())
}

/// A lock taken with the OS answer supplied. 2.10's row has two values and this
/// machine's filesystems only ever produce one of them, so this is how both
/// directions of [`select`] and of 8.7's fallback are reachable in a test.
#[cfg(test)]
pub(crate) fn take_as(state_dir: &Path, attempt: Attempt) -> Result<DaemonLock, String> {
    take_with(state_dir, move |_| Ok(attempt))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("whirl-lock-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    /// The lock file of a scratch state directory, spelled the way `take` spells
    /// it: these tests read the file itself, because that is what the refusal and
    /// the record are about.
    fn lock_path(state_dir: &Path) -> PathBuf {
        state_dir.join(LOCK_FILE)
    }

    /// 2.10's `lock_mode` row is a function of the OS answer and nothing else:
    /// `Acquired` is 7.2's `flock`, `Unsupported` is 8.7's `excl_file`, and
    /// `Held` has no value at all because "a daemon that cannot take the lock
    /// does not run".
    #[test]
    fn the_primitive_follows_the_os_answer_and_has_no_third_value() {
        assert_eq!(select(Attempt::Acquired), Some(Mode::Flock));
        assert_eq!(select(Attempt::Unsupported), Some(Mode::ExclFile));
        assert_eq!(select(Attempt::Held), None);
        assert_eq!(Mode::Flock.as_str(), "flock");
        assert_eq!(Mode::ExclFile.as_str(), "excl_file");
    }

    /// 1.5 step 1 and 7.2: the lock is real, and the second daemon refuses rather
    /// than sharing the state directory. Both directions of the refusal and of
    /// the release are asserted here; the two-process case is the integration
    /// test's job (`tests/daemon_lock.rs`).
    #[test]
    fn the_second_take_refuses_and_the_first_holder_releases_on_drop() {
        let dir = scratch("second-take");
        let first = take(&dir).expect("the first daemon takes the lock");
        assert_eq!(
            first.mode(),
            Mode::Flock,
            "APFS supports flock, so 7.2's primitive is the one in force here"
        );
        assert!(
            lock_path(&dir).is_file(),
            "1.5 step 1 writes {}",
            lock_path(&dir).display()
        );

        let refused = take(&dir).expect_err("the second daemon does not start");
        assert!(
            refused.contains(&format!("pid {}", std::process::id())),
            "the holder's pid is named: {refused}"
        );
        assert!(refused.contains("started "), "{refused}");

        drop(first);
        assert!(take(&dir).is_ok(), "a released flock leaves nothing behind");
    }

    /// 7.2's message names the pid the *holder* recorded, so this plants a record
    /// whose pid is not this process's and asserts that one comes back.
    #[test]
    fn the_refusal_names_the_recorded_holder_not_the_process_that_refused() {
        let dir = scratch("recorded-pid");
        create_private_dir(&dir.join(LOCK_DIR)).expect("the locks directory");
        std::fs::write(lock_path(&dir), "pid: 4242\nstart: 2026-09-25T07:41:12Z\n")
            .expect("a planted record");

        let refused = take_as(&dir, Attempt::Held).expect_err("someone else holds it");
        assert!(refused.contains("pid 4242"), "{refused}");
        assert!(
            refused.contains("2026-09-25T07:41:12Z"),
            "and the start time of the record: {refused}"
        );
    }

    /// 8.7's fallback, in both directions: the exclusive file is the lock where
    /// the filesystem cannot `flock`, it names `excl_file`, a second daemon is
    /// refused by its existence, and releasing removes it.
    #[test]
    fn a_filesystem_without_flock_falls_back_to_the_exclusive_file() {
        let dir = scratch("excl-file");
        let lock = take_as(&dir, Attempt::Unsupported).expect("the fallback is taken");
        assert_eq!(lock.mode(), Mode::ExclFile);
        assert_eq!(
            lock.mode().as_str(),
            "excl_file",
            "the value `status` reports (2.10)"
        );

        let path = lock_path(&dir);
        let record = std::fs::read_to_string(&path).expect("the record of 8.7");
        assert!(
            record.contains(&format!("pid: {}", std::process::id())),
            "{record}"
        );
        assert!(record.contains("start: "), "{record}");

        let refused =
            take_as(&dir, Attempt::Unsupported).expect_err("the file exists, so it is held");
        assert!(
            refused.contains(&format!("pid {}", std::process::id())),
            "{refused}"
        );

        drop(lock);
        assert!(
            !path.exists(),
            "8.7's file is the lock, so releasing it has to remove it"
        );
    }
}
