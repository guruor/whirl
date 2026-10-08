//! The macOS half of `whirl daemon ...`: a per-user `LaunchAgent`, the unit
//! docs/architecture.md 5.2 specifies.
//!
//! The unit is whirl's and whirl writes it. No release archive carries a plist,
//! because a plist holds one machine's absolute paths; this module builds one
//! from the home directory the config module already resolves and the installed
//! `whirld` beside this `whirl`. Every step then goes through `launchctl`, so
//! the supervisor owns the daemon's lifetime and nothing here ever spawns one
//! (docs/architecture.md 1.5, section 8).
//!
//! The keys are the ones 5.2 decided and no others: `RunAtLoad` and `KeepAlive`,
//! the two log paths, and the program. There is deliberately no `StartInterval`
//! and no `StartCalendarInterval`, because launchd's interval is floored by
//! `ThrottleInterval` and loses its firing across sleep, so the rotation clock
//! stays the daemon's own persisted deadline (5.1, 5.5). There is no
//! `EnvironmentVariables` block either: the daemon resolves its own paths and
//! the worker's environment is 1.6's business, so a unit that pinned one
//! machine's environment would be a unit that is not portable.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output};

use whirl_core::config::paths;

use super::Verb;
use crate::render::{EXIT_OK, EXIT_REFUSED, EXIT_UNREACHABLE};

/// The launchd label of the daemon's unit, which is also the job name every
/// `launchctl` command here answers to. The reverse-DNS form is the one
/// docs/architecture.md 1.2 fixes for a platform that demands one (`com.guruor.
/// whirl` as the launchd label).
pub(super) const LABEL: &str = "com.guruor.whirl";

/// `launchctl(1)`, named by its own name. It is the platform's own supervisor
/// command and the install recipe docs/research/scheduling.md Part 3 measured is
/// written that way, and naming it from `PATH` is also what lets a test (or a
/// sandbox run) put a stand-in first: the unit is then exercised without
/// registering this product's label in the login session the machine is using.
const LAUNCHCTL: &str = "launchctl";

/// `plist(5)`'s own byte order and document type. launchd refuses a unit whose
/// header is wrong rather than reading it leniently.
const PLIST_HEADER: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
    "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
    "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">",
);

/// The one command line a frontend's request reaches from here.
pub(super) fn run(verb: Verb) -> ExitCode {
    match verb {
        Verb::Install => install(),
        Verb::Uninstall => uninstall(),
        Verb::Start => start(),
        Verb::Stop => stop(),
        Verb::Status => status(),
    }
}

/// A refusal is the CLI's own exit 1, with the reason on the stream 2.5.1 gives
/// a refusal.
fn refuse(message: String) -> ExitCode {
    eprintln!("whirl: {message}");
    ExitCode::from(EXIT_REFUSED)
}

/// `~/Library/LaunchAgents/com.guruor.whirl.plist`: the unit 5.2 names.
///
/// The file name is the label in one place, so the file a reader is sent to and
/// the job `launchctl` knows cannot drift apart.
fn unit_file() -> Result<PathBuf, String> {
    paths::home()
        .map(|home| {
            home.join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist"))
        })
        .ok_or_else(no_home)
}

fn no_home() -> String {
    "$HOME is unset or not an absolute path, so the per-user LaunchAgents and Logs \
     directories cannot be named"
        .to_string()
}

/// The parent directory of a path this module builds. Every path here is built
/// from the home directory, so none of them is a bare file name.
fn parent_of(path: &Path) -> &Path {
    path.parent().unwrap_or(Path::new("."))
}

// The real user id: the `gui/<uid>` domain the unit belongs to. Declared by
// hand, like `kill` in `worker.rs`, because this workspace has no third-party
// dependencies to borrow the declaration from.
unsafe extern "C" {
    fn getuid() -> u32;
}

fn uid() -> u32 {
    // SAFETY: `getuid(2)` takes no arguments, dereferences no pointer and cannot
    // fail; it returns the calling process's real user id.
    unsafe { getuid() }
}

/// `whirl daemon install`: write the unit 5.2 specifies and hand it to launchd.
///
/// An install over an install is the ordinary case rather than an error: the
/// same bytes go to the same path, the old job goes out and the same job comes
/// back.
fn install() -> ExitCode {
    let launchctl = Launchctl::on_path();
    let unit = match unit_file() {
        Ok(unit) => unit,
        Err(message) => return refuse(message),
    };
    let program = match daemon_binary() {
        Ok(program) => program,
        Err(message) => return refuse(message),
    };
    let log = match paths::log_file() {
        Some(log) => log,
        None => return refuse(no_home()),
    };
    match install_unit(&launchctl, &unit, &program, &log) {
        Ok(xml) => {
            println!("unit: {}", unit.display());
            println!("program: {}", program.display());
            println!("log: {}", log.display());
            println!("job: {}", launchctl.service());
            println!();
            print!("{xml}");
            ExitCode::from(EXIT_OK)
        }
        Err(message) => refuse(message),
    }
}

/// The whole install, on paths the caller names, so that the printing above and
/// the tests below drive the same code.
///
/// The unit is written before the old job goes out, so a failed write leaves a
/// running daemon alone; then the label goes out before the new job comes in,
/// so there is no moment with two of it.
fn install_unit(
    launchctl: &Launchctl,
    unit: &Path,
    program: &Path,
    log: &Path,
) -> Result<String, String> {
    let loaded = owned(launchctl, unit)?;
    // launchd creates the two log files but not the directory holding them (the
    // probes in docs/research/scheduling.md leave 0-byte files), and it reads
    // the unit from a directory that has to be there first.
    create_dir(parent_of(unit), 0o755)?;
    create_dir(parent_of(log), 0o700)?;
    let xml = plist_xml(program, log);
    write_unit(unit, &xml)?;
    if loaded.is_some() {
        launchctl.bootout()?;
    }
    launchctl.bootstrap(unit)?;
    Ok(xml)
}

