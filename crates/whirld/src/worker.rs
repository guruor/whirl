//! The worker contract: argv, the scrubbed environment, the deadline and the
//! stdout parse (docs/architecture.md 1.6 and 1.7).
//!
//! The daemon spawns `whirl-worker` exactly as 1.6 specifies and reads the last
//! non-empty stdout line as the result. The environment is `env_clear()` plus
//! the names 1.6 lists, which is the fix for `[M 16]` (the prototype inherited
//! the daemon's whole environment) and what lets the Linux adapters see the
//! session signals they need.

use std::ffi::OsString;
use std::io::{ErrorKind, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use whirl_core::config::Backend;
use whirl_core::protocol::{self, ErrorCode, SetRecord, Via};

/// The variables the worker always gets (docs/architecture.md 1.6).
const ALWAYS: [&str; 2] = ["PATH", "HOME"];

/// The two path knobs of docs/architecture.md 4.3's environment layer that the
/// worker resolves for itself, passed through with the daemon's own values when
/// the daemon's environment has them (1.6 says so; the list is 1.6's).
///
/// They are here because 4.3 puts the environment ahead of the config file for
/// the cache root and the state directory, and the worker resolves both on its
/// own (`whirl_worker::pipeline::Cache::resolve` and `state_directory()`). A
/// scrub that dropped them left the daemon reporting the directory the
/// environment named while the worker fell through to the compiled default and
/// wrote somewhere else: two processes, two directories, one rotation. Each name
/// is passed only when set, which is the rule the API key below already follows.
const PASSED_THROUGH: [&str; 2] = ["WHIRL_CACHE_DIR", "WHIRL_STATE_DIR"];

/// The nine Linux-only variables: the four decisive session signals (five
/// names, because sway and i3 share a row) plus the four a session bus or a
/// display connection needs (docs/architecture.md 1.6, `[D 3]`).
const LINUX_ONLY: [&str; 9] = [
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_TYPE",
    "SWAYSOCK",
    "I3SOCK",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "WAYLAND_DISPLAY",
    "DISPLAY",
];

/// The names the scrub lets through, in one place so the spawn, its test and
/// 1.6's list cannot drift: `ALWAYS`, then 4.3's two path knobs, each one only
/// when the lookup finds it. The daemon's own environment is the lookup in
/// production; a test supplies its own table and asks the same question.
fn forwarded(lookup: &dyn Fn(&str) -> Option<OsString>) -> Vec<(String, OsString)> {
    ALWAYS
        .iter()
        .chain(PASSED_THROUGH.iter())
        .filter_map(|name| lookup(name).map(|value| (name.to_string(), value)))
        .collect()
}

/// The worker's verb (1.6). `prev` is not one: `prev`'s job is a `set` of a
/// candidate the daemon already knows, so it spawns `--verb set --target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Rotate,
    Set,
    Check,
}

impl Verb {
    fn as_str(self) -> &'static str {
        match self {
            Verb::Rotate => "rotate",
            Verb::Set => "set",
            Verb::Check => "check",
        }
    }
}

/// What a finished worker gave back.
#[derive(Debug)]
pub enum Outcome {
    /// A `set:` line, already turned into the four-field record of 2.6.
    Set(SetRecord),
    /// The check lines, forwarded verbatim.
    Lines(Vec<String>),
}

#[derive(Debug)]
pub enum WorkerError {
    /// The deadline expired: `ERR timeout`, and `last_error: timeout` (1.7.1).
    Timeout,
    /// The worker exited non-zero, crashed or was killed: `worker_failed`, or
    /// the code the worker named on stderr (2.7).
    Failed { code: ErrorCode, message: String },
}

/// A sleep between two `try_wait` calls. The worker is a child process measured
/// at 1.2-3.4 s `[M 4]`; this is a 5 ms guard around a syscall, not a scheduler,
/// and nothing else in the daemon waits on it.
const POLL: Duration = Duration::from_millis(5);

/// How long the worker gets after `SIGTERM` before `SIGKILL` (1.7.1).
const TERM_GRACE: Duration = Duration::from_secs(5);

