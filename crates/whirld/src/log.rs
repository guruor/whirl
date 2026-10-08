//! The daemon's own log file and its cap (docs/spec/state-and-cache.md 1.1,
//! docs/architecture.md 4.2's `log_max_bytes`).
//!
//! The daemon does not open this file. The supervisor does, and hands the daemon
//! the descriptor as its standard output and standard error, so every line the
//! daemon writes lands here and the daemon's own writes go through that same
//! descriptor. Two facts about that descriptor fix the strategy:
//!
//! - It is `O_APPEND`. launchd opens `StandardOutPath` that way (it has no
//!   rotation key at all, and a job's descriptor carries `AP`), so the next line
//!   written lands at the file's *current* end and not at an offset the
//!   descriptor recorded earlier.
//! - The descriptor therefore follows the file, not the other way round: a
//!   rotation that renames the inode out from under it leaves it appending to
//!   the renamed file, so the live log would stay empty while the archive grew.
//!
//! So the file is rewritten **in place**, at line boundaries, never rotated. The
//! inode survives, which is what makes the supervisor's next append land on the
//! kept text. The requirement this puts on a reader's setup is only that the
//! descriptor appends: a `>` redirect that does not append would write at its
//! stale offset after a trim and leave a NUL hole, which is why the harness in
//! `crates/whirld/tests/control_socket.rs` opens the daemon's log with
//! `append(true)` rather than `File::create`. A file with NUL bytes in it is
//! never produced by the arrangement above.
//!
//! A cap of `0` keeps everything and only ever means that (docs/architecture.md
//! 4.3); any other cap is at least `LOG_CAP_MIN_BYTES` and leaves `RESERVE_BYTES`
//! of itself free, so the lines a rotation writes after the check still fit.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

/// How much of the cap a trim leaves free.
///
/// A trim runs at the end of a rotation, so the lines still to be written after
/// the check are the outcome line its caller logs and the one line the trim logs
/// itself. Both are single lines, and the widest line the daemon writes measured
/// 484 bytes (the plan line) on a real install, so this much free space is what
/// keeps the cap from being passed by the lines that report the rotation.
const RESERVE_BYTES: u64 = 2048;

/// What one trim took out, in bytes.
#[derive(Debug, PartialEq, Eq)]
pub struct Trim {
    pub before: u64,
    pub after: u64,
    pub dropped: u64,
}

/// What `enforce` found and did.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The cap is `0`: keep everything, by the configuration's own statement.
    Disabled,
    /// At or under the cap's budget; the file was not touched.
    Kept,
    /// Rewritten in place, keeping the newest whole lines.
    Trimmed(Trim),
    /// The file could not be examined or rewritten. The daemon says so and
    /// carries on; a log it cannot bound is worse than a log it can, not fatal.
    Failed(String),
}

/// Hold the log at or under `cap` bytes.
///
/// The oldest whole lines go first, and the kept text always begins at a line
/// start, so a reader never meets a half line at the trim point. A single
/// trailing line longer than the budget is kept whole rather than cut, which can
/// leave the file over `cap` by that one line; the cap's floor is set so that no
/// line the daemon writes comes near it.
pub fn enforce(path: &Path, cap: u64) -> Outcome {
    if cap == 0 {
        return Outcome::Disabled;
    }
    let before = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata.len(),
        // A directory where the log should be, or no file at all: there is
        // nothing here to bound.
        Ok(_) => return Outcome::Kept,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Outcome::Kept,
        Err(error) => return Outcome::Failed(format!("{}: {error}", path.display())),
    };
    let budget = cap.saturating_sub(RESERVE_BYTES);
    if before <= budget {
        return Outcome::Kept;
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => return Outcome::Failed(format!("{}: {error}", path.display())),
    };
    let kept = &bytes[keep_from(&bytes, budget)..];
    if kept.len() == bytes.len() {
        // Nothing to drop: the file is one line or one tail with no line start
        // to cut back to, so it is left alone rather than rewritten.
        return Outcome::Kept;
    }
    if let Err(error) = rewrite(path, kept) {
        return Outcome::Failed(format!("{}: {error}", path.display()));
    }
    Outcome::Trimmed(Trim {
        before,
        after: kept.len() as u64,
        dropped: before - kept.len() as u64,
    })
}

