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

    /// 8.7's reporting half: "report `lock_mode: excl_file` in `status` so the
    /// weaker guarantee is visible rather than assumed". 4.2's `cache.root`
    /// comment has `config check` report the same fact. Under `flock` there is
    /// nothing to add, which is why this is an `Option` and not a line: 2.5's
    /// `config check` body is exhaustive, and `status` derives its own value from
    /// `mode` rather than from here.
    pub fn report_line(&self) -> Option<String> {
        match self.mode {
            Mode::Flock => None,
            Mode::ExclFile => Some(format!("lock_mode: {}", self.mode.as_str())),
        }
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
                    write_record(&path, &file, pid)?;
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
                    write_record(&path, &file, pid)?;
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

fn write_record(path: &Path, file: &File, pid: u32) -> Result<(), String> {
    let mut writer = file;
    writer
        .seek(SeekFrom::Start(0))
        .and_then(|_| writer.set_len(0))
        .and_then(|_| writer.write_all(DaemonLock::record(pid).as_bytes()))
        .map_err(|error| {
            format!(
                "cannot write the lock record at {}: {error}",
                path.display()
            )
        })
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

// ---------------------------------------------------------------------------
// The rotation lock
// ---------------------------------------------------------------------------

/// The rotation lock, relative to the state directory: 7.2's ownership table
/// gives `state/locks/rotate.lock` two writers, "a worker, for the run; the
/// daemon, for a sweep", and two readers, "both".
pub const ROTATE_FILE: &str = "locks/rotate.lock";

/// The rotation lock while one process holds it.
///
/// It is the same primitive as [`DaemonLock`] and for the same reason: 7.2 calls
/// `rotate.lock` "the same primitive", and what the primitive buys is the release
/// the kernel performs when the holder exits, `SIGKILL` included [L 3].
///
/// The difference is what a refusal means. A daemon that cannot take
/// `daemon.lock` does not run (1.5 step 1); the process that cannot take
/// `rotate.lock` is told "someone else is rotating" and does something else --
/// a worker exits `busy` (7.3 step 2), and a sweep records `sweep_deferred: 1`
/// and leaves the work for the next rotation (5.5 step 1). So this type has no
/// refusal message: [`take_rotate`] answers `Ok(None)` for a lock held by
/// someone else.
#[derive(Debug)]
pub struct RotateLock {
    path: PathBuf,
    /// Under `flock` the descriptor whose close releases the lock; under 8.7's
    /// fallback nothing, because the file's existence is the lock.
    file: Option<File>,
    mode: Mode,
}

impl Drop for RotateLock {
    fn drop(&mut self) {
        // 8.7's file *is* the lock, so it has to go when the lock goes.
        if self.mode == Mode::ExclFile {
            let _ = std::fs::remove_file(&self.path);
        }
        self.file.take();
    }
}

/// Take `state/locks/rotate.lock`, non-blocking (5.5 step 1, 7.3 step 2).
///
/// `Ok(None)` is "someone else holds it", which is not an error for either
/// caller: the worker exits `busy`, the sweep defers. Creating the `locks`
/// directory is this function's job for the same reason it is the daemon
/// lock's: it may be the first thing to touch the state directory.
///
/// This is the take without 8.8's exception, which is the one 5.5's startup and
/// `reset` triggers make: neither of them has reaped a worker, so neither holds
/// the proof the exception rests on.
pub fn take_rotate(state_dir: &Path) -> Result<Option<RotateLock>, String> {
    take_rotate_with(state_dir, flock_attempt, None)
}

/// 8.8's one exception, as an argument: the same non-blocking take, told the pid
/// of the worker this daemon has **just reaped** (7.3 step 4).
///
/// 8.8: "the daemon removes the lock file here when the record in it names the
/// worker it has just reaped. No liveness probe and no pid-reuse question arise,
/// because only the parent holds the exit status." So the exception is exactly
/// this argument and nothing more. Under 8.7's `excl_file` fallback, a file
/// whose record names `reaped_pid` is removed and the lock is taken in its
/// place; a file naming any other holder -- a live rotation, or the hand-run
/// worker of 5.5 step 3 -- is left exactly where it is, which is 5.5 step 1's
/// deferral and the conservative direction 8.8 asks for. Under `flock` the
/// argument changes nothing at all, because 8.8 keeps this case to the fallback
/// alone: there the kernel releases the lock when the holder exits and there is
/// nothing to judge (7.2).
pub fn take_rotate_after_reaping(
    state_dir: &Path,
    reaped_pid: u32,
) -> Result<Option<RotateLock>, String> {
    take_rotate_with(state_dir, flock_attempt, Some(reaped_pid))
}

/// The same, with the capability probe supplied: 8.7's `ENOTSUP` direction is
/// one no filesystem on this machine can produce, and under it "the file exists"
/// means "someone else holds the lock" rather than "we do". `reaped` is 8.8's
/// exception, [`take_rotate_after_reaping`]; `None` is every other caller.
fn take_rotate_with(
    state_dir: &Path,
    probe: impl Fn(&File) -> Result<Attempt, std::io::Error>,
    reaped: Option<u32>,
) -> Result<Option<RotateLock>, String> {
    create_private_dir(state_dir)?;
    create_private_dir(&state_dir.join(LOCK_DIR))?;
    let path = state_dir.join(ROTATE_FILE);

    match open_exclusive(&path) {
        // Nobody held it, so there is nothing to remove: the file being already
        // gone is the usual outcome of a fallback run, because the worker's own
        // guard removes it on the way out. 8.8's removal has no case to reach
        // here, and the lock is taken the way 8.7 defines it.
        Ok(file) => take_created(&path, file, &probe),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = open_existing(&path)?;
            let attempt = probe(&file).map_err(|error| {
                format!(
                    "cannot take the rotation lock at {}: {error}",
                    path.display()
                )
            })?;
            match select(attempt) {
                Some(Mode::Flock) => Ok(Some(RotateLock {
                    path,
                    file: Some(file),
                    mode: Mode::Flock,
                })),
                // 8.8: under the fallback the file's existence is the lock, so a
                // file this process did not create is a holder this process does
                // not take over -- with the one exception the parent is entitled
                // to make, the worker whose exit status it holds (7.3 step 4).
                // The record is what names that worker, so the exception is a
                // comparison of two pids and not a liveness probe.
                Some(Mode::ExclFile) if names_the_reaped_worker(&file, reaped) => {
                    drop(file);
                    remove_reaped_file(&path)?;
                    match open_exclusive(&path) {
                        Ok(file) => take_created(&path, file, &probe),
                        // The file is back and it is not the one just removed:
                        // another process took the lock in the window this
                        // removal opened, so this is the deferral it would have
                        // answered without 8.8's exception.
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
                        Err(error) => Err(format!("cannot create {}: {error}", path.display())),
                    }
                }
                Some(Mode::ExclFile) | None => Ok(None),
            }
        }
        Err(error) => Err(format!("cannot create {}: {error}", path.display())),
    }
}

/// The take that follows an exclusive create: the probe, 8.7's selection, and
/// the guard. Shared by the two creates this module can perform -- the one at
/// the top of [`take_rotate_with`], and the one 8.8's exception performs after
/// removing the reaped worker's file -- so both answer the same way.
fn take_created(
    path: &Path,
    file: File,
    probe: &impl Fn(&File) -> Result<Attempt, std::io::Error>,
) -> Result<Option<RotateLock>, String> {
    let attempt = probe(&file).map_err(|error| {
        format!(
            "cannot take the rotation lock at {}: {error}",
            path.display()
        )
    })?;
    match select(attempt) {
        Some(mode) => {
            // 8.7's fallback file "names the holder: its pid, and the platform's
            // own start time for that pid", and 8.7 says the fallback "covers
            // both locks of 7.2". This process created the file exclusively, so
            // nobody else can be holding it and the record is written before the
            // guard that would let a reader find it exists. Under `flock` no
            // record is written at all: the file is the kernel's lock and 8.8
            // has nothing to judge there.
            if mode == Mode::ExclFile {
                write_record(path, &file, std::process::id())?;
            }
            Ok(Some(RotateLock {
                path: path.to_path_buf(),
                file: Some(file),
                mode,
            }))
        }
        // A descriptor this process created exclusively cannot be held by anyone
        // else, so an OS that says otherwise is not a state to sweep in.
        None => Err(format!(
            "{} was created exclusively and still reads as held: not sweeping on that answer",
            path.display()
        )),
    }
}

/// 8.8's exception, as the one question it is: does the record in this file name
/// the worker this daemon has just reaped?
///
/// Both halves have to be there. A file with no readable record names nobody,
/// and a take that was given no reaped pid is no exception at all, which is why
/// this is one comparison over two `Option`s rather than a test on one of them:
/// a missing record must not match a missing pid.
fn names_the_reaped_worker(file: &File, reaped: Option<u32>) -> bool {
    match (read_record(file).pid, reaped) {
        (Some(recorded), Some(reaped)) => recorded == reaped,
        _ => false,
    }
}

/// The removal of 8.8's exception.
///
/// `ENOENT` is not a failure: the file was there when its record was read, so a
/// removal that finds nothing means another process removed it in the window
/// between the read and this call, and "already gone" is the state this step
/// wants either way. Nothing else is tolerated, because a removal that did not
/// happen would leave behind the file that makes the take defer.
fn remove_reaped_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot remove {}: {error}", path.display())),
    }
}

