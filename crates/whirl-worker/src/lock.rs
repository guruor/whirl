//! The worker's half of `state/locks/rotate.lock` (docs/spec/state-and-cache.md
//! 7.2 and 7.3 step 2, docs/architecture.md 1.5).
//!
//! 7.3 step 2 is the obligation this file implements, and it is one sentence:
//!
//! > **The lock has exactly one holder at a time and it is the worker**: the
//! > worker takes `rotate.lock` exclusively for its own lifetime, and the daemon
//! > takes it only for a sweep. A second rotation that arrives while the first is
//! > running therefore does not wait behind a queue: its worker fails the
//! > non-blocking lock and exits `busy`, which is a fact the user can see,
//! > instead of a second download nobody asked for.
//!
//! Three things are read off that sentence, and this module is those three and
//! nothing else:
//!
//! - **Exclusive and non-blocking.** 7.2 takes the primitive from `daemon.lock`:
//!   `flock(LOCK_EX|LOCK_NB)` on POSIX, or 8.7's `O_CREAT|O_EXCL` fallback where
//!   that call answers `ENOTSUP`. There is no third mode and no waiting: 7.3 step
//!   2's worker "fails the non-blocking lock".
//! - **Held for the worker's own lifetime.** The guard the caller holds is
//!   dropped when the process leaves `main`, and closing the descriptor is the
//!   release (7.2, `[L 3]`: "locks are on files rather than descriptors and are
//!   released when the descriptor is closed"), so the kernel releases it on any
//!   exit, "including `SIGKILL`". That is what the primitive buys, and it is why
//!   a killed worker needs no recovery step of its own (architecture.md 1.7.2).
//! - **Contention is `busy`, not an error.** [`take`] answers `Ok(None)` for
//!   "someone else holds it", and the caller exits `busy` (2.7).
//!
//! This is the other half of `crates/whirld/src/lock.rs` and it must stay one
//! primitive with it: same path, same `flock` flags, same 8.7 fallback, and the
//! same lock-file record, because 8.8's one exception is the daemon reading the
//! record **this** file writes to recognise the worker it has just reaped.
//!
//! The daemon's half is the refuse-or-defer reading of a contention: 5.5 step 1
//! records `sweep_deferred: 1` and leaves the work for the next rotation. Nothing
//! here defers anything: a worker that cannot take the lock has nothing else to
//! do, which is why the type has no refusal message to carry.
//!
//! **Deviation from 7.2, stated rather than hidden.** 7.2's Windows primitive is
//! `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK|LOCKFILE_FAIL_IMMEDIATELY)`, and this
//! build has no `LockFileEx` (nor a Windows daemon: `whirld` refuses to start
//! there, and no Windows host was available to verify an FFI declaration
//! against). Rather than run a rotation with no lock at all, a target that
//! cannot offer the primitive takes 8.7's `excl_file` fallback, which is the
//! mechanism the spec defines for exactly this shape of "the platform's own call
//! is not available here". It is the conservative direction: the invariant of
//! 7.3 step 2 holds, on the weaker of 2.10's two modes.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The lock file, relative to the state directory: 7.2's ownership table lists
/// `state/locks/rotate.lock` as written by "a worker, for the run; the daemon,
/// for a sweep", and read by "both".
pub const ROTATE_FILE: &str = "locks/rotate.lock";

/// The directory the lock file lives in, relative to the state directory, split
/// out so that the directory and the file it holds cannot drift apart.
const LOCK_DIR: &str = "locks";

/// The exclusive lock of 7.2, `LOCK_EX|LOCK_NB`: macOS and Linux agree on both
/// bits (`[L 3]`).
#[cfg(unix)]
const LOCK_EX: i32 = 2;
#[cfg(unix)]
const LOCK_NB: i32 = 4;

/// `EWOULDBLOCK`, which is `EAGAIN` on both targets: `flock`'s "already held by
/// someone else", which for a worker is 7.3 step 2's `busy`.
#[cfg(unix)]
#[cfg(target_os = "macos")]
const EWOULDBLOCK: i32 = 35;
#[cfg(unix)]
#[cfg(not(target_os = "macos"))]
const EWOULDBLOCK: i32 = 11;

/// `ENOTSUP`, which 8.7 names as the fallback's trigger. macOS keeps `ENOTSUP`
/// (45) and `EOPNOTSUPP` (102) apart, so both are accepted; on Linux both are 95.
#[cfg(unix)]
#[cfg(target_os = "macos")]
const ENOTSUP: i32 = 45;
#[cfg(unix)]
#[cfg(target_os = "macos")]
const EOPNOTSUPP: i32 = 102;
#[cfg(unix)]
#[cfg(not(target_os = "macos"))]
const ENOTSUP: i32 = 95;
#[cfg(unix)]
#[cfg(not(target_os = "macos"))]
const EOPNOTSUPP: i32 = 95;

