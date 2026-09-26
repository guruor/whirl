//! Turning protocol lines into a terminal's output and an exit code.
//!
//! The grammar already classifies a line ([`protocol::classify_line`]); this
//! module is the CLI's policy on top of it, and it is separate so the policy is
//! readable in one place: what is printed, what is a failure, and which of the
//! four exit codes of docs/architecture.md 2.5.1 an answer earns.
//!
//! The four exit codes are the whole program's, so they compile everywhere. The
//! verdict policy and the greeting check are read by the transport, which is a
//! Unix domain socket in this build (docs/architecture.md 2.1 gives Windows a
//! named pipe, a later card), so those items are `cfg(unix)`.

#[cfg(unix)]
use whirl_core::protocol::{self, ErrorCode, LineKind};

/// The verb completed (2.5.1).
pub const EXIT_OK: u8 = 0;
/// The daemon refused, and the `ERR` code says why. Exactly one meaning. Only
/// the transport can see a refusal, so like it this is `cfg(unix)`.
#[cfg(unix)]
pub const EXIT_REFUSED: u8 = 1;
/// The daemon is not reachable: nothing is listening on the socket.
pub const EXIT_UNREACHABLE: u8 = 2;
/// The command line was wrong, or this client and that daemon do not agree on
/// the protocol. A usage error is not a failure of the daemon.
pub const EXIT_USAGE: u8 = 3;

/// What the CLI does with one line from the daemon.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// Print it and keep reading.
    Line,
    /// Print it, and this one ends the subscription (`whirl idle`).
    Event,
    /// The response is complete: exit 0.
    Done,
    /// The response is a refusal: print it to stderr, exit 1.
    Refused(ErrorCode),
    /// A data line with no `key: ` prefix, which the grammar does not allow.
    Malformed,
}

#[cfg(unix)]
pub fn verdict(line: &str) -> Exit {
    match protocol::classify_line(line) {
        LineKind::Ok => Exit::Done,
        LineKind::Err { code, .. } => Exit::Refused(code),
        LineKind::Event => Exit::Event,
        LineKind::Malformed => Exit::Malformed,
        // The greeting, `queued`, and every `key: value` or positional record
        // line: printed as it arrived, because the CLI formats nothing.
        LineKind::Greeting | LineKind::Interim | LineKind::Data => Exit::Line,
    }
}

/// The exit code for a refusal. One code, because the reason travels on the wire
/// in the `ERR` line: a second code here would be a second answer to the same
/// question (2.5.1's "1 keeps exactly one meaning").
#[cfg(unix)]
pub fn refused(_code: ErrorCode) -> u8 {
    EXIT_REFUSED
}

/// The greeting is `OK whirl <version> protocol <n>` (2.4). A client refuses to
/// run against a protocol it does not know rather than misreading answers.
#[cfg(unix)]
pub fn check_greeting(line: &str) -> Result<(), String> {
    let fields:Vec<&str>=line.split(' ').collect();
    if fields.len() != 5 || fields[0] != "OK" {
        return Err(format!(
            "the greeting is not `OK <product> <version> protocol <n>`: {line:?}"
        ));
    }
    if fields[1] != protocol::PRODUCT {
        return Err(format!(
            "the daemon at this socket is {}, not whirl; this client speaks to whirl only",
            fields[1]
        ));
    }
    match fields[4].parse::<u32>() {
        Ok(version) if version == protocol::PROTOCOL_VERSION => Ok(()),
        Ok(version) => Err(format!(
            "the daemon speaks protocol {version}; this whirl speaks {}",
            protocol::PROTOCOL_VERSION
        )),
        Err(_) => Err(format!("the greeting names no protocol version: {line:?}")),
    }
}