/// How many times the spawn is attempted when the kernel refuses it with
/// `ETXTBSY`: the first, plus nine retries.
///
/// `ETXTBSY` means the kernel refused **before anything ran**. Linux refuses to
/// `execve` a file that any process holds open for writing (`deny_write_access`
/// on the exec target), so no worker existed, no `rotate.lock` was taken and
/// nothing was written: re-attempting is free. The alternative is a
/// `worker_failed` report (2.7) about a process that never started.
///
/// The condition is transient **by construction**, not by luck: *any* open-for-
/// write descriptor on the file being exec'd causes it, and the holder is always
/// someone who is about to be done with it. Under `cargo test` it is a forked
/// child of a sibling thread that inherited the descriptor `Scripts::script`
/// wrote with (`fork` duplicates the descriptor into the child, which can
/// outlive the write in the parent). In an install it is the process replacing
/// the binary, which holds it open for write only while it copies.
const SPAWN_ATTEMPTS: usize = 10;

/// The sleep between two spawn attempts: nine of them is 450 ms of waiting in
/// the worst case, and it is only ever reached on a real `ETXTBSY`. Every other
/// errno (a missing program, a directory, a permission denial) returns on the
/// first attempt with today's message and today's immediacy.
const SPAWN_RETRY_PAUSE: Duration = Duration::from_millis(50);

pub struct Worker {
    program: PathBuf,
    config_path: PathBuf,
    backend: Backend,
}

impl Worker {
    pub fn new(program: PathBuf, config_path: PathBuf, backend: Backend) -> Worker {
        Worker {
            program,
            config_path,
            backend,
        }
    }

    /// The worker sits next to the daemon: `cargo run -p whirld` and an
    /// installed layout both put the two binaries in one directory.
    pub fn default_program() -> PathBuf {
        let name = if cfg!(windows) {
            "whirl-worker.exe"
        } else {
            "whirl-worker"
        };
        match std::env::current_exe() {
            Ok(exe) => exe.with_file_name(name),
            Err(_) => PathBuf::from(name),
        }
    }

    /// Spawn one worker and wait for it, with the deadline of 1.7.1.
    pub fn run(
        &self,
        verb: Verb,
        target: Option<&str>,
        run: u64,
        deadline: Duration,
    ) -> Result<Outcome, WorkerError> {
        let mut command = Command::new(&self.program);
        command
            .arg("--config")
            .arg(&self.config_path)
            .arg("--verb")
            .arg(verb.as_str());
        if let Some(target) = target {
            command.arg("--target").arg(target);
        }
        command.arg("--run").arg(run.to_string());
        command.env_clear();
        for (name, value) in forwarded(&|name| std::env::var_os(name)) {
            command.env(name, value);
        }
        // The daemon resolved the backend, so the worker is told rather than
        // asked to re-resolve it (docs/development.md section 7).
        command.env("WHIRL_CONFIG", &self.config_path);
        command.env("WHIRL_BACKEND", self.backend.as_str());
        // Only when the daemon's own environment sets it, and never written to
        // a file (docs/architecture.md 6.3). The value is never printed.
        if let Some(value) = std::env::var_os("WHIRL_WALLHAVEN_API_KEY") {
            command.env("WHIRL_WALLHAVEN_API_KEY", value);
        }
        if cfg!(target_os = "linux") {
            for name in LINUX_ONLY {
                if let Some(value) = std::env::var_os(name) {
                    command.env(name, value);
                }
            }
        }

        // `ETXTBSY` is the kernel refusing before anything ran, so a retry costs
        // nothing and a report would be about a worker that never existed. Every
        // other refusal is today's first-attempt failure.
        let mut attempt = 1;
        let mut child = loop {
            match command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(child) => break child,
                Err(error) if error.kind() == ErrorKind::ExecutableFileBusy => {
                    if attempt >= SPAWN_ATTEMPTS {
                        return Err(WorkerError::Failed {
                            code: ErrorCode::WorkerFailed,
                            message: format!("cannot spawn {}: {error}", self.program.display()),
                        });
                    }
                    attempt += 1;
                    std::thread::sleep(SPAWN_RETRY_PAUSE);
                }
                Err(error) => {
                    return Err(WorkerError::Failed {
                        code: ErrorCode::WorkerFailed,
                        message: format!("cannot spawn {}: {error}", self.program.display()),
                    });
                }
            }
        };

