//! The control socket: bind it `0600` under a `0077` umask, accept, and answer
//! the verbs (docs/architecture.md 2.1, 2.3).
//!
//! One thread per connection, bounded at 16 (1.8, `[L 4]`). The accept loop
//! never runs a worker: a rotation happens on the connection that asked for it,
//! which is what keeps `status` answering in one round trip while a worker runs.
//! Unix only in this scaffold: the Windows named pipe of 2.1 is a later card,
//! and `main` refuses to start there rather than pretending.

use crate::state::{Daemon, Resolved, favorite_state, now, platform};
use crate::worker::{Outcome, Verb, WorkerError};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use whirl_core::protocol::{self, ErrorCode, Request, Response, Via};
use whirl_core::state::{Favorite, HistoryEntry};

/// At most 16 concurrent connections; the 17th is answered `ERR busy` and
/// closed immediately (2.3 rule 7).
pub const MAX_CONNECTIONS: usize = 16;

/// `sun_path` is 104 bytes on macOS and 108 on Linux ([L 1], 2.1). The smaller
/// bound is the one that refuses a path early on both platforms.
pub const SUN_PATH_LIMIT: usize = 104;

/// `connection_idle_timeout` (2.8): 300 s without a complete request line.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn umask(mask: u16) -> u16;
}

#[cfg(not(target_os = "macos"))]
unsafe extern "C" {
    fn umask(mask: u32) -> u32;
}

/// The process umask, so the socket cannot be bound group- or world-connectable
/// for even one instruction (2.1, "Permissions, and the window between `bind`
/// and `chmod`"). `mode_t` is 16 bits on macOS and 32 elsewhere, which is the
/// only reason this is two functions.
#[cfg(target_os = "macos")]
fn set_umask(mask: u32) -> u32 {
    // SAFETY: `umask` is always safe to call; it takes no pointer and cannot
    // fail. The cast is to macOS's 16-bit `mode_t`.
    u32::from(unsafe { umask(mask as u16) })
}

/// See the macOS half above: the same call, with a 32-bit `mode_t`.
#[cfg(not(target_os = "macos"))]
fn set_umask(mask: u32) -> u32 {
    // SAFETY: `umask` is always safe to call; it takes no pointer and cannot
    // fail.
    unsafe { umask(mask) }
}

fn with_private_umask<T>(body: impl FnOnce() -> T) -> T {
    let previous = set_umask(0o077);
    let value = body();
    set_umask(previous);
    value
}

/// Bind the control socket, or refuse and name why.
///
/// A live socket at this path means another daemon: 1.5 step 5 unlinks only
/// after a connect probe returns `ECONNREFUSED`/`ENOENT`, never unconditionally,
/// which is the prototype's bug `[M 15]`.
pub fn bind(path: &Path) -> Result<UnixListener, String> {
    let bytes = path.as_os_str().as_bytes().len();
    if bytes >= SUN_PATH_LIMIT {
        return Err(format!(
            "the socket path {} is {bytes} bytes and the limit is {SUN_PATH_LIMIT}",
            path.display()
        ));
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        crate::plan::create_private_dir(parent)?;
    }
    if path.exists() {
        match UnixStream::connect(path) {
            Ok(_) => {
                return Err(format!(
                    "{} is a live socket: another daemon is listening",
                    path.display()
                ));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) =>
            {
                std::fs::remove_file(path).map_err(|error| {
                    format!("cannot remove the stale socket {}: {error}", path.display())
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "cannot probe the existing {}: {error}",
                    path.display()
                ));
            }
        }
    }
    let listener = with_private_umask(|| UnixListener::bind(path))
        .map_err(|error| format!("cannot bind {}: {error}", path.display()))?;
    // `0777 & ~0077` is 0700, so the group and other bits are already gone; this
    // is what removes the owner's execute bit and makes it exactly 0600.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot set mode 600 on {}: {error}", path.display()))?;
    Ok(listener)
}