/// Hold the log at or under `cap`, and say in the log what happened when it had
/// to trim (docs/spec/state-and-cache.md 1.1). Silent when nothing was needed.
pub fn enforce_reporting(path: &Path, cap: u64) {
    match enforce(path, cap) {
        Outcome::Trimmed(trim) => eprintln!(
            "whirld: log trimmed: {} -> {} bytes, {} bytes dropped, cap {cap}",
            trim.before, trim.after, trim.dropped
        ),
        Outcome::Failed(message) => eprintln!("whirld: warning: cannot trim the log: {message}"),
        Outcome::Disabled | Outcome::Kept => {}
    }
}

/// The offset the kept text starts at: the first whole line whose tail fits the
/// budget, or the start of the last line when that one line is itself longer than
/// the budget, or `0` when the file is one line with no newline at all.
fn keep_from(bytes: &[u8], budget: u64) -> usize {
    let budget = usize::try_from(budget).unwrap_or(usize::MAX);
    if bytes.len() <= budget {
        return 0;
    }
    let oldest = bytes.len() - budget;
    match bytes[oldest..].iter().position(|byte| *byte == b'\n') {
        Some(offset) => oldest + offset + 1,
        None => bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(0),
    }
}

/// Replace the file's contents without replacing the file.
///
/// `File::create` truncates an existing file rather than unlinking it, so the
/// inode, and with it the supervisor's descriptor, survives. A crash between the
/// truncate and the write leaves an empty log rather than a torn one, which is
/// the reader-protecting half of the trade.
fn rewrite(path: &Path, kept: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(kept)?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use whirl_core::config::LOG_CAP_MIN_BYTES;

    /// A scratch file this test owns, named after the test so two tests in the
    /// same process cannot collide.
    fn scratch(name: &str, contents: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("whirld-log-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join(name);
        fs::write(&path, contents).expect("scratch file");
        path
    }

    fn read(path: &Path) -> Vec<u8> {
        let mut buffer = Vec::new();
        File::open(path)
            .expect("open")
            .read_to_end(&mut buffer)
            .expect("read");
        buffer
    }

    fn lines(prefix: &str, count: usize, width: usize) -> Vec<u8> {
        let mut text = String::new();
        for index in 0..count {
            text.push_str(&format!("{prefix}{index:0width$}\n"));
        }
        text.into_bytes()
    }

    #[test]
    fn a_trim_keeps_the_newest_whole_lines_and_starts_on_a_line_start() {
        // 400 lines of 10 bytes: 4000 bytes, and a budget of 1000 lands inside
        // them, so there is a line start to cut back to.
        let contents = lines("line-", 400, 4);
        let path = scratch("keeps-newest.log", &contents);
        let cap = RESERVE_BYTES + 1000;

        let outcome = enforce(&path, cap);
        let Outcome::Trimmed(trim) = outcome else {
            panic!("expected a trim, got {outcome:?}");
        };
        assert_eq!(trim.before, contents.len() as u64);
        assert!(trim.after <= 1000, "kept {} bytes", trim.after);
        assert_eq!(trim.dropped, trim.before - trim.after);

        let kept = read(&path);
        assert_eq!(kept.len() as u64, trim.after);
        assert!(
            contents.ends_with(&kept),
            "the newest lines are the ones kept"
        );
        assert_eq!(
            kept.last(),
            Some(&b'\n'),
            "the kept text ends at a line end"
        );
        let text = String::from_utf8(kept).expect("utf8");
        assert!(
            text.starts_with("line-"),
            "a kept line is whole, not a tail: {:?}",
            &text[..text.len().min(12)]
        );
    }

    #[test]
    fn a_zero_cap_keeps_everything_and_says_so_without_touching_the_file() {
        let contents = lines("line-", 400, 4);
        let path = scratch("zero-keeps.log", &contents);
        assert_eq!(enforce(&path, 0), Outcome::Disabled);
        assert_eq!(read(&path), contents);
        assert_eq!(
            fs::metadata(&path).expect("stat").len(),
            contents.len() as u64
        );
    }

    #[test]
    fn a_log_under_the_cap_is_not_touched() {
        let contents = b"whirld: one\nwhirld: two\n".to_vec();
        let path = scratch("under.log", &contents);
        assert_eq!(enforce(&path, LOG_CAP_MIN_BYTES), Outcome::Kept);
        assert_eq!(read(&path), contents);
    }

    #[test]
    fn a_log_that_is_not_there_is_kept_and_not_created() {
        let dir = std::env::temp_dir().join(format!("whirld-log-absent-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("nothing.log");
        let _ = fs::remove_file(&path);
        assert_eq!(enforce(&path, LOG_CAP_MIN_BYTES), Outcome::Kept);
        assert!(!path.exists(), "the trim does not create the log");
    }

    #[test]
    fn a_single_line_longer_than_the_budget_is_kept_whole() {
        // One line, no newline at all: there is no line start to cut back to, so
        // the file is left as it is rather than cut mid-line.
        let contents = vec![b'x'; 20_000];
        let path = scratch("one-long-line.log", &contents);
        assert_eq!(enforce(&path, LOG_CAP_MIN_BYTES), Outcome::Kept);
        assert_eq!(read(&path), contents);
    }

    #[test]
    fn a_trailing_long_line_after_a_newline_is_kept_whole() {
        // The last line is longer than the budget and the file has no trailing
        // newline, so the kept text is that one line: over the budget, but whole,
        // and it is the newest thing in the file.
        let mut contents = lines("line-", 200, 4);
        contents.extend(std::iter::repeat_n(b'y', 9_000));
        let path = scratch("long-tail.log", &contents);
        let outcome = enforce(&path, LOG_CAP_MIN_BYTES);
        let Outcome::Trimmed(trim) = outcome else {
            panic!("expected a trim, got {outcome:?}");
        };
        assert!(trim.after > RESERVE_BYTES, "the long line is kept");
        assert_eq!(
            read(&path),
            contents[contents.len() - trim.after as usize..].to_vec()
        );
        assert!(
            read(&path).ends_with(&vec![b'y'; 9_000]),
            "the long line survived whole"
        );
    }

    #[test]
    fn the_trim_keeps_the_inode_so_a_held_descriptor_follows_the_new_end() {
        let contents = lines("line-", 400, 4);
        let path = scratch("inode.log", &contents);
        let before = fs::metadata(&path).expect("stat").len();

        // A supervisor's descriptor: opened once, held across the trim, appended
        // to afterwards. This is what launchd does with the daemon's log.
        let mut held = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("held descriptor");

        let cap = RESERVE_BYTES + 1000;
        let Outcome::Trimmed(trim) = enforce(&path, cap) else {
            panic!("expected a trim");
        };
        assert_eq!(trim.before, before);

        held.write_all(b"whirld: the next line\n").expect("append");
        held.flush().expect("flush");

        let appended = "whirld: the next line\n".len() as u64;
        let after = read(&path);
        assert_eq!(
            after.len() as u64,
            trim.after + appended,
            "the append landed at the new end, not at a hole at the old offset"
        );
        assert!(after.ends_with(b"whirld: the next line\n"));
        assert!(
            !after.contains(&0),
            "an append at a stale offset would leave NUL padding"
        );
    }

    #[test]
    fn the_report_line_fits_the_reserve_it_leaves_free() {
        // The trim leaves `budget` free; the line it writes itself has to fit in
        // the rest, or the cap would be passed by its own report.
        let contents = lines("line-", 400, 4);
        let path = scratch("report-fits.log", &contents);
        let cap = RESERVE_BYTES + 1000;
        let Outcome::Trimmed(trim) = enforce(&path, cap) else {
            panic!("expected a trim");
        };
        // `enforce_reporting` writes this line; its length is what must fit.
        let report = format!(
            "whirld: log trimmed: {} -> {} bytes, {} bytes dropped, cap {cap}\n",
            trim.before, trim.after, trim.dropped
        );
        assert!(
            trim.after + report.len() as u64 <= cap,
            "{} + {} exceeds {cap}",
            trim.after,
            report.len()
        );
    }
}
