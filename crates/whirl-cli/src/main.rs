//! `whirl`: the CLI (docs/architecture.md 2.5.1).
//!
//! Thin by design, and the table in 2.5.1 is the whole surface: one command line
//! becomes one protocol request, the data lines go to stdout, and the terminator
//! becomes the exit code (0 the verb completed, 1 the daemon refused, 2 the
//! daemon is unreachable, 3 the command line was wrong).
//!
//! Nothing the CLI reports about the daemon comes from a file it read itself.
//! The one exception is the socket path, which a client must know before it can
//! ask anything, and which it takes in the same precedence the daemon used:
//! `WHIRL_SOCKET`, then the config's `socket`, then the platform default.

mod render;

use render::{EXIT_OK, EXIT_REFUSED, EXIT_UNREACHABLE, EXIT_USAGE, Exit};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use whirl_core::config::{Config, paths};
use whirl_core::protocol::{self, DEFAULT_HISTORY_COUNT, MAX_HISTORY_COUNT, Request};

const USAGE: &str = "\
usage: whirl <command>

  next                 set the next image now
  prev                 set the previous image
  set <path|id>        set this image
  history [n]          the last n entries (default 10, max 50)
  favorites            pinned entries
  favorite [id]        pin the current entry, or the one named
  unfavorite <id>      unpin
  sources              configured sources
  status               daemon status
  version              protocol and daemon versions
  config path          the config file the daemon read
  config check         parse the config and report the effective values
  pause                stop rotating
  resume               rotate again
  idle                 follow events (this daemon does not implement subscribe yet)
  ping                 is the daemon there
  help                 this text";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return ExitCode::from(EXIT_OK);
    }
    if args.is_empty() {
        eprintln!("whirl: a command is required");
        eprintln!("{USAGE}");
        return ExitCode::from(EXIT_USAGE);
    }
    let request = match request(&args) {
        Ok(request) => request,
        Err(message) => {
            eprintln!("whirl: {message}");
            eprintln!("{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    match talk(&request) {
        Ok(exit) => exit,
        Err(TalkError::Unreachable { path, message }) => {
            // 2.5.1's first failure row, verbatim: the client never unlinks a
            // stale socket, it only says so.
            let stale = if path.exists() {
                " (stale socket; the daemon is not running)"
            } else {
                ""
            };
            eprintln!(
                "whirl: cannot reach the daemon at {}{stale}: {message}",
                path.display()
            );
            ExitCode::from(EXIT_UNREACHABLE)
        }
        Err(TalkError::Protocol(message)) => {
            // A greeting this build cannot speak to is neither a refusal by the
            // daemon (1) nor an unreachable socket (2), so it is reported as the
            // third kind of failure: this client and that daemon do not agree.
            eprintln!("whirl: {message}");
            ExitCode::from(EXIT_USAGE)
        }
        Err(TalkError::Io(error)) => {
            eprintln!("whirl: {error}");
            ExitCode::from(EXIT_REFUSED)
        }
    }
}

/// The CLI verb table of 2.5.1, and nothing else. `set` is the one place the
/// client guesses, and 2.5.1 puts the guess here on purpose: a wrong guess costs
/// exit code 3, not a wall of state.
fn request(args: &[String]) -> Result<Request, String> {
    let verb = args[0].as_str();
    let rest = &args[1..];
    let request = match (verb, rest) {
        ("next", []) => Request::Next,
        ("prev", []) => Request::Prev,
        ("status", []) => Request::Status,
        ("version", []) => Request::Version,
        ("sources", []) => Request::Sources,
        ("favorites", []) => Request::Favorites,
        ("pause", []) => Request::Pause,
        ("resume", []) => Request::Resume,
        ("ping", []) => Request::Ping,
        ("idle", []) => Request::Subscribe { since: None },
        ("set", [target]) => {
            if protocol::is_absolute_path(target) {
                Request::SetPath(target.clone())
            } else {
                Request::SetId(target.clone())
            }
        }
        ("history", []) => Request::History {
            count: DEFAULT_HISTORY_COUNT,
        },
        ("history", [count]) => {
            let count = count
                .parse::<usize>()
                .map_err(|_| format!("history: {count} is not a count"))?;
            Request::History {
                count: count.clamp(1, MAX_HISTORY_COUNT),
            }
        }
        ("favorite", []) => Request::Favorite { id: None },
        ("favorite", [id]) => Request::Favorite {
            id: Some(id.clone()),
        },
        ("unfavorite", [id]) => Request::Unfavorite(id.clone()),
        ("config", [path]) if path == "path" => Request::ConfigPath,
        ("config", [check]) if check == "check" => Request::ConfigCheck,
        (other, _) => {
            return Err(format!(
                "{other} is not a command, or it has the wrong number of arguments"
            ));
        }
    };
    Ok(request)
}

enum TalkError {
    Unreachable { path: PathBuf, message: String },
    Protocol(String),
    Io(std::io::Error),
}

/// One connection, one request, one response (2.3). The greeting is checked
/// before the request goes out, because a client that cannot read the answer
/// should not ask the question.
fn talk(request: &Request) -> Result<ExitCode, TalkError> {
    let path = socket_path().map_err(TalkError::Protocol)?;
    let stream = UnixStream::connect(&path).map_err(|error| TalkError::Unreachable {
        path: path.clone(),
        message: error.to_string(),
    })?;
    let mut reader = BufReader::new(stream.try_clone().map_err(TalkError::Io)?);
    let mut writer = stream;

    let greeting = read_line(&mut reader)?.ok_or_else(|| {
        TalkError::Protocol("the daemon closed the connection without a greeting".to_string())
    })?;
    render::check_greeting(&greeting).map_err(TalkError::Protocol)?;

    writer
        .write_all(request.encode().as_bytes())
        .map_err(TalkError::Io)?;
    writer.flush().map_err(TalkError::Io)?;

    // `whirl idle` is `subscribe` plus one event, then `close` (2.5.1).
    let one_event = matches!(request, Request::Subscribe { .. });
    let mut closed = false;
    loop {
        let line = read_line(&mut reader)?.ok_or_else(|| {
            TalkError::Protocol("the daemon closed the connection without a terminator".to_string())
        })?;
        match render::verdict(&line) {
            Exit::Done => return Ok(ExitCode::SUCCESS),
            Exit::Refused(code) => {
                // The ERR line goes to stderr as it arrived: the code is on the
                // wire, and 2.5.1 gives the CLI exactly one refusal exit code.
                eprintln!("{line}");
                return Ok(ExitCode::from(render::refused(code)));
            }
            Exit::Malformed => {
                return Err(TalkError::Protocol(format!(
                    "the daemon sent a line the grammar does not allow: {line:?}"
                )));
            }
            Exit::Event => {
                println!("{line}");
                if one_event && !closed {
                    closed = true;
                    writer
                        .write_all(Request::Close.encode().as_bytes())
                        .map_err(TalkError::Io)?;
                    writer.flush().map_err(TalkError::Io)?;
                }
            }
            Exit::Line => println!("{line}"),
        }
    }
}

fn read_line(reader: &mut impl BufRead) -> Result<Option<String>, TalkError> {
    let mut buffer = String::new();
    let read = reader.read_line(&mut buffer).map_err(TalkError::Io)?;
    if read == 0 {
        return Ok(None);
    }
    Ok(Some(buffer.trim_end_matches(['\n', '\r']).to_string()))
}

/// Where the daemon is listening. The client needs one path before it can ask
/// the daemon anything, and this is the only file the CLI reads: `WHIRL_SOCKET`,
/// then the config's `socket`, then the platform default (2.1).
fn socket_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("WHIRL_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    let config_path = std::env::var_os("WHIRL_CONFIG")
        .map(PathBuf::from)
        .or_else(paths::config_file);
    if let Some(config_path) = config_path {
        if let Ok(text) = std::fs::read_to_string(&config_path) {
            match Config::parse(&text) {
                Ok(loaded) => {
                    if let Some(socket) = loaded.config.socket {
                        return Ok(socket);
                    }
                }
                Err(error) => {
                    eprintln!(
                        "whirl: {}: {error}; using the default socket",
                        config_path.display()
                    );
                }
            }
        }
    }
    paths::socket_file().ok_or_else(|| {
        "no socket path: WHIRL_SOCKET is unset and this platform has no default".to_string()
    })
}