/// The accept loop. Never returns: the daemon lives until it is signalled.
pub fn serve(listener: UnixListener, daemon: Arc<Daemon>) {
    let live = Arc::new(AtomicUsize::new(0));
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) => {
                // A failed accept is not fatal: the listener is still bound and
                // the next connection may succeed.
                eprintln!("whirld: accept failed: {error}");
                continue;
            }
        };
        if live.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            live.fetch_sub(1, Ordering::SeqCst);
            let mut stream = stream;
            let _ = stream.write_all(b"ERR busy too many clients (16)\n");
            let _ = stream.flush();
            continue;
        }
        let daemon = Arc::clone(&daemon);
        let live = Arc::clone(&live);
        std::thread::spawn(move || {
            if let Err(error) = handle(stream, &daemon) {
                if !matches!(
                    error.kind(),
                    io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
                ) {
                    eprintln!("whirld: connection ended: {error}");
                }
            }
            live.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

fn handle(stream: UnixStream, daemon: &Daemon) -> io::Result<()> {
    // The idle timeout of 2.8, applied to the socket rather than to a timer.
    stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    writeln!(writer, "{}", protocol::greeting())?;
    writer.flush()?;

    loop {
        match read_request_line(&mut reader) {
            Ok(ReadOutcome::Line(line)) => {
                if let Control::Close = dispatch(daemon, &line, &mut writer)? {
                    return Ok(());
                }
            }
            Ok(ReadOutcome::TooLong) => {
                return write_err(
                    &mut writer,
                    ErrorCode::TooLong,
                    format!("request line exceeds {} bytes", protocol::MAX_REQUEST_LINE),
                );
            }
            Ok(ReadOutcome::BadFraming) => {
                return write_err(
                    &mut writer,
                    ErrorCode::BadFraming,
                    "request line is not UTF-8",
                );
            }
            // The client half-closed or died, or the idle timeout expired: the
            // connection ends and nothing is written (2.3 rule 4).
            Ok(ReadOutcome::Eof) => return Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        }
    }
}

enum ReadOutcome {
    Line(String),
    Eof,
    TooLong,
    BadFraming,
}

/// One request line, bounded at `MAX_REQUEST_LINE` bytes excluding the newline
/// (2.2). A `\r` before the newline is tolerated, never required.
fn read_request_line(reader: &mut impl BufRead) -> io::Result<ReadOutcome> {
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(ReadOutcome::Eof);
        }
        match available.iter().position(|byte| *byte == b'\n') {
            Some(index) => {
                buffer.extend_from_slice(&available[..index]);
                reader.consume(index + 1);
                break;
            }
            None => {
                let length = available.len();
                buffer.extend_from_slice(available);
                reader.consume(length);
                if buffer.len() > protocol::MAX_REQUEST_LINE {
                    return Ok(ReadOutcome::TooLong);
                }
            }
        }
    }
    if buffer.len() > protocol::MAX_REQUEST_LINE {
        return Ok(ReadOutcome::TooLong);
    }
    if buffer.last() == Some(&b'\r') {
        buffer.pop();
    }
    match std::str::from_utf8(&buffer) {
        Ok(text) => Ok(ReadOutcome::Line(text.to_string())),
        Err(_) => Ok(ReadOutcome::BadFraming),
    }
}

#[derive(PartialEq, Eq)]
enum Control {
    Continue,
    Close,
}

