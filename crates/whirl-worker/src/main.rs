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