/// `whirl daemon uninstall`: boot the job out and remove the unit, and touch
/// nothing else at all. Not the config, not the state directory, not the cache,
/// and not the log.
fn uninstall() -> ExitCode {
    let launchctl = Launchctl::on_path();
    let unit = match unit_file() {
        Ok(unit) => unit,
        Err(message) => return refuse(message),
    };
    match remove_unit(&launchctl, &unit) {
        Ok(true) => {
            println!("removed: {}", unit.display());
            println!("stopped: {}", launchctl.service());
            ExitCode::from(EXIT_OK)
        }
        Ok(false) => {
            println!("no unit: {}", unit.display());
            ExitCode::from(EXIT_OK)
        }
        Err(message) => refuse(message),
    }
}

/// Uninstall, on a unit path the caller names: the same code the command runs,
/// with nothing of its own decided twice.
fn remove_unit(launchctl: &Launchctl, unit: &Path) -> Result<bool, String> {
    if owned(launchctl, unit)?.is_some() {
        launchctl.bootout()?;
        // Fail closed: a `bootout` that did not take would leave the daemon
        // supervised by a job whose file is already gone, so the removal is not
        // reported as done unless `launchctl print` can no longer find the
        // label.
        if let Some(job) = launchctl.job()? {
            return Err(format!(
                "{LABEL} is still loaded after bootout (state {}), so the daemon is still \
                 supervised; check `launchctl print {}`",
                job.state,
                launchctl.service()
            ));
        }
    }
    match std::fs::remove_file(unit) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("cannot remove {}: {error}", unit.display())),
    }
}

/// `whirl daemon start`: ask the supervisor to run the unit's job.
fn start() -> ExitCode {
    let launchctl = Launchctl::on_path();
    let unit = match unit_file() {
        Ok(unit) => unit,
        Err(message) => return refuse(message),
    };
    match start_unit(&launchctl, &unit) {
        Ok(line) => {
            println!("{line}");
            ExitCode::from(EXIT_OK)
        }
        Err(message) => refuse(message),
    }
}

/// Start, on a unit path the caller names. A daemon the supervisor already has
/// up is reported as running rather than restarted: `start` is not a restart,
/// and the card that asks for a restart is 5.2's `kickstart -k` on the
/// supervisor's own behalf, not a user-facing verb.
fn start_unit(launchctl: &Launchctl, unit: &Path) -> Result<String, String> {
    // Ownership comes first: a label loaded from a unit that is not this
    // whirl's is refused before anything here looks at this whirl's own file.
    match owned(launchctl, unit)? {
        Some(job) if job.running() => {
            let pid = job.pid.map(|pid| format!(" pid {pid}")).unwrap_or_default();
            Ok(format!("already running: {}{pid}", launchctl.service()))
        }
        // Loaded and not running: the supervisor is holding a job it is not
        // running, so `kickstart -k` starts it. It is only reached in this
        // state, so the `-k` has no live daemon to kill.
        Some(_) => {
            launchctl.kickstart()?;
            Ok(format!("started: {}", launchctl.service()))
        }
        // There is nothing to bootstrap without a unit: the job is the unit's,
        // and bootstrap needs the file (5.2).
        None => {
            if !unit.is_file() {
                return Err(format!(
                    "no unit at {}; run `whirl daemon install` first",
                    unit.display()
                ));
            }
            launchctl.bootstrap(unit)?;
            Ok(format!("started: {}", launchctl.service()))
        }
    }
}

/// `whirl daemon stop`: ask the supervisor to unload the job. That is the
/// persistent stop 5.2 names for this platform, and it is what shut a
/// login-installed daemon down while an uninstall would remove the unit as well.
fn stop() -> ExitCode {
    let launchctl = Launchctl::on_path();
    let unit = match unit_file() {
        Ok(unit) => unit,
        Err(message) => return refuse(message),
    };
    match stop_unit(&launchctl, &unit) {
        Ok(line) => {
            println!("{line}");
            ExitCode::from(EXIT_OK)
        }
        Err(message) => refuse(message),
    }
}

fn stop_unit(launchctl: &Launchctl, unit: &Path) -> Result<String, String> {
    if owned(launchctl, unit)?.is_none() {
        return Ok(format!("not running: {}", launchctl.service()));
    }
    launchctl.bootout()?;
    if launchctl.job()?.is_some() {
        return Err(format!(
            "{LABEL} is still loaded after bootout, so the daemon did not stop; check \
             `launchctl print {}`",
            launchctl.service()
        ));
    }
    Ok(format!("stopped: {}", launchctl.service()))
}

/// `whirl daemon status`: what the supervisor says about the daemon, in one
/// line, and the exit code that answer earns.
fn status() -> ExitCode {
    let launchctl = Launchctl::on_path();
    match status_line(&launchctl) {
        Ok((line, code)) => {
            if code == EXIT_OK {
                println!("{line}");
            } else {
                eprintln!("whirl: {line}");
            }
            ExitCode::from(code)
        }
        // The question could not be asked, which is not an answer about the
        // daemon: like an unreachable socket, it is exit 2.
        Err(message) => {
            eprintln!("whirl: {message}");
            ExitCode::from(EXIT_UNREACHABLE)
        }
    }
}

