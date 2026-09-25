//! `whirld`: the resident daemon (docs/architecture.md 1.1).
//!
//! One lightweight process, no GUI toolkit, no image bytes, no scheduler
//! library: it owns the control socket, the state directory and the cache
//! directory, and it spawns `whirl-worker` for anything that needs to touch a
//! file or a platform (1.6, 1.9).
//!
//! ```text
//! whirld [--config <path>] [--socket <path>] [--backend <native|noop>]
//! ```
//!
//! This is the scaffold: the socket, the protocol, the config, the worker
//! contract, the state files of `docs/spec/state-and-cache.md` section 6 and the
//! `subscribe` stream of docs/architecture.md 2.9 are real, and the scheduler,
//! the cache, the sweep and the wallpaper backends are later cards. It logs to
//! stderr, because a scaffold is run in the foreground (docs/development.md
//! section 7) and the log file of `[D 6 §7.2]` arrives with the daemon that has
//! something to rotate.

#[cfg(unix)]
mod events;
#[cfg(unix)]
mod plan;
#[cfg(unix)]
mod schedule;
#[cfg(unix)]
mod socket;
#[cfg(unix)]
mod state;
#[cfg(unix)]
mod statefile;
#[cfg(unix)]
mod worker;

use std::process::ExitCode;
#[cfg(unix)]
use std::sync::Arc;

const USAGE: &str = "usage: whirld [--config <path>] [--socket <path>] [--backend <native|noop>]";

/// The transport of docs/architecture.md 2.1 is a Unix domain socket in this
/// build. On Windows that document asks for a named pipe with an explicit DACL,
/// which is a later card: until it lands there is no startup path to compile, so
/// this binary refuses to start rather than binding something weaker.
#[cfg(not(unix))]
fn main() -> ExitCode {
    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    eprintln!(
        "whirld: the control socket is a unix domain socket in this scaffold; the Windows named pipe of docs/architecture.md 2.1 is not implemented yet"
    );
    ExitCode::from(1)
}

#[cfg(unix)]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let flags = match plan::Flags::parse(&args) {
        Ok(flags) => flags,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&flags) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("whirld: {message}");
            ExitCode::from(1)
        }
    }
}

#[cfg(unix)]
fn run(flags: &plan::Flags) -> Result<(), String> {
    let effective = plan::Effective::resolve(flags)?;
    eprintln!("whirld: config {}", effective.config_path.display());
    eprintln!(
        "whirld: state {} cache {}",
        effective.state_dir.display(),
        effective.cache_dir.display()
    );
    eprintln!("whirld: backend {}", effective.backend.as_str());

    let program = worker::Worker::default_program();
    let worker = worker::Worker::new(program, effective.config_path.clone(), effective.backend);
    let socket_path = effective.socket_path.clone();
    // The state files are read here: a quarantine, a rebuild and the degraded
    // modes of docs/spec/state-and-cache.md 6.4 all happen before the socket is
    // bound, so the first `status` already reports them.
    let daemon = Arc::new(state::Daemon::load(effective, worker));
    let listener = socket::bind(&socket_path)?;
    eprintln!("whirld: listening on {}", socket_path.display());

    // The accept loop runs until the process is signalled, which is the
    // supervisor's job to do (1.5).
    socket::serve(listener, daemon);
    Ok(())
}
