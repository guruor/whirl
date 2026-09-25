//! The state layer: the three files of `docs/spec/state-and-cache.md` section 6.
//!
//! Reading is total and never panics: a file that does not exist is a first run,
//! a file that cannot be read or parsed is quarantined and rebuilt (6.4), and a
//! file whose `schema` is newer than this build is left exactly as it is and
//! becomes read-only for this daemon (6.4 step 4). Nothing here writes a partial
//! file: the write protocol of 6.3 is a temp file in the same directory, `fsync`,
//! then `rename`.
//!
//! The schemas themselves live in `whirl-core::state`, beside the protocol they
//! encode, so the daemon owns the I/O and the core owns the shape.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use whirl_core::state::{CURRENT_FILE, FAVORITES_FILE, HISTORY_FILE};

/// Which state file. A caller passes this rather than a path, so no call can
/// reach outside the state directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Current,
    History,
    Favorites,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Current => CURRENT_FILE,
            Kind::History => HISTORY_FILE,
            Kind::Favorites => FAVORITES_FILE,
        }
    }

    /// The index into a per-file flag array.
    pub fn index(self) -> usize {
        match self {
            Kind::Current => 0,
            Kind::History => 1,
            Kind::Favorites => 2,
        }
    }
}

/// The state directory, and the only thing that writes to it.
pub struct Store {
    dir: PathBuf,
}

/// A write that has reached the disk but is not visible yet: 6.3 steps 2 and 3
/// are done, steps 4 and 5 are not. Dropping it leaves the target file exactly
/// as it was, which is what makes "killed between temp and rename" a crash the
/// state layer survives rather than a truncated state file.
#[derive(Debug)]
pub struct Staged {
    temp: PathBuf,
    target: PathBuf,
    dir: PathBuf,
    committed: bool,
}

impl Store {
    pub fn new(dir: &Path) -> Store {
        Store {
            dir: dir.to_path_buf(),
        }
    }

    pub fn path(&self, kind: Kind) -> PathBuf {
        self.dir.join(kind.name())
    }

    /// The file's text, `None` when it does not exist.
    ///
    /// A read that fails for any other reason (`EISDIR` for a directory where a
    /// file should be, `EACCES`) is an error the caller quarantines: 6.4 treats
    /// "cannot read it" and "cannot parse it" as one case, because both mean the
    /// daemon has no trustworthy value.
    pub fn read(&self, kind: Kind) -> Result<Option<String>, String> {
        let path = self.path(kind);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("cannot read {}: {error}", path.display())),
        }
    }

    /// 6.3 steps 1 to 3: serialise, write the temp file in the same directory,
    /// `fsync` it. Nothing is visible under the target name yet.
    pub fn stage(&self, kind: Kind, text: &str) -> Result<Staged, String> {
        let target = self.path(kind);
        let temp = self.dir.join(format!(
            "{}.tmp-{}-{}",
            kind.name(),
            std::process::id(),
            unique()
        ));
        let mut file = fs::File::create(&temp)
            .map_err(|error| format!("cannot create {}: {error}", temp.display()))?;
        // The state directory is 0700, so the file is already out of reach of
        // other users; this is the same belt as `plan::restrict_to_owner` wears
        // for the config, and it costs one call.
        restrict(&temp);
        file.write_all(text.as_bytes())
            .map_err(|error| format!("cannot write {}: {error}", temp.display()))?;
        file.sync_all()
            .map_err(|error| format!("cannot fsync {}: {error}", temp.display()))?;
        Ok(Staged {
            temp,
            target,
            dir: self.dir.clone(),
            committed: false,
        })
    }

    /// The whole protocol: stage, then rename.
    pub fn write(&self, kind: Kind, text: &str) -> Result<(), String> {
        self.stage(kind, text)?.commit()
    }

    /// 6.4 step 1: move the file that could not be read out of the way, in
    /// place, and never delete it. Only the newest quarantine per file is kept,
    /// so a repeated failure cannot grow without bound (R4).
    ///
    /// A directory where a state file belongs is renamed the same way, because a
    /// directory is one of the three shapes 6.4 has to survive.
    pub fn quarantine(&self, kind: Kind, now: i64) -> Result<PathBuf, String> {
        let source = self.path(kind);
        let stamp = timestamp_for_path(now);
        let destination = self.dir.join(format!("{}.corrupt-{stamp}", kind.name()));
        if let Err(error) = fs::rename(&source, &destination) {
            return Err(format!(
                "cannot quarantine {} to {}: {error}",
                source.display(),
                destination.display()
            ));
        }
        // "older ones are removed when a new one is created": the newest is the
        // one just written, and anything else with this prefix is superseded.
        if let Ok(entries) = fs::read_dir(&self.dir) {
            let prefix = format!("{}.corrupt-", kind.name());
            for entry in entries.flatten() {
                let path = entry.path();
                if path != destination {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with(&prefix) {
                        let _ = remove_any(&path);
                    }
                }
            }
        }
        Ok(destination)
    }
}