/// The supervisor's answer, with the code the CLI's convention gives it: 0 the
/// supervisor is running the job, 1 it holds the job stopped, 2 it has no such
/// job (which is what a machine nothing installed looks like, and what an
/// uninstall leaves).
fn status_line(launchctl: &Launchctl) -> Result<(String, u8), String> {
    let domain = launchctl.domain();
    match launchctl.job()? {
        None => Ok((
            format!("no supervised daemon: {LABEL} is not loaded in {domain}"),
            EXIT_UNREACHABLE,
        )),
        Some(job) if job.running() => {
            let pid = job.pid.map(|pid| format!(" pid {pid}")).unwrap_or_default();
            Ok((format!("running: {LABEL}{pid} ({domain})"), EXIT_OK))
        }
        Some(_) => Ok((
            format!("loaded and not running: {LABEL} ({domain})"),
            EXIT_REFUSED,
        )),
    }
}

/// The daemon the unit names: the `whirld` installed beside this `whirl`.
///
/// There is no second place to look, which is why a missing one is a refusal
/// rather than a guess: a unit that named a different whirl than the one that
/// wrote it would be a unit whose daemon is a different build.
fn daemon_binary() -> Result<PathBuf, String> {
    let whirl = std::env::current_exe()
        .map_err(|error| format!("cannot name the directory this whirl lives in: {error}"))?;
    daemon_binary_beside(parent_of(&whirl))
}

/// The same decision, on a directory the caller names, so the refusal is
/// testable without writing a unit anywhere.
fn daemon_binary_beside(directory: &Path) -> Result<PathBuf, String> {
    let binary = directory.join("whirld");
    // A path inside a Cargo build directory is refused first. `target/` is a
    // tree that `cargo clean`, and the deletion of the checkout holding it, both
    // take away; a login item left naming a file in one fails at the next login,
    // and the only place that says so is the daemon's own log.
    if let Some(build) = build_directory(&binary) {
        return Err(format!(
            "{} is inside a Cargo build directory ({}), which `cargo clean` and the deletion of a \
             checkout both take away, and a unit that names it is a login item that fails at the \
             next login. Install the binaries first -- `cargo install --path crates/whirld --root \
             <prefix>` and `cargo install --path crates/whirl-cli --root <prefix>` -- then run \
             `<prefix>/bin/whirl daemon install`",
            binary.display(),
            build.display()
        ));
    }
    if !binary.is_file() {
        return Err(format!(
            "no whirld beside this whirl at {}; install the binaries first: `cargo install --path \
             crates/whirld --root <prefix>` and `cargo install --path crates/whirl-cli --root \
             <prefix>`",
            binary.display()
        ));
    }
    Ok(binary)
}

/// The `target` directory a path sits under, if it sits under one. A build tree
/// is not an install prefix, whatever it is called on the way in.
fn build_directory(binary: &Path) -> Option<&Path> {
    binary
        .ancestors()
        .find(|ancestor| ancestor.file_name() == Some(OsStr::new("target")))
}

/// The unit's bytes, for one binary and one log.
///
/// Both are XML text, so both are escaped: a path with an `&` in it would be a
/// unit launchd refuses to parse rather than one it reads.
fn plist_xml(binary: &Path, log: &Path) -> String {
    let mut xml = String::with_capacity(1024);
    let _ = writeln!(xml, "{PLIST_HEADER}");
    let _ = writeln!(xml, "<plist version=\"1.0\">");
    let _ = writeln!(xml, "<dict>");
    let _ = writeln!(xml, "\t<key>Label</key>");
    let _ = writeln!(xml, "\t<string>{LABEL}</string>");
    let _ = writeln!(xml, "\t<key>ProgramArguments</key>");
    let _ = writeln!(xml, "\t<array>");
    let _ = writeln!(
        xml,
        "\t\t<string>{}</string>",
        escape(&binary.to_string_lossy())
    );
    let _ = writeln!(xml, "\t</array>");
    let _ = writeln!(xml, "\t<key>RunAtLoad</key>");
    let _ = writeln!(xml, "\t<true/>");
    let _ = writeln!(xml, "\t<key>KeepAlive</key>");
    let _ = writeln!(xml, "\t<true/>");
    // `Background` is `launchd.plist(5)`'s classification for a process that
    // does work the user did not directly ask for, which is what a rotation is.
    let _ = writeln!(xml, "\t<key>ProcessType</key>");
    let _ = writeln!(xml, "\t<string>Background</string>");
    let log = escape(&log.to_string_lossy());
    // Both streams go to the one file, and launchd opens it with `O_APPEND`.
    // That descriptor holds the inode, so nothing here may rotate the log by
    // renaming it: the daemon would go on appending to the renamed file. The
    // `log_max_bytes` cap is instead applied by rewriting the file in place
    // (`crates/whirld/src/log.rs`, `docs/decisions/0004`).
    let _ = writeln!(xml, "\t<key>StandardOutPath</key>");
    let _ = writeln!(xml, "\t<string>{log}</string>");
    let _ = writeln!(xml, "\t<key>StandardErrorPath</key>");
    let _ = writeln!(xml, "\t<string>{log}</string>");
    let _ = writeln!(xml, "</dict>");
    let _ = writeln!(xml, "</plist>");
    xml
}