/// `ENOSYS`: a kernel that does not implement `flock` at all is the same fact as
/// a filesystem that refuses it, and 8.7's answer is the same.
#[cfg(unix)]
#[cfg(target_os = "macos")]
const ENOSYS: i32 = 78;
#[cfg(unix)]
#[cfg(not(target_os = "macos"))]
const ENOSYS: i32 = 38;

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

/// The primitive holding the lock, exactly as 7.2 and 8.7 define them. There is
/// no third value: a take that selects neither is a take that did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `flock(LOCK_EX|LOCK_NB)`: this platform's own exclusive lock.
    Flock,
    /// 8.7's fallback: an `O_CREAT|O_EXCL` file whose existence is the lock, and
    /// whose record names the holder.
    ExclFile,
}

/// What one non-blocking exclusive attempt on an open descriptor did.
///
/// `Acquired` and `Held` are the two answers only a platform primitive can give,
/// so on a target whose `platform_attempt` is the `Unsupported` stub they are
/// unconstructed in every non-test build. That is the design and not an
/// oversight -- they are 7.2's answers, kept in the type so `select` reads as the
/// spec's three-way choice on every target -- so the lint is switched off there
/// rather than the variants being cfg'd away, which would leave an enum that no
/// longer says what it means on the target that needs the fallback most.
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// The lock is ours.
    Acquired,
    /// Someone else holds it: 7.3 step 2's `busy`, and not an error.
    Held,
    /// This filesystem cannot `flock`, so 8.7's fallback applies.
    Unsupported,
}

/// The primitive [`Attempt`] selects, or `None` when someone else holds the
/// lock. `Held` is the only answer a caller may not ignore.
fn select(attempt: Attempt) -> Option<Mode> {
    match attempt {
        Attempt::Acquired => Some(Mode::Flock),
        Attempt::Unsupported => Some(Mode::ExclFile),
        Attempt::Held => None,
    }
}

/// The rotation lock while this worker holds it.
#[derive(Debug)]
pub struct RotateLock {
    path: PathBuf,
    /// The descriptor the `flock` lives on, kept open for the run: closing it is
    /// the release. Under the fallback, `None` is never reached, because the
    /// file's existence is the lock and there is nothing to hold.
    file: Option<File>,
    mode: Mode,
}

impl Drop for RotateLock {
    fn drop(&mut self) {
        // 8.7's file *is* the lock, so it goes when the lock goes. Under `flock`
        // there is nothing to remove: the lock lives on the descriptor, and
        // closing it releases it.
        if self.mode == Mode::ExclFile {
            let _ = std::fs::remove_file(&self.path);
        }
        self.file.take();
    }
}

/// Take `state/locks/rotate.lock` exclusively and non-blocking (7.2, 7.3 step 2).
///
/// `Ok(None)` is "someone else holds it": the second rotation is refused, not
/// queued, and the caller says `busy`. `Err` is a lock that could not be created
/// or taken at all, which no rotation may proceed past.
pub fn take(state_dir: &Path) -> Result<Option<RotateLock>, String> {
    take_with(state_dir, platform_attempt)
}