        let deadline_at = Instant::now() + deadline;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => {
                    return Err(WorkerError::Failed {
                        code: ErrorCode::WorkerFailed,
                        message: format!("cannot wait for the worker: {error}"),
                    });
                }
            }
            if Instant::now() >= deadline_at {
                terminate(&mut child);
                return Err(WorkerError::Timeout);
            }
            std::thread::sleep(POLL);
        };

        // Read after the exit. The contract caps stdout at two lines and stderr
        // at one, so the pipe buffer cannot be full; a worker that ignored the
        // contract and filled it was killed by the deadline above, which is what
        // makes this read safe.
        let mut stdout = String::new();
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stdout.take() {
            let _ = pipe.read_to_string(&mut stdout);
        }
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }

        if !status.success() {
            return Err(failure_from(&stderr));
        }
        match verb {
            Verb::Check => Ok(Outcome::Lines(
                stdout
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(str::to_string)
                    .collect(),
            )),
            Verb::Rotate | Verb::Set => {
                let last = stdout
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("")
                    .trim_end_matches('\r');
                let (digest, origin_key, path) =
                    protocol::parse_worker_set_line(last).ok_or_else(|| WorkerError::Failed {
                        code: ErrorCode::WorkerFailed,
                        message: format!(
                            "the worker exited 0 without a set: line; last line was {last:?}"
                        ),
                    })?;
                Ok(Outcome::Set(SetRecord {
                    digest,
                    origin_key,
                    via: Via::Source,
                    path: Some(path),
                }))
            }
        }
    }
}

/// The failure the worker reported. 1.6 fixes the stdout shape and says the
/// failing stage goes on stderr; the line it writes here (`stage=<name>
/// code=<code> message=<text>`) is this scaffold's spelling of that, and the
/// daemon takes the code from it or reports `worker_failed` (2.7).
fn failure_from(stderr: &str) -> WorkerError {
    for line in stderr.lines().rev() {
        let named = line
            .split(' ')
            .find_map(|field| field.strip_prefix("code="))
            .and_then(ErrorCode::parse);
        if let Some(code) = named {
            return WorkerError::Failed {
                code,
                message: line.trim().to_string(),
            };
        }
    }
    let message = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("the worker exited non-zero with no message")
        .trim()
        .to_string();
    WorkerError::Failed {
        code: ErrorCode::WorkerFailed,
        message,
    }
}

