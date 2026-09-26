//! The control socket: bind it `0600` under a `0077` umask, accept, and answer
//! the verbs (docs/architecture.md 2.1, 2.3).
//!
//! One thread per connection, bounded at 16 (1.8, `[L 4]`). The accept loop
//! never runs a worker: a rotation happens on the connection that asked for it,
//! which is what keeps `status` answering in one round trip while a worker runs.
//! Unix only in this scaffold: the Windows named pipe of 2.1 is a later card,
//! and `main` refuses to start there rather than pretending.

use crate::lock;
use crate::state::{Daemon, Rotation, platform};
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

/// A subscribed connection polls its socket for a request line every 100 ms so
/// it can also drain its event queue. This is not the idle timeout: 2.8 measures
/// idle in "no traffic at all", and a subscribed connection always has traffic,
/// so a poll that finds nothing is a loop iteration and not a disconnect.
pub const STREAM_POLL: Duration = Duration::from_millis(100);

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
    let mut writer = stream.try_clone()?;
    writeln!(writer, "{}", protocol::greeting())?;
    writer.flush()?;

    loop {
        match read_request_line(&mut reader) {
            Ok(ReadOutcome::Line(line)) => match dispatch(daemon, &line, &mut writer)? {
                Control::Close => return Ok(()),
                // 2.9: the subscription takes over the connection. When it ends
                // the connection ends with it, because everything a client sent
                // during it was answered inside the stream.
                Control::Subscribe { since } => {
                    return subscribe(daemon, &stream, &mut reader, &mut writer, since);
                }
                Control::Continue => {}
            },
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

/// The stream of docs/architecture.md 2.9: `subscribed:`, an optional `gap:`,
/// then one `event:` line per state change and one heartbeat per 30 s of quiet.
///
/// The daemon keeps no event history, so the only thing a `since` can produce is
/// the size of the gap. Everything the client sends during the stream is
/// answered `ERR bad_args subscribe takes over this connection` and the stream
/// continues, which is what lets a client keep exactly one connection open and
/// still see its own errors.
fn subscribe(
    daemon: &Daemon,
    stream: &UnixStream,
    reader: &mut BufReader<UnixStream>,
    out: &mut UnixStream,
    since: Option<u64>,
) -> io::Result<()> {
    // Subscribing and reading the sequence number happen under one lock, so a
    // state change can never land between the number the client is told and the
    // moment its queue exists (Daemon::subscribe).
    let (seq, events) = daemon.subscribe();
    writeln!(out, "subscribed: {seq}")?;
    if let Some(since) = since {
        if since < seq {
            writeln!(out, "gap: {}", seq - since)?;
        }
    }
    out.flush()?;
    stream.set_read_timeout(Some(STREAM_POLL))?;

    loop {
        let mut sent = false;
        loop {
            match events.try_recv() {
                Ok(line) => {
                    out.write_all(line.as_bytes())?;
                    sent = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // The bus dropped this subscriber: its queue filled because the
                // client stopped reading. The stream ends; the client notices
                // and reconnects, which re-reads `status` (2.9).
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if sent {
            out.flush()?;
        }
        match read_request_line(reader) {
            // `close` is the one request that is honoured: it ends the stream
            // and the connection (2.9).
            Ok(ReadOutcome::Line(line)) if matches!(Request::parse(&line), Ok(Request::Close)) => {
                return write_ok(out);
            }
            Ok(ReadOutcome::Line(_)) => {
                write_err(
                    out,
                    ErrorCode::BadArgs,
                    "subscribe takes over this connection",
                )?;
            }
            Ok(ReadOutcome::Eof) => return Ok(()),
            // A line that is too long or not UTF-8 leaves the connection out of
            // step with the framing, so the stream ends exactly as it does on a
            // command connection.
            Ok(ReadOutcome::TooLong) => {
                return write_err(
                    out,
                    ErrorCode::TooLong,
                    format!("request line exceeds {} bytes", protocol::MAX_REQUEST_LINE),
                );
            }
            Ok(ReadOutcome::BadFraming) => {
                return write_err(out, ErrorCode::BadFraming, "request line is not UTF-8");
            }
            // No request line this poll: not an idle timeout, because a
            // subscribed connection is never idle (2.8).
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error),
        }
        // 30 s of quiet anywhere in the daemon produces one heartbeat for every
        // subscriber, and a heartbeat consumes a seq like any other event. The
        // claim is exclusive, so two subscribers produce one heartbeat.
        if daemon.bus.claim_heartbeat() {
            daemon.heartbeat();
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
///
/// An interrupted read is retried, not reported. Every connection here has
/// `SO_RCVTIMEO` set (2.8's idle timeout, 2.9's `STREAM_POLL`), and signal(7)
/// puts a socket read that has a timeout in the class that is *never* restarted
/// after a signal handler returns: it fails with `EINTR` instead. `EINTR` means
/// nothing was transferred, so the line has not started arriving and the read is
/// simply repeated. That is not hypothetical for this daemon: under
/// `linux/amd64` emulation the signal that reaches this thread when
/// `Worker::run` forks `whirl-worker` closed a subscriber's connection, which is
/// what `control_socket`'s `subscribe_streams_one_event_per_state_change` and
/// `a_failed_rotation_is_visible_on_both_planes` saw as "the daemon closed the
/// connection early" (t_62920980).
fn read_request_line(reader: &mut impl BufRead) -> io::Result<ReadOutcome> {
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
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

/// What the connection does after one request. The third case is 2.9's mode:
/// `subscribe` is not an answer, it is a handover.
#[derive(PartialEq, Eq)]
enum Control {
    Continue,
    Close,
    Subscribe { since: Option<u64> },
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
            // 6.4 step 3: a degraded favorites file is not an empty pin set.
            // Writing a pin over one the daemon could not read would lose it.
            if let Some(path) = daemon.favorites_degraded_message() {
                write_err(out, ErrorCode::FavoritesDegraded, path)?;
                return Ok(Control::Continue);
            }
            let resolved = match &id {
                Some(id) => daemon.resolve_id(id),
                // `favorite` with no argument pins what is on screen (2.5).
                None => daemon.state().current_resolved(),
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
                    let already = !daemon.add_favorite(&resolved);
                    let digest = resolved.digest.clone().unwrap_or_else(|| "-".to_string());
                    let response = Response::ok()
                        .line(format!("favorited: {digest} {}", resolved.origin_key))
                        .kv("already", u8::from(already));
                    write_response(out, &response)?;
                }
            }
        }
        Request::Unfavorite(id) => {
            if let Some(path) = daemon.favorites_degraded_message() {
                write_err(out, ErrorCode::FavoritesDegraded, path)?;
                return Ok(Control::Continue);
            }
            match daemon.remove_favorite(&id) {
                None => {
                    write_err(
                        out,
                        ErrorCode::NotFound,
                        format!("{id} is not an origin_key, not a digest and not a favorite"),
                    )?;
                }
                Some(digest) => {
                    write_response(out, &Response::ok().line(format!("unfavorited: {digest}")))?;
                }
            }
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
            let deadline = daemon.worker_deadline();
            match daemon.worker.run(Verb::Check, None, run, deadline) {
                Ok(Outcome::Lines(lines)) => {
                    // 8.7 and 4.2's `cache.root` comment: the check refuses a
                    // root whose filesystem cannot `flock`, and reports
                    // `lock_mode: excl_file` when the weaker lock had to be used.
                    // The probe runs after the worker, so this config has already
                    // passed the value and ordering rules the worker enforces;
                    // its lines are dropped on a refusal, because 2.5's refusals
                    // are one `ERR` and a half-answer before one would be a body
                    // no rule describes.
                    if let Err(message) =
                        lock::cache_root_accepts_flock(&daemon.effective.cache_dir)
                    {
                        write_err(out, ErrorCode::BadConfig, message)?;
                    } else {
                        for line in lines {
                            writeln!(out, "{line}")?;
                        }
                        // Only under 8.7's fallback: 2.5's `config check` body is
                        // otherwise exhaustive, so this line is absent under
                        // `flock` rather than always present.
                        if let Some(line) = daemon.lock_line() {
                            writeln!(out, "{line}")?;
                        }
                        write_ok(out)?;
                    }
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
        Request::Subscribe { since } => {
            // 2.9: this is a handover, not an answer. `handle` writes the stream
            // and drains it until the client closes or the bus drops it.
            return Ok(Control::Subscribe { since });
        }
        Request::Close => {
            write_ok(out)?;
            return Ok(Control::Close);
        }
    }
    Ok(Control::Continue)
}

/// `queued`, then the worker, then `set:` and `OK`, or the failure (2.5, 2.6).
///
/// The rotation itself (spawn, gate, record, and the events of 2.9) is
/// `Daemon::rotation`, which is also what the scheduler calls for a due slot:
/// one implementation, so a scheduled rotation and a client's `next` cannot
/// answer differently. What is here is only the part a client sees.
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
    match daemon.rotation(run, via, verb, target) {
        Rotation::Set(record) => write_response(
            out,
            &Response::ok().line(protocol::set_record(
                &record.digest,
                &record.origin_key,
                via,
                record.path.as_deref(),
            )),
        ),
        Rotation::Failed { code, message } => write_err(out, code, &message),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader whose first `read` is interrupted, as a socket read with
    /// `SO_RCVTIMEO` is when a signal reaches the thread (t_62920980), and which
    /// then serves one line.
    struct InterruptedOnce {
        line: &'static [u8],
        interrupted: bool,
    }

    impl io::Read for InterruptedOnce {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            let remaining: &'static [u8] = self.line;
            let length = remaining.len().min(out.len());
            out[..length].copy_from_slice(&remaining[..length]);
            self.line = &remaining[length..];
            Ok(length)
        }
    }

    /// The retry itself: `EINTR` must not end the connection, and the request
    /// line behind the interruption must still be read. Before this, the error
    /// propagated out of `handle`, which dropped the connection: the client saw
    /// EOF where its next event should have been.
    #[test]
    fn an_interrupted_read_retries_instead_of_ending_the_connection() {
        let reader = InterruptedOnce {
            line: b"pause\n",
            interrupted: false,
        };
        let mut reader = BufReader::new(reader);
        match read_request_line(&mut reader).expect("EINTR is retried") {
            ReadOutcome::Line(line) => assert_eq!(line, "pause"),
            _ => panic!("the line behind the interruption is the request"),
        }
    }

    /// The other half: a read error that is not an interruption is still
    /// reported, so the retry above cannot swallow a real failure.
    #[test]
    fn a_read_error_that_is_not_an_interruption_is_still_reported() {
        struct Failing;
        impl io::Read for Failing {
            fn read(&mut self, _out: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::ConnectionReset))
            }
        }
        let error = match read_request_line(&mut BufReader::new(Failing)) {
            Err(error) => error,
            Ok(_) => panic!("a reset is not an interruption"),
        };
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
    }
}
