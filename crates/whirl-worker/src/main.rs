//! `whirl-worker`: one rotation, one process (docs/architecture.md 1.6).
//!
//! The argv, the environment and stdout are the whole interface: the daemon
//! spawns this binary as a child with a scrubbed environment, reads its last
//! non-empty stdout line as the result and keeps the whole capture for the log.
//! Nothing here writes state, because the daemon owns every file (1.9).
//!
//! ```text
//! whirl-worker --config <abs path> --verb <rotate|set|check> [--target <path|id>] --run <rotation id>
//! ```
//!
//! A scaffold no longer: the argv, the environment and stdout are the contract
//! (1.6), and the stages behind them are implemented end to end except the
//! sources themselves. `local` and `wallhaven` are separate cards, so
//! [`sources::Sources::from_config`] returns an empty table today and a rotation
//! fails with `no_candidates` and a message that names the missing kind.

mod backend;
mod http;
mod lock;
mod pipeline;
mod sources;

use std::path::PathBuf;
use std::process::ExitCode;
use whirl_core::config::{Backend, Config};
use whirl_core::protocol::ErrorCode;

const USAGE: &str = "usage: whirl-worker --config <abs path> --verb <rotate|set|check> [--target <path|id>] --run <rotation id>";

/// The three verbs the daemon spawns (docs/architecture.md 1.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Rotate,
    Set,
    Check,
}

impl Verb {
    fn parse(name: &str) -> Option<Verb> {
        match name {
            "rotate" => Some(Verb::Rotate),
            "set" => Some(Verb::Set),
            "check" => Some(Verb::Check),
            _ => None,
        }
    }
}

struct Args {
    config: PathBuf,
    verb: Verb,
    target: Option<String>,
    /// The daemon's monotonic slot counter (1.6). The scaffold only echoes it in
    /// a failure message: there is no cache, so there is no part file to name.
    run: u64,
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
        let mut config: Option<PathBuf> = None;
        let mut verb: Option<Verb> = None;
        let mut target: Option<String> = None;
        let mut run: Option<u64> = None;
        let mut args = args.peekable();
        while let Some(flag) = args.next() {
            let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
            match flag.as_str() {
                "--config" => {
                    let path = PathBuf::from(&value);
                    if !path.is_absolute() {
                        return Err(format!("--config must be an absolute path, got {value}"));
                    }
                    config = Some(path);
                }
                "--verb" => {
                    verb =
                        Some(Verb::parse(&value).ok_or_else(|| {
                            format!("--verb {value} is not rotate, set or check")
                        })?);
                }
                "--target" => target = Some(value),
                "--run" => {
                    run = Some(
                        value
                            .parse::<u64>()
                            .map_err(|_| format!("--run {value} is not a rotation id"))?,
                    );
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        Ok(Args {
            config: config.ok_or("--config is required")?,
            verb: verb.ok_or("--verb is required")?,
            target,
            run: run.ok_or("--run is required")?,
        })
    }
}

fn main() -> ExitCode {
    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    let config = match load_config(&args.config) {
        Ok(config) => config,
        Err(failure) => return failure.report(),
    };
    // 7.2 and 7.3 step 2: the worker's half of `rotate.lock`, taken after the
    // config (a config this worker cannot read is `bad_config`, not `busy`) and
    // before anything that touches the cache, then held for the rest of this
    // process's life: the guard is dropped as `main` returns, and under `flock`
    // the kernel releases the lock on any exit, `SIGKILL` included.
    let _rotation_lock = match args.verb {
        // A check is not a rotation: 2.5's `config check` row gives that verb
        // `bad_config`, `worker_failed` and `timeout` and no `busy`, and a check
        // writes nothing into the cache, so it takes no lock and a check during a
        // rotation still answers.
        Verb::Check => None,
        Verb::Rotate | Verb::Set => match rotation_lock() {
            Ok(guard) => Some(guard),
            Err(failure) => return failure.report(),
        },
    };
    let backend = match backend(&config) {
        Ok(backend) => backend,
        Err(failure) => return failure.report(),
    };

    let sources = sources::Sources::from_config(&config);
    let cache = match pipeline::Cache::resolve(&config) {
        Ok(cache) => cache,
        Err(failure) => return failure.report(),
    };
    // The recent window is the history ring plus the index, read and never
    // written: the daemon owns both files (4.1, 7.2).
    let window = match pipeline::state_directory() {
        Some(state_dir) => pipeline::Window::load(
            &state_dir,
            &cache.index_path(),
            config.dedupe.recent_entries,
        ),
        None => pipeline::Window::empty(),
    };
    let platform = pipeline::host_platform();
    let run = pipeline::Run {
        config: &config,
        setter: &pipeline::PlatformSet(backend),
        sources: &sources,
        transport: &pipeline::Paths,
        cache: &cache,
        window: &window,
        platform,
        run: args.run,
        draw: pipeline::draw(args.run),
    };

    let result = match args.verb {
        Verb::Check => {
            pipeline::check(&config, backend, &sources, &window, platform);
            return ExitCode::SUCCESS;
        }
        Verb::Rotate => run.rotate(),
        Verb::Set => match args.target.as_deref() {
            Some(target) => run.set(target),
            None => Err(pipeline::Failure::new(
                "set",
                ErrorCode::BadArgs,
                "--target is required for `set`",
            )),
        },
    };
    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(failure) => failure.report(),
    }
}

/// The config the daemon named, read and validated here: the worker is a
/// separate process and must not trust a document it has not parsed (1.6).
fn load_config(path: &PathBuf) -> Result<Config, pipeline::Failure> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        pipeline::Failure::new(
            "config",
            ErrorCode::BadConfig,
            format!("{}: {error}", path.display()),
        )
    })?;
    let loaded = Config::parse(&text).map_err(|error| {
        pipeline::Failure::new(
            "config",
            ErrorCode::BadConfig,
            error.with_path(path).to_string(),
        )
    })?;
    for warning in &loaded.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(loaded.config)
}