/// The same, with the capability probe supplied: 8.7's `ENOTSUP` direction is
/// one no filesystem on this machine can produce, and on a target with no
/// `LockFileEx` it is the direction taken by default.
fn take_with(
    state_dir: &Path,
    probe: impl Fn(&File) -> Result<Attempt, std::io::Error>,
) -> Result<Option<RotateLock>, String> {
    create_locks_dir(state_dir)?;
    let path = state_dir.join(ROTATE_FILE);
    let pid = std::process::id();

    // 8.7's fallback is `O_CREAT|O_EXCL`, so the file is created exclusively
    // first. A file this process created is a file no other process can be
    // holding, which is what makes "the existence is the lock" distinguishable
    // from "an `flock` holder exists" without a second file.
    match open_exclusive(&path) {
        Ok(file) => {
            let attempt = probe(&file).map_err(|error| cannot_lock(&path, &error))?;
            match select(attempt) {
                Some(mode) => {
                    // Nobody else can hold a file this process just created, so
                    // the record is written before anything can read it: 8.8's
                    // daemon-side exception matches on it.
                    write_record(&file, pid)?;
                    Ok(Some(RotateLock {
                        path,
                        file: Some(file),
                        mode,
                    }))
                }
                // A descriptor this process created exclusively cannot be held
                // by anyone else, so an OS that says otherwise is not a state to
                // rotate in.
                None => Err(format!(
                    "{} was created exclusively and still reads as held: not rotating on that answer",
                    path.display()
                )),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // The file outlives every run, so it is normally already there: with
            // `flock` it may exist from any earlier run and still be free,
            // because the lock is the kernel's and not the file's.
            let file = open_existing(&path)?;
            let attempt = probe(&file).map_err(|error| cannot_lock(&path, &error))?;
            match select(attempt) {
                Some(Mode::Flock) => {
                    write_record(&file, pid)?;
                    Ok(Some(RotateLock {
                        path,
                        file: Some(file),
                        mode: Mode::Flock,
                    }))
                }
                // 8.8: under the fallback the file's existence is the lock, so a
                // file this process did not create is a holder this process does
                // not take over, and the answer is 7.3 step 2's `busy`.
                Some(Mode::ExclFile) | None => Ok(None),
            }
        }
        Err(error) => Err(format!("cannot create {}: {error}", path.display())),
    }
}

/// The real probe: `flock(LOCK_EX|LOCK_NB)` on this descriptor (`[L 3]`).
#[cfg(unix)]
fn platform_attempt(file: &File) -> Result<Attempt, std::io::Error> {
    use std::os::unix::io::AsRawFd;
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

/// A target whose platform primitive is not in this build: 7.2's Windows half is
/// `LockFileEx`, which does not exist here, so 8.7's fallback is the honest
/// reading rather than a rotation with no lock at all (see the module docs).
#[cfg(not(unix))]
fn platform_attempt(_file: &File) -> Result<Attempt, std::io::Error> {
    Ok(Attempt::Unsupported)
}

fn open_exclusive(path: &Path) -> std::io::Result<File> {
    let options = OpenOptions::new();
    #[cfg(unix)]
    let options = {
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = options;
        options.mode(0o600);
        options
    };
    let mut options = options;
    options.read(true).write(true).create_new(true).open(path)
}

fn open_existing(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))
}

/// What 8.7 asks the lock file to carry: the holder's pid and start time, "which
/// is the pair 8.8 needs to tell a live holder from a recycled pid". The same
/// record `daemon.lock` carries, in the same format, because 8.8's owner is one
/// reader for both locks.
fn record(pid: u32) -> String {
    format!(
        "pid: {pid}\nstart: {}\n",
        whirl_core::protocol::rfc3339_utc(unix_seconds())
    )
}