/// One request, one response. `out` is written as the answer is produced, so
/// `queued` can reach the client before the daemon blocks on the worker (2.6).
fn dispatch(daemon: &Daemon, line: &str, out: &mut impl Write) -> io::Result<Control> {
    let request = match Request::parse(line) {
        Ok(request) => request,
        Err(error) => {
            write_err(out, error.code, &error.message)?;
            // A framing or protocol failure is the last line on this connection.
            return Ok(if error.code.closes() {
                Control::Close
            } else {
                Control::Continue
            });
        }
    };

    match request {
        Request::Ping => write_ok(out)?,
        Request::Hello { version, .. } => {
            // One version exists, so anything else is a client guessing at a
            // grammar nobody implemented: refuse and close (2.4).
            if version != protocol::PROTOCOL_VERSION {
                write_err(
                    out,
                    ErrorCode::BadProtocol,
                    format!(
                        "server speaks {}, client asked for {version}",
                        protocol::PROTOCOL_VERSION
                    ),
                )?;
                return Ok(Control::Close);
            }
            // `protocol: <version>` then `OK` (2.4), which the response shape
            // already produces: the data line, then the terminator.
            write_response(
                out,
                &Response::ok().kv("protocol", protocol::PROTOCOL_VERSION),
            )?;
        }
        Request::Version => {
            write_response(
                out,
                &Response::ok()
                    .kv(
                        "daemon_version",
                        format!("{} {}", protocol::PRODUCT, protocol::VERSION),
                    )
                    .kv("protocol", protocol::PROTOCOL_VERSION)
                    .kv("platform", platform()),
            )?;
        }
        Request::Status => write_response(out, &daemon.status())?,
        Request::Sources => {
            let records = daemon.effective.source_records();
            let mut response = Response::ok().kv("count", records.len());
            for record in records {
                response = response.line(record.line());
            }
            write_response(out, &response)?;
        }
        Request::ConfigPath => {
            write_response(
                out,
                &Response::ok().kv("config", daemon.effective.config_path.display()),
            )?;
        }
        Request::History { count } => {
            let entries: Vec<String> = {
                let state = daemon.state();
                state
                    .history
                    .iter()
                    .take(count)
                    .map(HistoryEntry::record_line)
                    .collect()
            };
            let mut response = Response::ok().kv("count", entries.len());
            for entry in entries {
                response = response.line(entry);
            }
            write_response(out, &response)?;
        }
        Request::Favorites => {
            let entries: Vec<String> = {
                let state = daemon.state();
                state
                    .favorites
                    .values()
                    .map(Favorite::record_line)
                    .collect()
            };
            let mut response = Response::ok().kv("count", entries.len());
            for entry in entries {
                response = response.line(entry);
            }
            write_response(out, &response)?;
        }
        Request::Favorite { id } => {
            let resolved = match &id {
                Some(id) => daemon.resolve_id(id),
                None => daemon.state().current.as_ref().map(|current| Resolved {
                    origin_key: current.origin_key.clone(),
                    digest: Some(current.digest.clone()),
                    kind: current.kind,
                    path: current.path.clone(),
                }),
            };
            match resolved {
                None => {
                    write_err(
                        out,
                        ErrorCode::NotFound,
                        "favorite: there is no current entry and no id resolves",
                    )?;
                }
                Some(resolved) => {
                    let response;
                    {
                        let mut state = daemon.state();
                        let already = state.favorites.contains_key(&resolved.origin_key);
                        if !already {
                            let bytes = favorite_state(resolved.path.as_deref());
                            state.favorites.insert(
                                resolved.origin_key.clone(),
                                Favorite {
                                    added_at: now(),
                                    kind: resolved.kind,
                                    origin_key: resolved.origin_key.clone(),
                                    digest: resolved.digest.clone(),
                                    state: bytes,
                                    path: resolved.path.clone(),
                                },
                            );
                        }
                        state.seq += 1;
                        let digest = resolved.digest.clone().unwrap_or_else(|| "-".to_string());
                        response = Response::ok()
                            .line(format!("favorited: {digest} {}", resolved.origin_key))
                            .kv("already", u8::from(already));
                    }
                    write_response(out, &response)?;
                }
            }
        }
        Request::Unfavorite(id) => {
            let response;
            {
                let mut state = daemon.state();
                let removed = state
                    .resolve(&id)
                    .and_then(|resolved| state.favorites.remove(&resolved.origin_key));
                match removed {
                    None => {
                        drop(state);
                        write_err(
                            out,
                            ErrorCode::NotFound,
                            format!("{id} is not an origin_key, not a digest and not a favorite"),
                        )?;
                        return Ok(Control::Continue);
                    }
                    Some(favorite) => {
                        state.seq += 1;
                        let digest = favorite.digest.unwrap_or_else(|| "-".to_string());
                        response = Response::ok().line(format!("unfavorited: {digest}"));
                    }
                }
            }
            write_response(out, &response)?;
        }
        Request::Next => {
            run_rotation(daemon, out, Via::Source, Verb::Rotate, None)?;
        }
        Request::Prev => {
            let previous = {
                let state = daemon.state();
                state
                    .history
                    .prev_from(
                        state
                            .current
                            .as_ref()
                            .map(|current| current.digest.as_str()),
                    )
                    .cloned()
            };
            match previous {
                None => write_err(out, ErrorCode::NoPrev, "no earlier entry in history")?,
                Some(entry) => {
                    let target = entry
                        .path
                        .clone()
                        .unwrap_or_else(|| entry.origin_key.clone());
                    run_rotation(daemon, out, Via::Prev, Verb::Set, Some(&target))?;
                }
            }
        }
        Request::SetPath(path) => {
            if !Path::new(&path).is_absolute() {
                write_err(
                    out,
                    ErrorCode::BadArgs,
                    format!("set path needs an absolute path, got {path:?}"),
                )?;
            } else if !Path::new(&path).exists() {
                write_err(out, ErrorCode::NotFound, format!("{path} does not exist"))?;
            } else {
                run_rotation(daemon, out, Via::Manual, Verb::Set, Some(&path))?;
            }
        }
        Request::SetId(id) => match daemon.resolve_id(&id) {
            None => {
                write_err(
                    out,
                    ErrorCode::NotFound,
                    format!("{id} is not an origin_key, not a digest and not a favorite"),
                )?;
            }
            Some(resolved) => {
                run_rotation(
                    daemon,
                    out,
                    Via::Manual,
                    Verb::Set,
                    Some(&resolved.origin_key),
                )?;
            }
        },
        Request::Pause => {
            daemon.set_paused(true);
            write_ok(out)?;
        }
        Request::Resume => {
            daemon.set_paused(false);
            write_ok(out)?;
        }
        Request::ConfigCheck => {
            // A check is not a rotation: it takes a slot for `--run` and its own
            // deadline, and it leaves `rotating` at 0 (2.5).
            let run = daemon.start_slot();
            writeln!(out, "queued")?;
            out.flush()?;
            let deadline = worker_deadline(daemon);
            match daemon.worker.run(Verb::Check, None, run, deadline) {
                Ok(Outcome::Lines(lines)) => {
                    for line in lines {
                        writeln!(out, "{line}")?;
                    }
                    write_ok(out)?;
                }
                Ok(Outcome::Set(_)) => {
                    write_err(
                        out,
                        ErrorCode::WorkerFailed,
                        "the worker answered a check with a set: line",
                    )?;
                }
                Err(WorkerError::Timeout) => {
                    write_err(out, ErrorCode::Timeout, "the worker did not finish in time")?;
                }
                Err(WorkerError::Failed { code, message }) => {
                    write_err(out, code, &message)?;
                }
            }
        }
        Request::Subscribe { .. } => {
            // 2.9 is a later card: this build answers commands only, and says so
            // rather than holding a connection open with nothing to send.
            write_err(
                out,
                ErrorCode::Internal,
                "subscribe is not implemented in this scaffold (docs/architecture.md 2.9)",
            )?;
        }
        Request::Close => {
            write_ok(out)?;
            return Ok(Control::Close);
        }
    }
    Ok(Control::Continue)
}