/// `WHIRL_BACKEND` is always set by the daemon to the backend it resolved, so a
/// value that is set but unparsable is a bug in the caller, not a default
/// (docs/development.md section 7 names `noop` as the safe path).
fn backend(config: &Config) -> Result<Backend, pipeline::Failure> {
    match std::env::var("WHIRL_BACKEND") {
        Ok(name) => Backend::parse(&name).ok_or_else(|| {
            pipeline::Failure::new(
                "backend",
                ErrorCode::BadConfig,
                format!("WHIRL_BACKEND={name:?} is neither native nor noop"),
            )
        }),
        Err(_) => Ok(config.backend),
    }
}

/// 7.3 step 2: take `state/locks/rotate.lock` non-blocking, or refuse this run.
///
/// The directory is the one `WHIRL_STATE_DIR` names, which 1.6 has the daemon set
/// to the directory **it** resolved: the lock has to be the file the daemon's
/// sweep takes, and that is the daemon's answer and not anything in the config
/// document (4.3). `pipeline::state_directory` is the same resolution the recent
/// window of 4.1 already uses.
///
/// Contention is `busy` and not an error of the worker's (7.3 step 2: "its worker
/// fails the non-blocking lock and exits `busy`, which is a fact the user can
/// see"). A lock that cannot be taken or created at all is `internal`, with the
/// reason in the message: 2.7 has no code for "the state directory is not
/// there", and a code the spec does not define would be a worse answer than the
/// sentence that says what happened.
fn rotation_lock() -> Result<lock::RotateLock, pipeline::Failure> {
    let state_dir = pipeline::state_directory().ok_or_else(|| {
        pipeline::Failure::new(
            "lock",
            ErrorCode::Internal,
            "no state directory: neither WHIRL_STATE_DIR nor a platform default \
             resolved, so the rotation lock of 7.2 cannot be taken",
        )
    })?;
    match lock::take(&state_dir) {
        Ok(Some(guard)) => Ok(guard),
        Ok(None) => Err(pipeline::Failure::new(
            "lock",
            ErrorCode::Busy,
            format!(
                "another rotation or a sweep holds {}",
                state_dir.join(lock::ROTATE_FILE).display()
            ),
        )),
        Err(message) => Err(pipeline::Failure::new("lock", ErrorCode::Internal, message)),
    }
}