/// 8.7: "Cache root on a filesystem that does not support `flock`: the lock
/// file's `flock` returns `ENOTSUP`", and 4.2's `cache.root` comment makes it a
/// check: "config check refuses a root whose filesystem does not support flock,
/// and reports `lock_mode: excl_file` if a weaker lock had to be used".
///
/// The probe is a real file in the cache root's own `tmp/`, taken the way 8.4's
/// writability probe takes one (`tmp/<token>.part`; the pid stands in for the run
/// id, because a check is not a run and must not consume one), and it removes
/// what it wrote.
///
/// A probe that cannot be taken at all is not this refusal: 8.4 and 8.5 make an
/// unwritable cache root a reported degraded cache rather than a refusal, and
/// turning a permissions error into a claim about the filesystem's capabilities
/// would be asserting a fact this probe did not learn.
pub fn cache_root_accepts_flock(cache_dir: &Path) -> Result<(), String> {
    cache_root_accepts_flock_with(cache_dir, flock_attempt)
}

/// The same, with the capability probe supplied: 8.7's `ENOTSUP` direction is
/// one no filesystem on this machine can produce.
fn cache_root_accepts_flock_with(
    cache_dir: &Path,
    probe: impl Fn(&File) -> Result<Attempt, std::io::Error>,
) -> Result<(), String> {
    let tmp = cache_dir.join("tmp");
    if create_private_dir(&tmp).is_err() {
        return Ok(());
    }
    let path = tmp.join(format!("{}-flock-probe.part", std::process::id()));
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
    {
        Ok(file) => file,
        Err(_) => return Ok(()),
    };
    let attempt = probe(&file);
    let _ = std::fs::remove_file(&path);
    match attempt {
        // `Unsupported` is `ENOTSUP`/`EOPNOTSUPP`/`ENOSYS` (see `flock_attempt`):
        // a filesystem that cannot answer the call is a filesystem that cannot
        // lock, which is the refusal 8.7 asks for.
        Ok(Attempt::Unsupported) => Err(format!(
            "cache.root {}: this filesystem does not support flock (ENOTSUP), which 8.7 refuses rather than running on the weaker lock",
            cache_dir.display()
        )),
        // `Acquired` is the answer the check wants, and `Held` (another probe
        // holding this file) still proves the filesystem can lock.
        Ok(_) | Err(_) => Ok(()),
    }
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
    use std::os::unix::fs::MetadataExt;

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

    /// 5.5 step 1 and 7.3 step 2 are the same primitive with two readers: the
    /// second `take_rotate` on one state directory answers `Ok(None)` -- "someone
    /// else holds it", which is a deferral for the sweep and `busy` for a worker
    /// -- and the same call answers `Ok(Some(..))` once the holder drops, because
    /// 7.2's primitive is the one the kernel releases [L 3].
    ///
    /// A `take_rotate` that leaked the lock to a second taker, or that kept
    /// refusing after the holder was gone, fails one of the two directions.
    #[test]
    fn the_rotation_lock_defers_to_a_second_holder_and_is_free_after_the_drop() {
        let dir = scratch("rotate");
        let holder = take_rotate(&dir)
            .expect("a take that cannot fail")
            .expect("the lock is free to begin with");
        assert!(
            take_rotate(&dir)
                .expect("a take that cannot fail")
                .is_none(),
            "5.5 step 1: a lock already held is a deferral, not a wait"
        );

        drop(holder);
        assert!(
            take_rotate(&dir)
                .expect("a take that cannot fail")
                .is_some(),
            "7.2: the release is the kernel's when the holder drops"
        );
        assert!(
            dir.join(ROTATE_FILE).is_file(),
            "the lock file is where 7.2's table puts it: {ROTATE_FILE}"
        );
    }

    /// 8.7's fallback direction, which no filesystem on this machine produces:
    /// there the file's *existence* is the lock, so a file this process did not
    /// create is a holder, and `take_rotate` defers to it rather than taking it
    /// over. That is 8.8's refusal, and it is what makes a `sweep_deferred: 1`
    /// reachable on such a filesystem.
    #[test]
    fn a_lock_file_this_process_did_not_create_is_a_holder_under_the_fallback() {
        let dir = scratch("rotate-fallback");
        create_private_dir(&dir).expect("the state directory");
        create_private_dir(&dir.join(LOCK_DIR)).expect("locks/");
        std::fs::write(dir.join(ROTATE_FILE), "pid 4711 start 1790000000\n")
            .expect("a planted lock file");

        let deferred = take_rotate_with(&dir, |_| Ok(Attempt::Unsupported), None)
            .expect("a take that cannot fail");
        assert!(
            deferred.is_none(),
            "8.8: the file's existence is the holder"
        );
        assert!(
            dir.join(ROTATE_FILE).is_file(),
            "and a lock this process did not create is not removed"
        );
    }

    /// The record of 8.7, as a holder writes it: the pid and the start time.
    fn plant_lock(state_dir: &Path, pid: u32) -> PathBuf {
        create_private_dir(state_dir).expect("the state directory");
        create_private_dir(&state_dir.join(LOCK_DIR)).expect("locks/");
        let path = state_dir.join(ROTATE_FILE);
        std::fs::write(&path, format!("pid: {pid}\nstart: 2026-09-26T06:00:00Z\n"))
            .expect("a planted lock file");
        path
    }

    /// 7.3 step 4 and 8.8's one exception, in the fallback's own direction: the
    /// `rotate.lock` of the worker this daemon has just reaped is removed, and
    /// the daemon becomes the holder in its place -- which is what lets the sweep
    /// that follows every rotation run at all on a filesystem that cannot
    /// `flock`, instead of deferring behind a holder that is gone.
    ///
    /// The record is what authorises the removal, and the inode is what proves
    /// it: the file at that path after the take is not the worker's file.
    #[test]
    fn the_reaped_workers_lock_file_is_removed_under_the_fallback() {
        let dir = scratch("reaped-worker");
        let worker = 4711;
        let path = plant_lock(&dir, worker);
        // The worker's own file, held open across the take. What the removal
        // leaves behind is this descriptor's inode with no directory entry left
        // (`nlink` 0), which is also what tells a removal apart from the
        // truncation 8.8 forbids. An inode *number* is not evidence: ext4 and
        // tmpfs hand the same number straight back to the file that replaces it.
        let left_behind = File::open(&path).expect("the worker's lock file");

        let guard = take_rotate_with(&dir, |_| Ok(Attempt::Unsupported), Some(worker))
            .expect("a take that cannot fail")
            .expect("8.8: the reaped worker's file is not a holder");

        assert_eq!(guard.mode, Mode::ExclFile);
        assert!(
            path.is_file(),
            "7.3 step 4: the daemon holds the lock now, so its file is there"
        );
        assert_eq!(
            left_behind.metadata().expect("the worker's file").nlink(),
            0,
            "the reaped worker's file is gone; this descriptor is all that is left of it"
        );
        // 8.7 says the fallback file names its holder, so the record that is there
        // is the daemon's own and not the worker's.
        let record = read_record(&File::open(&path).expect("the daemon's lock file"));
        assert_eq!(record.pid, Some(std::process::id()), "{record:?}");

        drop(guard);
        assert!(
            !path.exists(),
            "8.7: releasing the fallback's lock removes the file"
        );
    }

    /// The same question with no file to remove: a worker that exited cleanly
    /// removed its own file on the way out (8.7's release), so the fallback take
    /// finds none and creates one, record and all. 8.8's removal has no case for
    /// "the lock file is already gone" to reach, and this is that path.
    #[test]
    fn a_reaping_take_with_no_lock_file_creates_one() {
        let dir = scratch("reaped-worker-gone");
        create_private_dir(&dir).expect("the state directory");

        let guard = take_rotate_with(&dir, |_| Ok(Attempt::Unsupported), Some(4711))
            .expect("a take that cannot fail")
            .expect("no file existed, so nothing held the lock");

        assert_eq!(guard.mode, Mode::ExclFile);
        let path = dir.join(ROTATE_FILE);
        assert!(path.is_file(), "7.3 step 4's take created it");
        assert_eq!(
            read_record(&File::open(&path).expect("the lock file")).pid,
            Some(std::process::id()),
            "and 8.7's record names the holder that created it"
        );
    }

    /// The conservative direction 8.8 asks for, on the same fallback: a file
    /// whose record names *any other* holder -- a live rotation, or 5.5 step 3's
    /// hand-run worker -- is left exactly where it is, and the take answers the
    /// deferral it would have answered without the exception. The file is the
    /// same file afterwards, not a re-created one.
    #[test]
    fn a_lock_file_naming_another_holder_survives_the_reaping_take() {
        let dir = scratch("other-holder");
        let path = plant_lock(&dir, 4242);
        let left_behind = File::open(&path).expect("the planted lock file");

        let deferred = take_rotate_with(&dir, |_| Ok(Attempt::Unsupported), Some(4711))
            .expect("a take that cannot fail");
        assert!(
            deferred.is_none(),
            "8.8: a holder other than the reaped worker is not taken from"
        );
        assert_eq!(
            left_behind
                .metadata()
                .expect("the planted lock file")
                .nlink(),
            1,
            "and its file is untouched: still linked, neither removed nor replaced"
        );
        assert!(
            std::fs::read_to_string(&path)
                .expect("the planted lock file")
                .contains("pid: 4242"),
            "its record is untouched too"
        );
    }

    /// The same with no record to read: a lock file left by a holder that
    /// recorded nothing names nobody, so it is not the reaped worker's and it is
    /// not removed. This is the case the two `Option`s have to match on `Some`
    /// for: a file with no pid must not be the reaped worker of a take that was
    /// told a pid.
    #[test]
    fn a_lock_file_with_no_record_is_not_the_reaped_workers() {
        let dir = scratch("no-record");
        create_private_dir(&dir).expect("the state directory");
        create_private_dir(&dir.join(LOCK_DIR)).expect("locks/");
        let path = dir.join(ROTATE_FILE);
        std::fs::write(&path, "").expect("a lock file with no record");

        assert!(
            take_rotate_with(&dir, |_| Ok(Attempt::Unsupported), Some(4711))
                .expect("a take that cannot fail")
                .is_none(),
            "8.8: the record has to name the reaped worker, and this one names nobody"
        );
        assert!(path.is_file(), "so the file stays");
    }

    /// The other half of the same match, and the direction 8.8 does not grant: a
    /// take that was given **no** reaped pid has no exception to make at all, so a
    /// fallback file whose record *is* readable names a holder and stays. A
    /// missing pid is not a wildcard. The record planted here is 8.7's own, prefix
    /// and all, which is the only thing that separates this case from the reaped
    /// worker's file: the argument the take was not given, and nothing about the
    /// bytes on disk.
    #[test]
    fn a_readable_record_survives_a_take_with_no_reaped_pid() {
        let dir = scratch("no-reaped-pid");
        let path = plant_lock(&dir, 4242);
        let planted = std::fs::read_to_string(&path).expect("the planted lock file");
        let left_behind = File::open(&path).expect("the planted lock file");

        let deferred = take_rotate_with(&dir, |_| Ok(Attempt::Unsupported), None)
            .expect("a take that cannot fail");

        assert!(
            deferred.is_none(),
            "5.5 step 1: with no reaped worker to name, a readable record is a holder"
        );
        assert_eq!(
            left_behind
                .metadata()
                .expect("the planted lock file")
                .nlink(),
            1,
            "and its file is untouched: still linked, neither removed nor replaced"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("the planted lock file"),
            planted,
            "its record is the one 8.7's holder wrote, byte for byte"
        );
    }

    /// 8.8 keeps the exception to the fallback alone, and this is the other
    /// primitive's answer: under `flock` the kernel releases the lock when the
    /// holder exits, so a file that reads as *held* is a live holder and not a
    /// leftover -- it is not removed even when the take is handed that very
    /// process's pid. The holder here is this test process's own `flock`, which is
    /// the shape of 5.5 step 3's hand-run worker.
    #[test]
    fn a_held_flock_is_never_removed_by_the_reaping_take() {
        let dir = scratch("held-flock");
        let holder = take_rotate(&dir)
            .expect("a take that cannot fail")
            .expect("the lock is free to begin with");

        assert!(
            take_rotate_after_reaping(&dir, std::process::id())
                .expect("a take that cannot fail")
                .is_none(),
            "a live holder is a deferral, not something to remove"
        );
        assert!(
            dir.join(ROTATE_FILE).is_file(),
            "and the live holder's file is still there"
        );
        drop(holder);
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

    /// The other half of the same derivation: under `flock` there is nothing to
    /// report, which is why 2.5's `config check` body is unchanged there.
    #[test]
    fn the_flock_primitive_reports_no_extra_line() {
        let dir = scratch("flock-report");
        let lock = take(&dir).expect("the lock");
        assert_eq!(lock.mode(), Mode::Flock);
        assert_eq!(lock.report_line(), None);
    }

    /// 8.7's cache-root refusal, in both directions. The real probe runs on this
    /// machine's filesystem (which can `flock`); the `ENOTSUP` direction is the
    /// one APFS cannot produce, so it is supplied.
    #[test]
    fn the_cache_root_check_refuses_only_a_filesystem_that_cannot_flock() {
        let dir = scratch("cache-root");
        assert_eq!(
            cache_root_accepts_flock(&dir),
            Ok(()),
            "the filesystem this test runs on can flock"
        );
        let left_behind: Vec<_> = std::fs::read_dir(dir.join("tmp"))
            .expect("the probe's directory")
            .map(|entry| entry.expect("a directory entry").file_name())
            .collect();
        assert!(
            left_behind.is_empty(),
            "8.4's probe removes what it wrote: {left_behind:?}"
        );

        let refused = cache_root_accepts_flock_with(&dir, |_| Ok(Attempt::Unsupported))
            .expect_err("a filesystem without flock is refused");
        assert!(refused.contains("cache.root"), "{refused}");
        assert!(refused.contains("ENOTSUP"), "{refused}");
        assert!(refused.contains(&dir.display().to_string()), "{refused}");
    }

    /// A probe that cannot be taken at all is not the 8.7 refusal: 8.4 and 8.5
    /// make an unwritable cache root a reported degraded state, not a refusal.
    #[test]
    fn a_cache_root_that_cannot_be_probed_is_not_refused() {
        let dir = scratch("cache-root-missing");
        let missing = dir.join("nowhere").join("cache");
        assert_eq!(cache_root_accepts_flock(&missing), Ok(()));
    }
}