/// The three characters an XML text node may not carry raw. `&` goes first, or
/// escaping `<` would escape the ampersand it just wrote.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// launchd's own complaint, for a message that carries it: a failure a reader
/// cannot diagnose is one they have to reproduce by hand.
fn said(output: &Output) -> String {
    let message = String::from_utf8_lossy(&output.stderr);
    let message = message.trim();
    if message.is_empty() {
        String::new()
    } else {
        format!(": {message}")
    }
}

/// Write the unit, and end with all of it or none of it.
///
/// The part file's name does not end in `.plist`, so nothing reads a
/// half-written unit: launchd reads a plist once, at bootstrap, and a torn one
/// is a parse error from the supervisor rather than an error from here.
fn write_unit(unit: &Path, xml: &str) -> Result<(), String> {
    let part = unit.with_extension("plist.part");
    let written = std::fs::write(&part, xml)
        .map_err(|error| format!("cannot write {}: {error}", part.display()))
        .and_then(|()| restrict(&part, 0o644))
        .and_then(|()| {
            std::fs::rename(&part, unit).map_err(|error| {
                format!(
                    "cannot move {} onto {}: {error}",
                    part.display(),
                    unit.display()
                )
            })
        });
    if written.is_err() {
        // A part file left behind would be a file the next install writes over;
        // removing it leaves the directory as it was.
        let _ = std::fs::remove_file(&part);
    }
    written
}

/// Create a directory if it is not there, and restrict only a directory this
/// process created: an existing directory keeps the mode its owner chose.
fn create_dir(path: &Path, mode: u32) -> Result<(), String> {
    if path.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    restrict(path, mode)
}

fn restrict(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| format!("cannot set mode {mode:o} on {}: {error}", path.display()))
}

/// The job this whirl owns, or a refusal when the label is loaded from a unit
/// somewhere else.
///
/// The label is the product's and the supervisor holds one job under it, so a
/// unit under another home directory is somebody else's daemon running under
/// this label. Refusing is what keeps these steps from stopping or replacing a
/// daemon this whirl did not install, and it is what lets a run under a scratch
/// `HOME` exercise the commands without touching the machine's own job.
fn owned(launchctl: &Launchctl, unit: &Path) -> Result<Option<Job>, String> {
    match launchctl.job()? {
        Some(job) if !job.is_ours(unit) => {
            let loaded = job.unit.as_deref().map_or_else(
                || "(no unit file)".to_string(),
                |path| path.display().to_string(),
            );
            Err(format!(
                "{LABEL} is loaded from {loaded}, not from {}; this whirl does not manage that job, \
                 and will not stop or replace it",
                unit.display()
            ))
        }
        Some(job) => Ok(Some(job)),
        None => Ok(None),
    }
}

/// What `launchctl print` says about one job.
#[derive(Debug, Default)]
struct Job {
    /// The unit launchd loaded it from, when it came from a file. A job the
    /// system submitted carries a description here instead of a path, and that
    /// is not this product's unit.
    unit: Option<PathBuf>,
    /// launchd's own word, `running` or `not running` among them.
    state: String,
    pid: Option<u32>,
}

impl Job {
    /// Read what the job itself reports.
    ///
    /// Only the entries indented one tab are the job's own: `launchctl print`
    /// prints a nested dictionary one tab deeper, and a nested `state = active`
    /// inside `event triggers` is not the job's state. Reading a nested value
    /// here is how a running job was reported as "loaded and not running".
    fn parse(text: &str) -> Job {
        let mut job = Job::default();
        for line in text.lines() {
            let Some(field) = line.strip_prefix('\t') else {
                continue;
            };
            if field.starts_with('\t') {
                continue;
            }
            let Some((key, value)) = field.split_once(" = ") else {
                continue;
            };
            let value = value.trim();
            // First one wins, so a later nested block can never overwrite the
            // job's own answer even if the indentation is not what this expects.
            match key.trim() {
                "path" if job.unit.is_none() => job.unit = Some(PathBuf::from(value)),
                "state" if job.state.is_empty() => job.state = value.to_string(),
                "pid" if job.pid.is_none() => job.pid = value.parse().ok(),
                _ => {}
            }
        }
        job
    }

    /// Whether the supervisor's own word is that the daemon is up.
    fn running(&self) -> bool {
        self.state == "running"
    }

    /// Whether this job is the one a unit at `unit` is: the file the supervisor
    /// loaded it from is the file this whirl would write.
    fn is_ours(&self, unit: &Path) -> bool {
        self.unit.as_deref().is_some_and(|loaded| loaded == unit)
    }
}

/// The supervisor, and the one binary every step goes through.
struct Launchctl {
    program: PathBuf,
}

impl Launchctl {
    fn on_path() -> Launchctl {
        Launchctl {
            program: PathBuf::from(LAUNCHCTL),
        }
    }

    #[cfg(test)]
    fn at(program: &Path) -> Launchctl {
        Launchctl {
            program: program.to_path_buf(),
        }
    }

    /// `gui/<uid>`: the per-user Aqua session the unit belongs to (5.2).
    fn domain(&self) -> String {
        format!("gui/{}", uid())
    }

    /// `gui/<uid>/<label>`: the service target `print` and `bootout` take.
    fn service(&self) -> String {
        format!("{}/{LABEL}", self.domain())
    }