fn write_record(file: &File, pid: u32) -> Result<(), String> {
    let mut writer = file;
    writer
        .seek(SeekFrom::Start(0))
        .and_then(|_| writer.set_len(0))
        .and_then(|_| writer.write_all(record(pid).as_bytes()))
        .map_err(|error| format!("cannot write the rotation lock record: {error}"))
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

/// `state/locks`, owner-only, for the same reason `daemon.lock`'s directory is:
/// the worker can be the first thing to touch the state directory of an install
/// whose daemon has not started yet. An existing directory keeps the mode its
/// owner chose; only one this process created is restricted
/// (`create_private_dir` in `crates/whirld/src/plan.rs`).
fn create_locks_dir(state_dir: &Path) -> Result<(), String> {
    let locks = state_dir.join(LOCK_DIR);
    if locks.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(&locks)
        .map_err(|error| format!("cannot create {}: {error}", locks.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&locks, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn cannot_lock(path: &Path, error: &std::io::Error) -> String {
    format!(
        "cannot take the rotation lock at {}: {error}",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own, removed first so a rerun starts free.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("whirl-worker-lock-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        dir
    }

    /// 7.2's primitive, both directions: the first take holds the lock, the
    /// second is told "someone else holds it" -- which is 7.3 step 2's `busy` --
    /// and the lock is free again once the holder drops, because the release is
    /// the kernel's ("`rotate.lock` needs no recovery logic where the kernel owns
    /// the lock", architecture.md 1.7.2).
    #[test]
    fn a_second_take_is_refused_until_the_holder_drops() {
        let dir = scratch("held");
        let holder = take(&dir)
            .expect("a take that cannot fail")
            .expect("the lock is free to begin with");
        assert!(
            take(&dir).expect("a take that cannot fail").is_none(),
            "7.3 step 2: a second rotation fails the non-blocking lock"
        );

        drop(holder);
        assert!(
            take(&dir).expect("a take that cannot fail").is_some(),
            "the kernel releases a dropped `flock`, so the next run takes it"
        );
    }

    /// 7.2's record in the lock file: the pid and start time 8.7 asks for, which
    /// is the pair 8.8's daemon-side exception matches on to recognise the worker
    /// it has just reaped. Read back as text, because that is how the daemon
    /// reads it.
    #[test]
    fn the_lock_file_names_its_holder() {
        let dir = scratch("record");
        let holder = take(&dir)
            .expect("a take that cannot fail")
            .expect("the lock is free to begin with");
        let text = std::fs::read_to_string(dir.join(ROTATE_FILE)).expect("the lock file");
        assert_eq!(
            text,
            record(std::process::id()),
            "the record is the holder's own pid and start time"
        );
        drop(holder);
    }

    /// 8.7's fallback direction, which no filesystem on this machine produces:
    /// there the file's *existence* is the lock, so a file this process did not
    /// create is a holder it does not take over (8.8), and the answer is the same
    /// `Ok(None)` that `busy` is made of.
    #[test]
    fn a_lock_file_this_process_did_not_create_is_a_holder_under_the_fallback() {
        let dir = scratch("fallback");
        create_locks_dir(&dir).expect("the locks directory");
        std::fs::write(dir.join(ROTATE_FILE), record(4711)).expect("a planted lock file");

        let refused =
            take_with(&dir, |_| Ok(Attempt::Unsupported)).expect("a take that cannot fail");
        assert!(
            refused.is_none(),
            "8.8: the file's existence is the holder, and it is not taken over"
        );
        assert!(
            dir.join(ROTATE_FILE).is_file(),
            "and a lock file this process did not create is not removed"
        );
    }

    /// The fallback's other direction: a file this process *did* create
    /// exclusively is itself the lock, so it holds until it drops, and then the
    /// file is gone rather than left behind for 8.8 to judge.
    #[test]
    fn the_fallback_holds_by_existence_and_removes_its_own_file() {
        let dir = scratch("fallback-own");
        let holder = take_with(&dir, |_| Ok(Attempt::Unsupported))
            .expect("a take that cannot fail")
            .expect("the file is ours to create");
        let path = dir.join(ROTATE_FILE);
        assert!(path.is_file(), "the existence is the lock");
        assert!(
            take_with(&dir, |_| Ok(Attempt::Unsupported))
                .expect("a take that cannot fail")
                .is_none(),
            "the second take sees the file and is refused"
        );

        drop(holder);
        assert!(
            !path.exists(),
            "8.7: the file goes when the lock goes, so a clean exit leaves nothing"
        );
    }

    /// The probe's `Acquired` answer: 7.2's own primitive, so the mode is the
    /// platform lock and the lock file outlives the run -- only the descriptor was
    /// locked, and closing it was the release. On a target whose
    /// `platform_attempt` answers `Unsupported`, this is also the only place the
    /// arm is exercised at all, which is why the probe is a parameter.
    #[test]
    fn a_probe_of_acquired_is_the_platform_lock_and_leaves_the_file() {
        let dir = scratch("probe-acquired");
        let holder = take_with(&dir, |_| Ok(Attempt::Acquired))
            .expect("a take that cannot fail")
            .expect("the file is ours to create");
        assert_eq!(
            holder.mode,
            Mode::Flock,
            "`flock` acquired is 7.2's primitive"
        );

        drop(holder);
        assert!(
            dir.join(ROTATE_FILE).is_file(),
            "under `flock` the file outlives the run: closing the descriptor was the release"
        );
    }

    /// The probe's remaining answer, which is 7.3 step 2's `busy` and never 8.7's
    /// fallback: a descriptor someone else holds is a refusal, and on a file this
    /// process created exclusively it is an answer no descriptor can give, so
    /// nothing rotates on it either.
    #[test]
    fn a_probe_of_held_is_busy_and_never_the_fallback() {
        let dir = scratch("probe-held");
        create_locks_dir(&dir).expect("the locks directory");
        std::fs::write(dir.join(ROTATE_FILE), record(4711)).expect("a planted lock file");

        let refused = take_with(&dir, |_| Ok(Attempt::Held)).expect("a take that cannot fail");
        assert!(
            refused.is_none(),
            "7.3 step 2: a holder is `busy`, which is not 8.7's fallback and not a take"
        );

        let fresh = scratch("probe-held-fresh");
        let message = take_with(&fresh, |_| Ok(Attempt::Held))
            .expect_err("a file this process created exclusively cannot be held");
        assert!(message.contains("reads as held"), "{message}");
    }
}
