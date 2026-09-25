//! The worker contract: argv, the scrubbed environment, the deadline and the
//! stdout parse (docs/architecture.md 1.6 and 1.7).
//!
//! The daemon spawns `whirl-worker` exactly as 1.6 specifies and reads the last
//! non-empty stdout line as the result. The environment is `env_clear()` plus
//! the names 1.6 lists, which is the fix for `[M 16]` (the prototype inherited
//! the daemon's whole environment) and what lets the Linux adapters see the
//! session signals they need.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use whirl_core::config::Backend;
use whirl_core::protocol::{self, ErrorCode, SetRecord, Via};

/// The variables the worker always gets (docs/architecture.md 1.6).
const ALWAYS: [&str; 2] = ["PATH", "HOME"];

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
        for name in ALWAYS {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
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

        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| WorkerError::Failed {
                code: ErrorCode::WorkerFailed,
                message: format!("cannot spawn {}: {error}", self.program.display()),
            })?;

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