    /// Run one `launchctl` subcommand and capture what it said.
    ///
    /// Captured rather than inherited, because launchd talks on stderr about
    /// things that are not failures here: the `bootout` of a label that was
    /// never loaded is what a first install and a second stop both do, and it
    /// answers with "Boot-out failed: 3: No such process". A real refusal still
    /// reaches the reader, through the message the caller builds from this.
    fn run(&self, args: &[&OsStr]) -> Result<Output, String> {
        Command::new(&self.program)
            .args(args)
            .output()
            .map_err(|error| format!("cannot run {}: {error}", self.program.display()))
    }

    /// What the supervisor says about the job, or `None` when it has no such
    /// job.
    ///
    /// "No such job" is the supervisor's own words, and it is a different
    /// answer from "the question could not be asked": the first is what a
    /// machine nothing installed looks like, and the second is reported as a
    /// failure to reach the supervisor.
    fn job(&self) -> Result<Option<Job>, String> {
        let service = self.service();
        let output = self.run(&[OsStr::new("print"), OsStr::new(&service)])?;
        if output.status.success() {
            return Ok(Some(Job::parse(&String::from_utf8_lossy(&output.stdout))));
        }
        if String::from_utf8_lossy(&output.stderr).contains("Could not find") {
            return Ok(None);
        }
        Err(format!(
            "launchctl print {service} failed: {}{}",
            output.status,
            said(&output)
        ))
    }

    /// Register the unit and let `RunAtLoad` start it. `bootstrap` names the
    /// domain and then the file, in that order.
    fn bootstrap(&self, unit: &Path) -> Result<(), String> {
        let domain = self.domain();
        let output = self.run(&[
            OsStr::new("bootstrap"),
            OsStr::new(&domain),
            unit.as_os_str(),
        ])?;
        if output.status.success() {
            return Ok(());
        }
        Err(format!(
            "launchctl bootstrap {domain} {} failed: {}{}",
            unit.display(),
            output.status,
            said(&output)
        ))
    }

    /// Start a job the supervisor is holding stopped.
    fn kickstart(&self) -> Result<(), String> {
        let service = self.service();
        let output = self.run(&[
            OsStr::new("kickstart"),
            OsStr::new("-k"),
            OsStr::new(&service),
        ])?;
        if output.status.success() {
            return Ok(());
        }
        Err(format!(
            "launchctl kickstart -k {service} failed: {}{}",
            output.status,
            said(&output)
        ))
    }