fn worker_deadline(daemon: &Daemon) -> Duration {
    Duration::from_secs(daemon.effective.config.schedule.worker_deadline_seconds)
}

/// `queued`, then the worker, then `set:` and `OK`, or the failure (2.5, 2.6).
fn run_rotation(
    daemon: &Daemon,
    out: &mut impl Write,
    via: Via,
    verb: Verb,
    target: Option<&str>,
) -> io::Result<()> {
    let run = match daemon.start_rotation() {
        Ok(run) => run,
        Err(running) => {
            return write_err(
                out,
                ErrorCode::Busy,
                format!("a rotation is in flight (run {running})"),
            );
        }
    };
    // The interim line goes out before the daemon blocks, which is the whole
    // point of it (2.6), and the lock is not held while the worker runs (1.8).
    writeln!(out, "queued")?;
    out.flush()?;
    let deadline = worker_deadline(daemon);
    let outcome = daemon.worker.run(verb, target, run, deadline);
    match outcome {
        Ok(Outcome::Set(record)) => {
            daemon.record_success(
                &record.digest,
                &record.origin_key,
                via,
                record.path.as_deref(),
            );
            write_response(
                out,
                &Response::ok().line(protocol::set_record(
                    &record.digest,
                    &record.origin_key,
                    via,
                    record.path.as_deref(),
                )),
            )
        }
        Ok(Outcome::Lines(lines)) => {
            for line in lines {
                writeln!(out, "{line}")?;
            }
            write_ok(out)
        }
        Err(WorkerError::Timeout) => {
            daemon.record_failure(ErrorCode::Timeout);
            write_err(
                out,
                ErrorCode::Timeout,
                "the worker did not finish inside schedule.worker_deadline_seconds",
            )
        }
        Err(WorkerError::Failed { code, message }) => {
            daemon.record_failure(code);
            write_err(out, code, &message)
        }
    }
}

fn write_response(out: &mut impl Write, response: &Response) -> io::Result<()> {
    out.write_all(response.encode().as_bytes())?;
    out.flush()
}

fn write_ok(out: &mut impl Write) -> io::Result<()> {
    out.write_all(b"OK\n")?;
    out.flush()
}

fn write_err(
    out: &mut impl Write,
    code: ErrorCode,
    message: impl std::fmt::Display,
) -> io::Result<()> {
    write_response(out, &Response::err(code, message))
}
