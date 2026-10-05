//! `whirl daemon ...`: the daemon's lifecycle, over the platform's supervisor
//! (docs/architecture.md 1.5, 5.1, 5.2 and section 8).
//!
//! Section 5.1 gives the daemon's lifetime to the OS supervisor and section 8
//! keeps a frontend from starting one. Both are right, and on their own they
//! leave nobody able to start the daemon at all: the app may not, and the OS was
//! never given a unit to start. So whirl owns the steps that put a unit in front
//! of the supervisor and take it back out, and this is where they live.
//!
//! Every step goes through the supervisor. Nothing here spawns `whirld`: `whirl
//! daemon start` asks the supervisor to run the job it already owns, so the
//! supervisor remains the only thing that can have a daemon, which is what keeps
//! a second one impossible (1.5, `[D 6 §7.2]`).
//!
//! macOS is the platform this build implements, because 5.2 is the platform
//! section with a unit that can be written from here. The per-user Task
//! Scheduler task of 5.3 and the `systemd --user` unit of 5.4 are deferred: a
//! platform without one refuses, in one sentence, rather than growing a
//! half-written unit.
//!
//! Exit codes are the CLI's own (render.rs): 0 the step was done, 1 whirl
//! refused, 2 there is no supervised daemon to reach (or the supervisor could
//! not be asked), 3 the command line was wrong.

use std::process::ExitCode;

#[cfg(not(target_os = "macos"))]
use crate::render::EXIT_REFUSED;

#[cfg(target_os = "macos")]
mod macos;

/// The five subcommands of `whirl daemon`, one per spelling in USAGE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// Write the unit and hand it to the supervisor.
    Install,
    /// Boot the job out and remove the unit, touching nothing else.
    Uninstall,
    /// Ask the supervisor to run the unit's job.
    Start,
    /// Ask the supervisor to unload the job.
    Stop,
    /// Report what the supervisor says about the job.
    Status,
}

impl Verb {
    /// The subcommand named by a word, or `None` for anything else, which is a
    /// usage error rather than a verb (2.5.1's exit 3).
    pub fn parse(word: &str) -> Option<Verb> {
        match word {
            "install" => Some(Verb::Install),
            "uninstall" => Some(Verb::Uninstall),
            "start" => Some(Verb::Start),
            "stop" => Some(Verb::Stop),
            "status" => Some(Verb::Status),
            _ => None,
        }
    }
}

/// The one sentence a platform with no supervisor implementation gets: it names
/// the platform and points at the sections that would describe the unit.
///
/// Compiled everywhere so that the sentence itself can be read and tested on a
/// machine this build does implement, and used on every platform that it does
/// not.
#[cfg(any(not(target_os = "macos"), test))]
fn unsupported(platform: &str) -> String {
    format!(
        "the daemon lifecycle has no {platform} implementation in this build, and no half of one: \
         the per-user unit and the supervisor steps are macOS only so far (docs/architecture.md \
         5.2, and 5.3 and 5.4 for the two that are still to come)"
    )
}

#[cfg(target_os = "macos")]
pub fn run(verb: Verb) -> ExitCode {
    macos::run(verb)
}

/// No supervisor means no lifecycle: whirl refuses rather than spawning a
/// daemon itself, which is the one thing 1.5 and section 8 both forbid.
#[cfg(not(target_os = "macos"))]
pub fn run(_verb: Verb) -> ExitCode {
    eprintln!("whirl: {}", unsupported(std::env::consts::OS));
    ExitCode::from(EXIT_REFUSED)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The platform that has no implementation is told so, by name, and given
    /// somewhere to read: a refusal a reader cannot act on is a defect
    /// (docs/architecture.md 5.3 and 5.4 are the two sections named).
    #[test]
    fn a_platform_without_a_supervisor_refuses_in_one_sentence() {
        let message = unsupported("linux");
        assert!(message.contains("linux"), "{message}");
        assert!(message.contains("docs/architecture.md 5.2"), "{message}");
        assert!(
            message.matches(". ").count() == 0,
            "one sentence: {message}"
        );
        assert!(
            !message.contains("systemd") && !message.contains("Task Scheduler"),
            "the deferred platforms are not half-implemented: {message}"
        );
    }

    /// The five words USAGE advertises are the five this accepts, and anything
    /// else is a usage error rather than a verb.
    #[test]
    fn the_subcommands_are_the_five_the_usage_names() {
        for (word, verb) in [
            ("install", Verb::Install),
            ("uninstall", Verb::Uninstall),
            ("start", Verb::Start),
            ("stop", Verb::Stop),
            ("status", Verb::Status),
        ] {
            assert_eq!(Verb::parse(word), Some(verb), "{word}");
        }
        for word in ["", "restart", "enable", "Install", "install "] {
            assert_eq!(Verb::parse(word), None, "{word}");
        }
    }
}