    /// Unregister the job. The caller decides what a refusal means: on an
    /// install it is "nothing was loaded", and on a stop the check that follows
    /// is what makes it a failure.
    fn bootout(&self) -> Result<(), String> {
        let service = self.service();
        let output = self.run(&[OsStr::new("bootout"), OsStr::new(&service)])?;
        if output.status.success() {
            return Ok(());
        }
        Err(format!(
            "launchctl bootout {service} failed: {}{}",
            output.status,
            said(&output)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// What the fake supervisor reports before anything runs.
    enum Loaded {
        /// No such job: the state a machine nothing installed is in.
        Nothing,
        /// A job loaded from this unit, in this state.
        Ours(&'static str),
        /// A job loaded from a unit file somewhere else.
        From(String, &'static str),
    }

    /// A fake `launchctl` for one unit path, and the state it answers from.
    ///
    /// `print` reads a marker file whose first two lines are the unit it was
    /// loaded from and its state; `bootstrap` writes that marker and `bootout`
    /// removes it. So a check that follows a step sees what the step did,
    /// exactly as launchd would answer. `stubborn` is the other supervisor: the
    /// one whose `bootout` does not take.
    struct Fake {
        unit: PathBuf,
        log: PathBuf,
        program: PathBuf,
        calls: PathBuf,
        marker: PathBuf,
        launchctl: PathBuf,
    }

    impl Fake {
        fn new(name: &str, loaded: Loaded, stubborn: bool) -> Fake {
            let dir =
                std::env::temp_dir().join(format!("whirl-daemon-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a temporary directory");
            let fake = Fake {
                unit: dir.join("LaunchAgents").join(format!("{LABEL}.plist")),
                log: dir.join("Logs/whirl/whirl.log"),
                program: dir.join("bin/whirld"),
                calls: dir.join("calls"),
                marker: dir.join("loaded"),
                launchctl: dir.join("launchctl"),
            };
            match loaded {
                Loaded::Nothing => {}
                Loaded::Ours(state) => fake.mark_loaded(&fake.unit, state),
                Loaded::From(unit, state) => fake.mark_loaded(Path::new(&unit), state),
            }
            let unload = if stubborn {
                ":".to_string()
            } else {
                format!("rm -f '{}'", fake.marker.display())
            };
            let script = format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{calls}'
case "$1" in
  print)
    if [ -f '{marker}' ]; then
      printf '%s\n' '{domain}/{label} = {{'
      printf '\tactive count = 1\n'
      printf '\tpath = %s\n' "$(sed -n 1p '{marker}')"
      printf '\ttype = LaunchAgent\n'
      printf '\tstate = %s\n' "$(sed -n 2p '{marker}')"
      printf '\n\tprogram = {program}\n'
      printf '\tstdout path = {log}\n'
      printf '\tpid = 4242\n'
      printf '\n\tevent triggers = {{\n'
      printf '\t\t"{label}" => {{\n'
      printf '\t\t\tstate = active\n'
      printf '\t\t\tpid = 9999\n'
      printf '\t\t}}\n'
      printf '\t}}\n'
      printf '}}\n'
      exit 0
    fi
    printf '%s\n' 'Could not find service "{label}" in domain for user gui: {uid}' >&2
    exit 113 ;;
  bootstrap)
    printf '%s\n' '{unit}' 'running' > '{marker}'
    exit 0 ;;
  bootout)
    {unload}
    exit 0 ;;
  *) exit 0 ;;
esac
"#,
                calls = fake.calls.display(),
                marker = fake.marker.display(),
                domain = fake.domain(),
                label = LABEL,
                uid = uid(),
                unit = fake.unit.display(),
                program = fake.program.display(),
                log = fake.log.display(),
                unload = unload,
            );
            std::fs::write(&fake.launchctl, script).expect("the fake launchctl");
            std::fs::set_permissions(&fake.launchctl, std::fs::Permissions::from_mode(0o755))
                .expect("the fake launchctl is executable");
            fake
        }

        /// The same fake, with the job loaded from a unit that is not this
        /// whirl's.
        fn foreign(name: &str, unit: &Path, state: &'static str) -> Fake {
            Fake::new(name, Loaded::From(unit.display().to_string(), state), false)
        }

        fn mark_loaded(&self, unit: &Path, state: &str) {
            std::fs::write(&self.marker, format!("{}\n{state}\n", unit.display()))
                .expect("the marker");
        }

        /// Put a unit on disk without asking the supervisor anything: the state
        /// a previous install left behind, which is what `start` and `stop`
        /// find on a machine that is already set up.
        fn plant_unit(&self) {
            create_dir(parent_of(&self.unit), 0o755).expect("the unit directory");
            write_unit(&self.unit, &plist_xml(&self.program, &self.log)).expect("the unit");
        }

        fn launchctl(&self) -> Launchctl {
            Launchctl::at(&self.launchctl)
        }

        /// The domain and the service target, from this machine's own user id:
        /// the code asks `getuid(2)`, so the fake answers for the same user.
        fn domain(&self) -> String {
            format!("gui/{}", uid())
        }

        fn service(&self) -> String {
            format!("{}/{LABEL}", self.domain())
        }

        fn print_call(&self) -> String {
            format!("print {}", self.service())
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(&self.calls)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }

        fn unit_text(&self) -> String {
            std::fs::read_to_string(&self.unit).expect("the unit the step wrote")
        }
    }

    /// The unit file is the label under the home directory (5.2), so the file a
    /// reader is sent to and the job `launchctl` knows cannot drift apart.
    #[test]
    fn the_unit_is_the_label_under_the_home_directory() {
        let unit = unit_file().expect("a home directory");
        assert!(
            unit.ends_with(format!("Library/LaunchAgents/{LABEL}.plist")),
            "{}",
            unit.display()
        );
        assert!(unit.starts_with(paths::home().expect("a home directory")));
    }

    /// The keys are 5.2's, the program and the log are named, and no schedule
    /// of any kind is in the unit: the rotation clock is the daemon's own.
    #[test]
    fn the_plist_carries_the_decided_keys_and_no_schedule() {
        let xml = plist_xml(
            Path::new("/opt/whirl/bin/whirld"),
            Path::new("/opt/whirl/logs/whirl.log"),
        );
        assert!(
            xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"),
            "{xml}"
        );
        for key in [
            "<key>Label</key>",
            "<key>ProgramArguments</key>",
            "<key>RunAtLoad</key>",
            "<key>KeepAlive</key>",
            "<key>StandardOutPath</key>",
            "<key>StandardErrorPath</key>",
        ] {
            assert!(xml.contains(key), "{key} is missing from {xml}");
        }
        assert!(
            xml.contains("<string>/opt/whirl/bin/whirld</string>"),
            "{xml}"
        );
        assert_eq!(
            xml.matches("<string>/opt/whirl/logs/whirl.log</string>")
                .count(),
            2,
            "both streams land in the daemon's one log: {xml}"
        );
        for forbidden in [
            "StartInterval",
            "StartCalendarInterval",
            "EnvironmentVariables",
            "WatchPaths",
        ] {
            assert!(!xml.contains(forbidden), "{forbidden} is in {xml}");
        }
    }

    /// A path with an `&` in it is XML text, not markup: a unit launchd refuses
    /// to parse is a unit that installs nothing.
    #[test]
    fn a_path_is_escaped_before_it_becomes_xml_text() {
        let xml = plist_xml(
            Path::new("/opt/a&b/whirld"),
            Path::new("/opt/a&b/whirl.log"),
        );
        assert!(
            xml.contains("<string>/opt/a&amp;b/whirld</string>"),
            "{xml}"
        );
        assert!(!xml.contains("a&b"), "{xml}");
    }

    /// A build tree is not an install prefix: the unit may not name a file
    /// `cargo clean` takes away.
    #[test]
    fn a_binary_inside_a_build_directory_is_refused() {
        let here = std::env::current_exe().expect("this test binary's path");
        let error = daemon_binary_beside(parent_of(&here)).expect_err("the refusal");
        assert!(error.contains("Cargo build directory"), "{error}");
        assert!(error.contains("target"), "{error}");
        assert!(error.contains("cargo install"), "{error}");
    }

    /// With nothing installed beside it, whirl says so and names both installs
    /// rather than writing a unit that would not start anything.
    #[test]
    fn a_missing_whirld_beside_whirl_is_refused() {
        let dir = std::env::temp_dir().join(format!("whirl-daemon-{}-missing", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        let error = daemon_binary_beside(&dir).expect_err("the refusal");
        assert!(error.contains("no whirld beside this whirl"), "{error}");
        assert!(error.contains("cargo install"), "{error}");
    }

    /// An install writes the unit and hands it to the supervisor, and the file
    /// is on disk when the supervisor is asked, so the job it loads is the unit
    /// this whirl wrote.
    #[test]
    fn install_writes_the_unit_and_bootstraps_it() {
        let fake = Fake::new("install", Loaded::Nothing, false);
        let xml = install_unit(&fake.launchctl(), &fake.unit, &fake.program, &fake.log)
            .expect("the install");
        assert_eq!(fake.unit_text(), xml);
        assert_eq!(
            fake.calls(),
            vec![
                fake.print_call(),
                format!("bootstrap {} {}", fake.domain(), fake.unit.display()),
            ]
        );
        // The log directory launchd writes into exists and is the daemon's own.
        assert!(parent_of(&fake.log).is_dir());
        // The part file is gone: one unit, and nothing else in the directory.
        assert!(!fake.unit.with_extension("plist.part").exists());
    }

    /// An install over an install is the ordinary case: the same bytes, one
    /// job, and no second daemon to fight.
    #[test]
    fn a_second_install_changes_nothing_but_the_job() {
        let fake = Fake::new("reinstall", Loaded::Ours("running"), false);
        let once = vec![
            fake.print_call(),
            format!("bootout {}", fake.service()),
            format!("bootstrap {} {}", fake.domain(), fake.unit.display()),
        ];
        let first = install_unit(&fake.launchctl(), &fake.unit, &fake.program, &fake.log)
            .expect("the first install");
        assert_eq!(fake.calls(), once, "one job out, one job in");
        let second = install_unit(&fake.launchctl(), &fake.unit, &fake.program, &fake.log)
            .expect("the second install");
        assert_eq!(first, second, "the unit's bytes are the same");
        assert_eq!(fake.unit_text(), first);
        assert_eq!(
            fake.calls(),
            [once.clone(), once].concat(),
            "the second install did the same as the first, and never two jobs at once"
        );
    }

    /// Uninstall boots the job out and removes the unit, and touches nothing
    /// else: a neighbour in the same directory and the log file both survive.
    #[test]
    fn uninstall_removes_the_unit_and_boots_the_job_out() {
        let fake = Fake::new("uninstall", Loaded::Ours("running"), false);
        install_unit(&fake.launchctl(), &fake.unit, &fake.program, &fake.log).expect("the install");
        let neighbour = parent_of(&fake.unit).join("com.example.other.plist");
        std::fs::write(&neighbour, "not whirl's").expect("a neighbour");
        std::fs::write(&fake.log, "the daemon's log").expect("the log");

        let removed = remove_unit(&fake.launchctl(), &fake.unit).expect("the removal");
        assert!(removed, "the unit was there to remove");
        assert!(!fake.unit.exists(), "the unit is gone");
        assert!(neighbour.exists(), "the neighbour is untouched");
        assert!(fake.log.exists(), "the log is untouched");
        // Ask, job out, load; then ask, job out, and ask once more to confirm
        // the job is really gone before the removal is reported.
        let print = fake.print_call();
        let bootout = format!("bootout {}", fake.service());
        let bootstrap = format!("bootstrap {} {}", fake.domain(), fake.unit.display());
        assert_eq!(
            fake.calls(),
            vec![
                print.clone(),
                bootout.clone(),
                bootstrap,
                print.clone(),
                bootout,
                print,
            ]
        );
    }

    /// Uninstall is idempotent: with no unit it says so and succeeds, which is
    /// what a second run does.
    #[test]
    fn uninstall_without_a_unit_removes_nothing_and_succeeds() {
        let fake = Fake::new("uninstall-twice", Loaded::Nothing, false);
        assert!(!remove_unit(&fake.launchctl(), &fake.unit).expect("the removal"));
        assert_eq!(fake.calls(), vec![fake.print_call()]);
    }

    /// A `bootout` that did not take is not reported as a removal, and the unit
    /// file is left in place: the supervisor still has the job, so a retry must
    /// still find the unit that describes it.
    #[test]
    fn uninstall_refuses_while_the_supervisor_still_has_the_job() {
        let fake = Fake::new("stubborn", Loaded::Ours("running"), true);
        install_unit(&fake.launchctl(), &fake.unit, &fake.program, &fake.log).expect("the install");
        let error = remove_unit(&fake.launchctl(), &fake.unit).expect_err("the refusal");
        assert!(error.contains("still loaded after bootout"), "{error}");
        assert!(
            fake.unit.exists(),
            "the unit stays, so a retry still finds it"
        );
    }

    /// `start` on a daemon the supervisor already has up says so, and does not
    /// restart it: `start` is not a restart.
    #[test]
    fn start_says_a_running_daemon_is_already_running() {
        let fake = Fake::new("start-running", Loaded::Ours("running"), false);
        fake.plant_unit();
        let line = start_unit(&fake.launchctl(), &fake.unit).expect("the report");
        assert_eq!(
            line,
            format!("already running: {} pid 4242", fake.service())
        );
        assert_eq!(
            fake.calls(),
            vec![fake.print_call()],
            "nothing was started or restarted"
        );
    }

    /// A unit the supervisor is holding stopped is started, and a unit it does
    /// not have at all is bootstrapped.
    #[test]
    fn start_reaches_the_supervisor_for_a_job_that_is_not_running() {
        let fake = Fake::new("start-stopped", Loaded::Ours("not running"), false);
        fake.plant_unit();
        let line = start_unit(&fake.launchctl(), &fake.unit).expect("the report");
        assert_eq!(line, format!("started: {}", fake.service()));
        assert_eq!(
            fake.calls(),
            vec![
                fake.print_call(),
                format!("kickstart -k {}", fake.service()),
            ]
        );
    }

    #[test]
    fn start_bootstraps_a_unit_the_supervisor_does_not_have() {
        let fake = Fake::new("start-nothing", Loaded::Nothing, false);
        fake.plant_unit();
        let line = start_unit(&fake.launchctl(), &fake.unit).expect("the report");
        assert_eq!(line, format!("started: {}", fake.service()));
        assert_eq!(
            fake.calls(),
            vec![
                fake.print_call(),
                format!("bootstrap {} {}", fake.domain(), fake.unit.display()),
            ]
        );
    }

    /// Nothing to start without a unit, and the reader is told what to run.
    #[test]
    fn start_without_a_unit_asks_for_an_install() {
        let fake = Fake::new("start-no-unit", Loaded::Nothing, false);
        let error = start_unit(&fake.launchctl(), &fake.unit).expect_err("the refusal");
        assert!(error.contains("whirl daemon install"), "{error}");
        assert_eq!(
            fake.calls(),
            vec![fake.print_call()],
            "the supervisor was asked, and nothing was bootstrapped"
        );
    }

    /// Stop boots the job out and confirms it is gone; a second stop is the
    /// same as the first.
    #[test]
    fn stop_boots_the_job_out_and_a_second_stop_is_the_same() {
        let fake = Fake::new("stop", Loaded::Ours("running"), false);
        let line = stop_unit(&fake.launchctl(), &fake.unit).expect("the stop");
        assert_eq!(line, format!("stopped: {}", fake.service()));
        let line = stop_unit(&fake.launchctl(), &fake.unit).expect("the second stop");
        assert_eq!(line, format!("not running: {}", fake.service()));
    }

    /// A job loaded from another home's unit file is not this whirl's, so no
    /// step of this whirl's stops it: that is the guard a run under a scratch
    /// `HOME` leans on.
    #[test]
    fn a_job_loaded_from_another_unit_is_left_alone() {
        let other = std::env::temp_dir().join("someone-elses/com.guruor.whirl.plist");
        let fake = Fake::foreign("foreign", &other, "running");
        let attempts = [
            stop_unit(&fake.launchctl(), &fake.unit).map(|_| ()),
            remove_unit(&fake.launchctl(), &fake.unit).map(|_| ()),
            start_unit(&fake.launchctl(), &fake.unit).map(|_| ()),
        ];
        for refusal in attempts {
            let error = refusal.expect_err("the refusal");
            assert!(error.contains("does not manage that job"), "{error}");
            assert!(error.contains(&other.display().to_string()), "{error}");
        }
        assert!(
            !fake
                .calls()
                .iter()
                .any(|call| call.starts_with("bootout") || call.starts_with("bootstrap")),
            "{:?}",
            fake.calls()
        );
        assert!(!fake.unit.exists(), "nothing of this whirl's was written");
    }

    /// The shape `launchctl print` actually printed on the machine, which no
    /// test had: the job's own `state` and `pid` one tab deep, and a nested
    /// `event triggers` block carrying its own `state = active` and `pid`. A
    /// parse that reads the nested values reports a running job as "loaded and
    /// not running", and that is what this test is here to keep out.
    #[test]
    fn a_real_launchctl_print_is_read_at_the_jobs_own_level() {
        let printed = "\
gui/501/com.guruor.whirl = {
\tactive count = 1
\tpath = /Users/example/Library/LaunchAgents/com.guruor.whirl.plist
\ttype = LaunchAgent
\tstate = running

\tprogram = /Users/example/bin/whirld
\tstdout path = /Users/example/Library/Logs/whirl/whirl.log
\tpid = 3272

\tevent triggers = {
\t\t\"com.guruor.whirl\" => {
\t\t\tstate = active
\t\t\tpid = 9999
\t\t}
\t}
}
";
        let job = Job::parse(printed);
        assert!(job.running(), "state was {}", job.state);
        assert_eq!(job.pid, Some(3272), "the job's pid, not a nested one");
        assert_eq!(
            job.unit.as_deref(),
            Some(Path::new(
                "/Users/example/Library/LaunchAgents/com.guruor.whirl.plist"
            ))
        );
    }

    /// Status is the supervisor's answer, in one line, with the code that
    /// answer earns: running 0, loaded and stopped 1, no such job 2.
    #[test]
    fn status_reports_what_the_supervisor_says() {
        let running = Fake::new("status-running", Loaded::Ours("running"), false);
        let (line, code) = status_line(&running.launchctl()).expect("the answer");
        assert_eq!(code, EXIT_OK);
        assert_eq!(
            line,
            format!("running: {LABEL} pid 4242 ({})", running.domain())
        );

        let stopped = Fake::new("status-stopped", Loaded::Ours("not running"), false);
        let (line, code) = status_line(&stopped.launchctl()).expect("the answer");
        assert_eq!(code, EXIT_REFUSED);
        assert_eq!(
            line,
            format!("loaded and not running: {LABEL} ({})", stopped.domain())
        );

        let nothing = Fake::new("status-nothing", Loaded::Nothing, false);
        let (line, code) = status_line(&nothing.launchctl()).expect("the answer");
        assert_eq!(code, EXIT_UNREACHABLE);
        assert_eq!(
            line,
            format!(
                "no supervised daemon: {LABEL} is not loaded in {}",
                nothing.domain()
            )
        );
    }
}