/// `SIGTERM`, five seconds, then `SIGKILL` (1.7.1).
///
/// `Child::kill` is `SIGKILL` on Unix and the standard library has no signal
/// API, so the polite half goes through `kill(1)`, which exists on every Unix
/// this build targets. A missing `kill` falls straight through to `SIGKILL`.
fn terminate(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(child.id().to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let grace = Instant::now() + TERM_GRACE;
    while Instant::now() < grace {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(POLL),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::thread::JoinHandle;

    /// A temp directory with scripts in it. The tests below are the only place
    /// where a "worker" is anything other than `whirl-worker`: a deliberately
    /// slow or misbehaving program is the only way to reach the deadline and the
    /// `SIGTERM`/`SIGKILL` escalation without waiting five minutes for the real
    /// one, and `Worker::new` takes the program path, so a script is a worker as
    /// far as this module is concerned.
    struct Scripts {
        dir: PathBuf,
    }

    impl Scripts {
        fn new(name: &str) -> Scripts {
            let dir = std::env::temp_dir()
                .join(format!("whirl-worker-test-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("a temp directory");
            Scripts { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }

        /// Write an executable `/bin/sh` script: the same interpreter every Unix
        /// this build targets has, and `env_clear()` does not touch argv.
        fn script(&self, name: &str, body: &str) -> PathBuf {
            let path = self.path(name);
            fs::write(&path, body).expect("a script");
            let mut permissions = fs::metadata(&path).expect("the script").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).expect("an executable script");
            path
        }

        fn text(&self, name: &str) -> String {
            fs::read_to_string(self.path(name)).unwrap_or_else(|error| {
                panic!("{} was not written: {error}", self.path(name).display())
            })
        }

        fn exists(&self, name: &str) -> bool {
            self.path(name).exists()
        }
    }

    impl Drop for Scripts {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// The config path is never read: every script here ignores its argv, which
    /// is itself part of what is being shown (1.6's argv is fixed and the
    /// program is told, not asked).
    fn worker(program: PathBuf) -> Worker {
        Worker::new(
            program,
            PathBuf::from("/nonexistent/whirl/config.json"),
            Backend::Noop,
        )
    }

    /// The deadline for the tests that are not about the deadline. `cargo test`
    /// runs these in parallel and each one spawns a process, so a one-second
    /// deadline here would be testing the machine's load rather than the worker.
    fn generous() -> Duration {
        Duration::from_secs(30)
    }

    /// The deadline of the test that *is* about the deadline.
    ///
    /// It has to be short enough that this test proves the escalation rather than
    /// the 300 s default of 1.7.1, and long enough that it cannot be defeated by
    /// the child's own start-up: the child has to be forked, exec'd, and have
    /// installed its `TERM` trap before the deadline can fire, and a `SIGTERM`
    /// delivered before the trap exists kills it by default action, which is a
    /// failure that says nothing about the daemon.
    ///
    /// One second was not long enough. Reproduced on this machine (macOS, 6 `sh`
    /// busy loops plus the eight test binaries of `cargo test --workspace` in
    /// parallel): 1 run in 8 failed on `elapsed >= TERM_GRACE` with the child
    /// dead in about 1 s, which is that race and not a daemon defect. Three
    /// seconds is 100x below the documented default and comfortably longer than
    /// any process start-up observed here; the trap is also installed before the
    /// pid file is written, so a child that is slow to start survives the signal
    /// whenever the two orderings can still be reconciled.
    const DEADLINE: Duration = Duration::from_secs(3);

    /// Two files are the same file: the device and inode comparison behind
    /// `[ a -ef b ]`, used to ask whether this process's own stdin is the null
    /// device.
    fn same_file(left: &str, right: &str) -> bool {
        match (fs::metadata(left), fs::metadata(right)) {
            (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
            _ => false,
        }
    }

    /// 1.6's environment rule, as a set equality rather than a membership test:
    /// the child sees the two names that are always set, the two the daemon
    /// adds, 4.3's two path knobs when the daemon's own environment has them, the
    /// API key when the daemon has one, the nine Linux session variables on Linux
    /// when they are set, and nothing else. The shell sets `PWD`, `SHLVL` and `_`
    /// for itself, which is why they are named here instead of silently tolerated.
    ///
    /// An implementation that forgot `env_clear()` fails on the extra names: the
    /// test process's own environment has at least `CARGO_*` in it under
    /// `cargo test`, and the assertion prints both sets when it fails.
    #[test]
    fn the_environment_is_exactly_the_names_1_6_lists() {
        let scripts = Scripts::new("env");
        let dump = scripts.path("env.txt");
        let program = scripts.script(
            "dump.sh",
            &format!("#!/bin/sh\nenv > '{}'\n", dump.display()),
        );

        let outcome = worker(program).run(Verb::Rotate, None, 1, generous());
        assert!(
            matches!(outcome, Err(WorkerError::Failed { .. })),
            "the script prints no set: line: {outcome:?}"
        );

        let mut names: Vec<String> = scripts
            .text("env.txt")
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_string()))
            .collect();
        names.sort();

        let mut expected: Vec<String> = ALWAYS.iter().map(|name| name.to_string()).collect();
        expected.push("WHIRL_CONFIG".to_string());
        expected.push("WHIRL_BACKEND".to_string());
        for name in PASSED_THROUGH {
            if std::env::var_os(name).is_some() {
                expected.push(name.to_string());
            }
        }
        if std::env::var_os("WHIRL_WALLHAVEN_API_KEY").is_some() {
            expected.push("WHIRL_WALLHAVEN_API_KEY".to_string());
        }
        if cfg!(target_os = "linux") {
            for name in LINUX_ONLY {
                if std::env::var_os(name).is_some() {
                    expected.push(name.to_string());
                }
            }
        }
        expected.sort();

        // The shell sets its own `PWD`, `SHLVL` and `_` for the process it
        // execs, and those are the *only* names the daemon did not put there
        // that are tolerated. They are named in one place and checked for
        // membership, so a variable smuggled in under a different name fails.
        const SHELL_OWNED: [&str; 3] = ["PWD", "SHLVL", "_"];
        let outside: Vec<&String> = names
            .iter()
            .filter(|name| !expected.contains(name))
            .collect();
        let unexpected: Vec<&&String> = outside
            .iter()
            .filter(|name| !SHELL_OWNED.contains(&name.as_str()))
            .collect();
        assert!(
            unexpected.is_empty(),
            "the worker saw names 1.6 does not list: {unexpected:?} (whole environment: {})",
            names.join(" ")
        );
        for name in &expected {
            assert!(
                names.contains(name),
                "{name} is missing from the worker's environment ({})",
                names.join(" ")
            );
        }
    }

    /// The pure half of the same rule, so the spawn and the list cannot drift:
    /// `PASSED_THROUGH` reaches the worker when the daemon's own environment has
    /// it, and is absent - not empty - when it does not. The lookup is the
    /// parameter here rather than the process environment, because a test that
    /// mutated its own environment would be racing every other test in this
    /// binary.
    #[test]
    fn the_4_3_path_knobs_are_passed_through_only_when_the_daemon_has_them() {
        fn names(environment: Vec<(String, OsString)>) -> Vec<String> {
            environment.into_iter().map(|(name, _)| name).collect()
        }

        let scratch = |name: &str| match name {
            "PATH" | "HOME" | "WHIRL_CACHE_DIR" | "WHIRL_STATE_DIR" => {
                Some(OsString::from(format!("/{name}")))
            }
            _ => None,
        };
        assert_eq!(
            names(forwarded(&scratch)),
            ["PATH", "HOME", "WHIRL_CACHE_DIR", "WHIRL_STATE_DIR"],
            "1.6's list, and the two knobs carry the daemon's own values"
        );
        assert_eq!(
            forwarded(&scratch)
                .into_iter()
                .filter(|(name, _)| name == "WHIRL_CACHE_DIR")
                .map(|(_, value)| value)
                .collect::<Vec<OsString>>(),
            [OsString::from("/WHIRL_CACHE_DIR")],
            "the value the worker is given is the daemon's, not a re-resolution"
        );

        let bare = |name: &str| match name {
            "PATH" | "HOME" => Some(OsString::from("/x")),
            _ => None,
        };
        assert_eq!(
            names(forwarded(&bare)),
            ["PATH", "HOME"],
            "an unset knob is not passed through as an empty value"
        );
    }

    /// How closely the test can pin the *early* side of the deadline: how much
    /// before `DEADLINE` a `SIGTERM` may be and still count as at it.
    ///
    /// The reading it is applied to is a late-biased one and can never lead the
    /// signal: the watcher polls every `WATCH_POLL`, and the shell runs a trap
    /// only once the command it is executing returns, which in the script below
    /// is one `sleep 0.05`. So the marker lags the true `SIGTERM` by up to about
    /// 52 ms plus however long the watching thread waited for a core, and 500 ms
    /// covers that with room to spare on a load-parallel `cargo test
    /// --workspace`. It is also six times below the 3 s the defect this asserts
    /// against is off by, so the mutation in the pull request still fails by
    /// thousands of milliseconds rather than by a hair.
    const TOLERANCE: Duration = Duration::from_millis(500);

    /// The tolerance on the *late* side: how much after the deadline the
    /// `SIGTERM` may be, and how much after `DEADLINE + TERM_GRACE` the total
    /// may be. Deliberately looser than `TOLERANCE`, because it is a sanity
    /// check on the total rather than one of the two facts this test exists for:
    /// it catches an escalation that runs away (a worker never killed, a wait
    /// measured in seconds instead of milliseconds), and the cost of being wrong
    /// the other way is a test the scheduler can fail, which would be worse than
    /// missing a defect this card does not concern.
    const SLACK: Duration = Duration::from_secs(2);

    /// How often the watcher below looks for the marker file.
    const WATCH_POLL: Duration = Duration::from_millis(2);

    /// The watcher's own bail-out, so a worker that is never signalled cannot
    /// leave a thread spinning past the end of the test. Longer than the
    /// deadline plus the grace, which is all the run can take.
    const WATCH_BOUND: Duration = Duration::from_secs(30);

    /// When a `TERM` trap was seen to run, on the test's own clock.
    ///
    /// A timestamp written by the trap itself would be a wall-clock reading
    /// taken in another process, and the test has no way to compare that with
    /// the `Instant` it takes before the spawn. So the trap keeps its one job,
    /// appending to the marker file, and this thread watches for that file and
    /// records an `Instant` the moment it exists: one clock, one process,
    /// nothing to correlate.
    ///
    /// The reading is always at or after the signal, never before it, as
    /// described on `TOLERANCE`.
    struct TermWatch {
        stop: Arc<AtomicBool>,
        running: JoinHandle<()>,
        seen: mpsc::Receiver<Instant>,
    }

    impl TermWatch {
        fn new(marker: PathBuf) -> TermWatch {
            let (sender, seen) = mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let watching = Arc::clone(&stop);
            let running = std::thread::spawn(move || {
                let give_up = Instant::now() + WATCH_BOUND;
                loop {
                    if marker.exists() {
                        let _ = sender.send(Instant::now());
                        return;
                    }
                    if watching.load(Ordering::SeqCst) || Instant::now() >= give_up {
                        // A marker written in the same instant the test stopped
                        // watching is still reported rather than lost to the
                        // race, since the file outlives both threads.
                        if marker.exists() {
                            let _ = sender.send(Instant::now());
                        }
                        return;
                    }
                    std::thread::sleep(WATCH_POLL);
                }
            });
            TermWatch {
                stop,
                running,
                seen,
            }
        }

        /// The reading, once the watcher has stopped. `None` if the marker never
        /// appeared at all.
        fn moment(self) -> Option<Instant> {
            self.stop.store(true, Ordering::SeqCst);
            let _ = self.running.join();
            self.seen.try_recv().ok()
        }
    }

    /// `SIGTERM` **at the deadline**, the whole five-second grace, then
    /// `SIGKILL` (1.7.1). The script appends to the marker file from its `TERM`
    /// trap and keeps running through the signal, so the marker proves the
    /// polite signal arrived and the exit proves the kill was needed and worked.
    ///
    /// Three facts, not one, and the reason is the earlier version of this test.
    /// It asserted only `elapsed >= TERM_GRACE`, which a daemon that sent
    /// `SIGTERM` the instant the child existed and `SIGKILL` five seconds later
    /// satisfied, while never looking at the deadline at all: the test passed
    /// for the wrong reason. The moment the trap ran is now recorded, and
    /// checked against the deadline itself:
    ///
    /// 1. the trap ran no earlier than the deadline, within `TOLERANCE`;
    /// 2. it did not run late either, within `SLACK`;
    /// 3. the exit came at least the grace after the trap, within `TOLERANCE`;
    /// 4. and the total is the deadline plus the grace, within those bounds.
    ///
    /// A daemon that skipped `SIGTERM` fails 1 and the marker assertion; one
    /// that killed alongside the signal fails 3 and 4; one that returned without
    /// killing leaves the process alive and fails the `kill -0` probe.
    ///
    /// What it still cannot see: the instant of the signal itself, any closer
    /// than `TOLERANCE` early and `SLACK` late, and the signal by number. The
    /// signal is observed only through the shell's trap table, so this says
    /// "the trap the script installed for `TERM` ran", which on every shell
    /// here means `SIGTERM` and not that the daemon used `kill(1)` rather than
    /// a syscall.
    ///
    /// The `trap` is the script's first statement and the pid file its second:
    /// both orderings are load-sensitive, and installing the handler before
    /// anything that can block keeps the window in which a `SIGTERM` would kill
    /// the child by default action as small as the shell can make it.
    #[test]
    fn a_slow_worker_is_termed_at_the_deadline_and_killed_after_the_grace() {
        let scripts = Scripts::new("escalate");
        let marker = scripts.path("term.txt");
        let pid = scripts.path("pid.txt");
        let program = scripts.script(
            "slow.sh",
            &format!(
                "#!/bin/sh\ntrap 'echo term >> \"{}\"' TERM\necho $$ > '{}'\nwhile :; do sleep 0.05; done\n",
                marker.display(),
                pid.display()
            ),
        );

        let started = Instant::now();
        let watch = TermWatch::new(marker.clone());
        let outcome = worker(program).run(Verb::Rotate, None, 7, DEADLINE);
        let elapsed = started.elapsed();
        let termed_at = match watch.moment() {
            Some(moment) => moment.duration_since(started),
            None => panic!(
                "no SIGTERM trap ran in {WATCH_BOUND:?}: {} never appeared",
                marker.display()
            ),
        };
        let after_term = elapsed - termed_at;

        assert!(
            matches!(outcome, Err(WorkerError::Timeout)),
            "the deadline expired, whatever the worker did on the way out: {outcome:?}"
        );
        assert!(
            termed_at >= DEADLINE - TOLERANCE,
            "SIGTERM is sent at the {DEADLINE:?} deadline, not before it: the trap ran {termed_at:?} after the spawn, and only {TOLERANCE:?} of tolerance is allowed"
        );
        assert!(
            termed_at <= DEADLINE + SLACK,
            "and not after it either: the trap ran {termed_at:?} after the spawn, and only {SLACK:?} of slack is allowed"
        );
        assert!(
            after_term >= TERM_GRACE - TOLERANCE,
            "the worker is killed a whole {TERM_GRACE:?} after SIGTERM: the exit came {after_term:?} after the trap ran"
        );
        assert!(
            elapsed >= DEADLINE + TERM_GRACE - TOLERANCE,
            "the total is the deadline plus the grace: {elapsed:?} elapsed, at least {DEADLINE:?} plus {TERM_GRACE:?} expected"
        );
        assert!(
            elapsed <= DEADLINE + TERM_GRACE + SLACK,
            "and no more than that plus slack: {elapsed:?} elapsed"
        );
        assert!(
            scripts.exists("term.txt"),
            "the worker was sent SIGTERM before being killed"
        );
        assert_eq!(
            scripts.text("term.txt").trim(),
            "term",
            "the trap ran once, on the one SIGTERM"
        );

        let pid: u32 = scripts.text("pid.txt").trim().parse().expect("a pid");
        let probe = Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("kill -0");
        assert!(
            !probe.success(),
            "the worker was killed and reaped, so {pid} is gone"
        );
    }

    /// A worker that dies on its own reports the code it named on stderr (2.7),
    /// and the daemon does not wait for the deadline to notice.
    #[test]
    fn a_worker_that_names_a_code_reports_it() {
        let scripts = Scripts::new("named-code");
        let program = scripts.script(
            "fail.sh",
            "#!/bin/sh\necho 'stage=set code=set_failed message=the setter refused' >&2\nexit 3\n",
        );

        let started = Instant::now();
        let outcome = worker(program).run(Verb::Rotate, None, 1, Duration::from_secs(300));
        let elapsed = started.elapsed();

        match outcome {
            Err(WorkerError::Failed { code, message }) => {
                assert_eq!(code, ErrorCode::SetFailed);
                assert_eq!(
                    message,
                    "stage=set code=set_failed message=the setter refused"
                );
            }
            other => panic!("the worker's own code wins over worker_failed: {other:?}"),
        }
        assert!(
            elapsed < Duration::from_secs(5),
            "the exit was seen and not waited out: {elapsed:?}"
        );
    }

    /// A worker that dies silently is `worker_failed`, with whatever it said as
    /// the message: 2.7's code for an exit the daemon cannot explain.
    #[test]
    fn a_silent_death_is_worker_failed() {
        let scripts = Scripts::new("silent-death");
        let program = scripts.script(
            "crash.sh",
            "#!/bin/sh\necho 'whirl-worker: the download stage panicked' >&2\nexit 1\n",
        );

        match worker(program).run(Verb::Rotate, None, 1, generous()) {
            Err(WorkerError::Failed { code, message }) => {
                assert_eq!(code, ErrorCode::WorkerFailed);
                assert_eq!(message, "whirl-worker: the download stage panicked");
            }
            other => panic!("a non-zero exit with no code: {other:?}"),
        }
    }

    /// A worker that exits 0 having printed no `set:` line is `worker_failed`
    /// too, and the message names what it did print, because that line is all
    /// the evidence there is (1.6's stdout shape).
    #[test]
    fn a_zero_exit_without_a_set_line_is_worker_failed() {
        let scripts = Scripts::new("no-set-line");
        let program = scripts.script("quiet.sh", "#!/bin/sh\necho 'downloaded: ok'\n");

        match worker(program).run(Verb::Rotate, None, 1, generous()) {
            Err(WorkerError::Failed { code, message }) => {
                assert_eq!(code, ErrorCode::WorkerFailed);
                assert!(
                    message.contains("downloaded: ok"),
                    "the offending line is in the message: {message}"
                );
            }
            other => panic!("exit 0 without a set: line is a failure: {other:?}"),
        }
    }

    /// The happy path of the stdout parse: the *last* non-empty line is the
    /// result, and a trailing carriage return is trimmed off it. Both are
    /// asserted on the parsed record, so a parse that took the first line or
    /// left the `\r` in the path fails on a field.
    ///
    /// The line here is the worker's own (`set: <digest> <origin_key> <abs
    /// path>`, 1.6's three fields), which is *not* the four-field `set:` line of
    /// 2.6: that one carries the `via` and is written by the daemon to a client.
    #[test]
    fn the_last_non_empty_line_is_the_result() {
        let scripts = Scripts::new("last-line");
        let digest = "a".repeat(64);
        let program = scripts.script(
            "set.sh",
            &format!(
                "#!/bin/sh\nprintf 'downloaded: {digest} /tmp/candidate.jpg\\n'\nprintf 'set: {digest} pictures:0123456789abcdef /tmp/candidate.jpg\\r'\n"
            ),
        );

        match worker(program).run(Verb::Rotate, None, 7, generous()) {
            Ok(Outcome::Set(record)) => {
                assert_eq!(record.digest, digest);
                assert_eq!(record.origin_key, "pictures:0123456789abcdef");
                assert_eq!(record.path.as_deref(), Some("/tmp/candidate.jpg"));
                assert_eq!(record.via, Via::Source);
            }
            other => panic!("the last line is a set: line: {other:?}"),
        }
    }

    /// 1.6's stdin rule: the worker's stdin is the null device, not whatever the
    /// daemon has. The check is `[ /dev/fd/0 -ef /dev/null ]`, and the test says
    /// so out loud when its own stdin is *also* the null device, because then an
    /// inherited stdin would look the same and the assertion would prove
    /// nothing.
    #[test]
    fn the_worker_reads_the_null_device_not_the_daemons_stdin() {
        let scripts = Scripts::new("stdin");
        let digest = "b".repeat(64);
        let program = scripts.script(
            "stdin.sh",
            &format!(
                "#!/bin/sh\nif [ /dev/fd/0 -ef /dev/null ]; then\n  printf 'set: {digest} pictures:fedcba9876543210 /tmp/candidate.jpg\\n'\nelse\n  echo 'stage=stdin code=bad_args message=stdin was inherited' >&2\n  exit 3\nfi\n"
            ),
        );

        if same_file("/dev/fd/0", "/dev/null") {
            eprintln!(
                "note: this harness's own stdin is the null device, so an inherited stdin would be indistinguishable; the assertion below is vacuous here"
            );
        }
        match worker(program).run(Verb::Rotate, None, 1, generous()) {
            Ok(Outcome::Set(record)) => {
                assert_eq!(record.path.as_deref(), Some("/tmp/candidate.jpg"))
            }
            other => panic!("the worker's stdin must be the null device: {other:?}"),
        }
    }

    /// How long the writer of `a_spawn_that_finds_the_script_busy_is_retried`
    /// holds the script open for write.
    ///
    /// It is held before the run and released 100 ms into it, which is what makes
    /// the first spawn attempt find the writer still there: without the retry
    /// this test is red on Linux, and red with the CI's own message. It is also
    /// short enough that a release thread descheduled for hundreds of
    /// milliseconds still lands inside `SPAWN_ATTEMPTS`'s 450 ms budget, and long
    /// enough that a pass cannot come from the release winning the race to the
    /// first spawn.
    const WRITER_HELD: Duration = Duration::from_millis(100);

    /// A spawn the kernel refuses with `ETXTBSY` is re-attempted, and the retry
    /// really runs the worker: the assertion is the parsed `Outcome::Set` and its
    /// fields, not "the spawn did not return an error".
    ///
    /// The rule being exercised is Linux's: `execve` refuses a file that any
    /// process holds open for writing (`deny_write_access` on the exec target)
    /// with `ETXTBSY`. In the CI failure that produced this test the holder was a
    /// child forked from a *sibling* thread while `Scripts::script`'s writable
    /// descriptor was still open, because `fork` duplicates the descriptor into
    /// the child and the child can keep it past the write in the parent. That
    /// interleaving cannot be constructed deterministically from a test, so the
    /// writer here stands in for it: the same kernel rule, the same refusal, the
    /// same release.
    ///
    /// Darwin does not enforce the rule at all, so on macOS this test is vacuous
    /// (the first spawn simply succeeds); only Linux makes it bite.
    #[test]
    fn a_spawn_that_finds_the_script_busy_is_retried() {
        let scripts = Scripts::new("busy");
        let digest = "c".repeat(64);
        let program = scripts.script(
            "busy.sh",
            &format!(
                "#!/bin/sh\nprintf 'set: {digest} pictures:0123456789abcdef /tmp/candidate.jpg\\n'\n"
            ),
        );

        // Held before the run, so the first attempt cannot miss it.
        let writer = fs::OpenOptions::new()
            .write(true)
            .open(&program)
            .expect("a writable handle on the script");
        let releasing = std::thread::spawn(move || {
            std::thread::sleep(WRITER_HELD);
            drop(writer);
        });

        let outcome = worker(program).run(Verb::Rotate, None, 1, generous());
        releasing.join().expect("the writer thread");

        match outcome {
            Ok(Outcome::Set(record)) => {
                assert_eq!(record.digest, digest);
                assert_eq!(record.origin_key, "pictures:0123456789abcdef");
                assert_eq!(record.path.as_deref(), Some("/tmp/candidate.jpg"));
                assert_eq!(record.via, Via::Source);
            }
            other => panic!("a script that is busy for 100 ms is not a failed spawn: {other:?}"),
        }
    }
}