impl Staged {
    /// 6.3 steps 4 and 5: rename the temp file over the target, then `fsync` the
    /// directory best-effort, because directory `fsync` is not uniformly
    /// supported on macOS (`EINVAL`/`ENOTSUP`) and a failure there is not a
    /// failed write.
    pub fn commit(mut self) -> Result<(), String> {
        fs::rename(&self.temp, &self.target).map_err(|error| {
            format!(
                "cannot rename {} to {}: {error}",
                self.temp.display(),
                self.target.display()
            )
        })?;
        self.committed = true;
        if let Ok(directory) = fs::File::open(&self.dir) {
            let _ = directory.sync_all();
        }
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.temp);
        }
    }
}

/// `<name>.corrupt-<UTC timestamp>`: RFC 3339 with the colons replaced, because a
/// state directory is a directory on Windows too.
pub fn timestamp_for_path(seconds: i64) -> String {
    whirl_core::protocol::rfc3339_utc(seconds).replace(':', "-")
}

/// A suffix no second writer in this process repeats: the pid and a nanosecond
/// clock, plus a counter for two writes inside one nanosecond.
fn unique() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.subsec_nanos())
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:x}{:x}", nanos, counter)
}

#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

/// `remove_file`, and `remove_dir` for the directory-shaped case of 6.4.
fn remove_any(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "whirl-statefile-{}-{}-{name}",
            std::process::id(),
            unique()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    #[test]
    fn a_write_is_reachable_only_after_the_rename() {
        // 6.3 steps 2 to 4. The case that matters is the one between the fsync
        // and the rename: the old file has to survive it byte for byte.
        let dir = scratch("atomic");
        let store = Store::new(&dir);
        store
            .write(
                Kind::Current,
                "{\"schema\": 1, \"seq\": 1, \"paused\": false}\n",
            )
            .expect("the first write");
        let before = fs::read_to_string(store.path(Kind::Current)).expect("the first file");

        let staged = store
            .stage(
                Kind::Current,
                "{\"schema\": 1, \"seq\": 2, \"paused\": true}\n",
            )
            .expect("a staged write");
        assert!(
            staged_path_exists(&dir, "current.json.tmp-"),
            "the temp file exists, in the same directory, so the rename is same-filesystem (6.3 step 2)"
        );
        assert_eq!(
            fs::read_to_string(store.path(Kind::Current)).expect("the target"),
            before,
            "the target is untouched before the rename"
        );
        // The write dies here: no rename happens.
        drop(staged);
        assert_eq!(
            fs::read_to_string(store.path(Kind::Current)).expect("the target"),
            before,
            "the old state survived the write that did not commit"
        );
        assert!(
            !staged_path_exists(&dir, "current.json.tmp-"),
            "a dropped stage leaves no temp file behind"
        );

        // The same write, allowed to commit, replaces the file whole.
        store
            .write(
                Kind::Current,
                "{\"schema\": 1, \"seq\": 2, \"paused\": true}\n",
            )
            .expect("the committed write");
        let after = fs::read_to_string(store.path(Kind::Current)).expect("the new file");
        assert!(after.contains("\"seq\": 2"));
        assert!(
            !after.contains("\"seq\": 1"),
            "no partial overwrite: {after}"
        );
    }

    #[test]
    fn the_newest_quarantine_survives_and_the_older_one_does_not() {
        // 6.4 step 1: keep only the newest quarantine per file.
        let dir = scratch("quarantine");
        let store = Store::new(&dir);
        fs::write(store.path(Kind::History), "not json at all").expect("a broken file");
        let first = store
            .quarantine(Kind::History, 1_790_324_533)
            .expect("a quarantine");
        assert_eq!(
            first.file_name().expect("a name").to_string_lossy(),
            "history.json.corrupt-2026-09-25T08-22-13Z",
            "the quarantine path carries the UTC timestamp"
        );
        assert_eq!(
            fs::read_to_string(&first).expect("the quarantined bytes"),
            "not json at all",
            "the file is moved, never deleted"
        );
        fs::write(store.path(Kind::History), "broken again").expect("a second broken file");
        let second = store
            .quarantine(Kind::History, 1_790_328_000)
            .expect("a second quarantine");
        assert!(second.exists());
        assert!(
            !first.exists(),
            "only the newest quarantine per file is kept (6.4 step 1)"
        );
        assert!(!store.path(Kind::History).exists(), "the target is gone");
    }

    #[test]
    fn a_directory_where_a_file_belongs_is_quarantined_rather_than_fatal() {
        // `read_to_string` on a directory is `EISDIR`; 6.4's answer is the same
        // as for a truncated file, and the rename has to cope with a directory.
        let dir = scratch("directory");
        let store = Store::new(&dir);
        fs::create_dir(store.path(Kind::Current)).expect("a directory in the file's place");
        assert!(store.read(Kind::Current).is_err(), "reading it is an error");
        let moved = store
            .quarantine(Kind::Current, 1_790_324_533)
            .expect("a directory can be renamed");
        assert!(moved.is_dir(), "the directory was moved, not deleted");
        assert!(!store.path(Kind::Current).exists());
        store
            .write(Kind::Current, "{\"schema\": 1}\n")
            .expect("the file can be rebuilt");
        assert!(store.path(Kind::Current).is_file());
    }

    #[test]
    fn an_absent_file_is_a_first_run_and_not_an_error() {
        let dir = scratch("absent");
        let store = Store::new(&dir);
        assert_eq!(store.read(Kind::Favorites).expect("no error"), None);
    }

    fn staged_path_exists(dir: &Path, prefix: &str) -> bool {
        fs::read_dir(dir)
            .expect("a directory")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
    }
}
