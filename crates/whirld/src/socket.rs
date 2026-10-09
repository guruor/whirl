//! The control socket: bind it `0600` in one step under a `0177` umask, accept,
//! and answer the verbs (docs/architecture.md 2.1, 2.3).
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

/// The umask the `bind` runs under: `0777 & ~0o177` is exactly `0o600`, so the
/// socket file is created at the mode 2.1 asks for and there is no instant at
/// which the path is anything else.
///
/// `0o077` is the mask that looks right and is not: `0777 & ~0o077` is `0700`,
/// which closes the window `[L 3]` measured (group and other cannot connect)
/// but still leaves the file at `0700` until the `fchmod` below lands, and a
/// bound socket accepts a connection the moment `bind` returns. A client that
/// connected in that window read `0700`: `control_socket`'s
/// `the_socket_is_0600_in_a_0700_directory` saw `448` where it asserts `384`,
/// once in eleven whole-workspace runs (cbefc4c).
const SOCKET_UMASK: u32 = 0o177;

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

/// Run `body` with `SOCKET_UMASK` in force, then put back the mask the process
/// had. The daemon is single-threaded at that point -- `main` spawns the
/// scheduler only after `bind` has returned -- which is what makes a
/// process-wide mask safe to borrow here: `umask` is not thread-local. Nothing
/// in a test binary may call it: `cargo test` runs a binary's unit tests on
/// parallel threads, so the mask would be in force while the state, statefile
/// and cache tests create their scratch files, and those fail with
/// `PermissionDenied` (cbefc4c).
fn with_socket_umask<T>(body: impl FnOnce() -> T) -> T {
    let previous = set_umask(SOCKET_UMASK);
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
    let listener = with_socket_umask(|| UnixListener::bind(path))
        .map_err(|error| format!("cannot bind {}: {error}", path.display()))?;
    // The socket is `0600` already: `SOCKET_UMASK` has the bind create it that
    // way. This call stays as the enforcement that does not rest on the umask --
    // it is what keeps 2.1's mode true even where `bind` ignored the mask -- and
    // after the mask has done its job it is a no-op, so it opens no window of
    // its own.
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
/// connection early" (580dffb).
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
                Ok(Outcome::Set { .. }) => {
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
    /// `SO_RCVTIMEO` is when a signal reaches the thread (580dffb), and which
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

    // -----------------------------------------------------------------------
    // The connection itself (docs/architecture.md 2.2, 2.3, 2.4, 2.9)
    //
    // `handle`, `dispatch` and `subscribe` are private to this module, so a unit
    // test can drive the daemon's real connection code without a listener: a
    // `UnixStream` pair is one connected socket, and every framing decision,
    // timeout and answer below is the daemon's own. The client half carries a
    // read timeout so a missing line fails the test instead of hanging it.
    // -----------------------------------------------------------------------

    use crate::testkit::{Scratch, daemon, daemon_with_a_set, digest};

    const ORIGIN_KEY: &str = "pictures:one";

    /// One in-process connection: the client half, and the thread running the
    /// daemon's `handle` on the other half of the same socket.
    struct Peer {
        client: UnixStream,
        reader: BufReader<UnixStream>,
        handler: Option<std::thread::JoinHandle<io::Result<()>>>,
    }

    impl Peer {
        fn connect(daemon: &Arc<Daemon>) -> Peer {
            let (client, server) = UnixStream::pair().expect("a socket pair");
            client
                .set_read_timeout(Some(Duration::from_secs(30)))
                .expect("a read timeout");
            let daemon = Arc::clone(daemon);
            let handler = std::thread::spawn(move || handle(server, &daemon));
            let mut reader = BufReader::new(client.try_clone().expect("a client clone"));
            let greeting = read_line(&mut reader);
            assert!(
                greeting.starts_with("OK whirl ") && greeting.ends_with(" protocol 2"),
                "2.4's greeting: {greeting:?}"
            );
            Peer {
                client,
                reader,
                handler: Some(handler),
            }
        }

        fn send(&mut self, line: &str) {
            self.send_raw(format!("{line}\n").as_bytes());
        }

        fn send_raw(&mut self, bytes: &[u8]) {
            self.client.write_all(bytes).expect("a write");
            self.client.flush().expect("a flush");
        }

        fn line(&mut self) -> String {
            read_line(&mut self.reader)
        }

        /// Every line up to and including the terminator of 2.2.
        fn answer(&mut self) -> Vec<String> {
            let mut lines = Vec::new();
            loop {
                let line = self.line();
                let last = line == "OK" || line.starts_with("ERR ");
                lines.push(line);
                if last {
                    return lines;
                }
            }
        }

        /// The single line of a response that cannot be more than one: `OK`, or
        /// one `ERR`.
        fn one(&mut self) -> String {
            let lines = self.answer();
            assert_eq!(lines.len(), 1, "{lines:?}");
            lines.into_iter().next().expect("a line")
        }

        /// Drop the client half and join the handler. The join is the assertion
        /// that no thread is left behind: every test ends here.
        fn close(mut self) -> io::Result<()> {
            drop(self.reader);
            drop(self.client);
            self.handler
                .take()
                .expect("a handler")
                .join()
                .expect("the handler did not panic")
        }
    }

    fn read_line(reader: &mut BufReader<UnixStream>) -> String {
        let mut line = String::new();
        reader.read_line(&mut line).expect("a line");
        line.strip_suffix('\n').unwrap_or(&line).to_string()
    }

    /// A daemon whose worker reports a successful `set:` of a fixed digest.
    fn set_daemon(scratch: &Scratch) -> (Arc<Daemon>, String) {
        let digest = digest('a');
        let daemon = daemon_with_a_set(
            scratch.path(),
            whirl_core::config::Config::default(),
            &digest,
            ORIGIN_KEY,
            "/cache/sha256/aa/aa/aa.jpg",
        );
        (Arc::new(daemon), digest)
    }

    /// The verbs that read and answer without a worker (2.5), over one
    /// connection: the framing of each answer, and that a connection is reused
    /// rather than reopened.
    #[test]
    fn the_read_only_verbs_answer_and_the_connection_is_reused() {
        let scratch = Scratch::new("socket-read-only");
        let (daemon, _) = set_daemon(&scratch);
        let mut peer = Peer::connect(&daemon);

        peer.send("hello 2");
        assert_eq!(
            peer.answer(),
            vec!["protocol: 2".to_string(), "OK".to_string()]
        );

        peer.send("ping");
        assert_eq!(peer.one(), "OK");

        peer.send("version");
        let version = peer.answer();
        assert!(
            version[0].starts_with("daemon_version: whirl "),
            "{version:?}"
        );
        assert_eq!(version[1], "protocol: 2");
        assert_eq!(version.last().map(String::as_str), Some("OK"));

        peer.send("status");
        let status = peer.answer();
        assert!(
            status[0].starts_with("daemon_version: "),
            "2.10's first row: {status:?}"
        );
        assert!(status.iter().any(|line| line == "paused: 0"), "{status:?}");
        assert!(
            status.iter().any(|line| line == "history_count: 0"),
            "{status:?}"
        );
        assert_eq!(status.last().map(String::as_str), Some("OK"));

        peer.send("sources");
        let sources = peer.answer();
        assert!(sources[0].starts_with("count: "), "{sources:?}");
        assert_eq!(sources.last().map(String::as_str), Some("OK"));

        peer.send("config path");
        let config = peer.answer();
        assert!(config[0].starts_with("config: "), "{config:?}");

        peer.send("history");
        assert_eq!(
            peer.answer(),
            vec!["count: 0".to_string(), "OK".to_string()]
        );

        peer.send("favorites");
        assert_eq!(
            peer.answer(),
            vec!["count: 0".to_string(), "OK".to_string()]
        );

        // `favorite` with no argument pins what is on screen, and there is
        // nothing on screen yet: 2.5's `not_found`.
        peer.send("favorite");
        assert!(peer.one().starts_with("ERR not_found "));

        peer.send("unfavorite deadbeef");
        assert!(peer.one().starts_with("ERR not_found "));

        // No history, so 2.7's `no_prev` rather than an empty `prev`.
        peer.send("prev");
        assert!(peer.one().starts_with("ERR no_prev "));

        peer.send("close");
        assert_eq!(peer.one(), "OK");
        assert_eq!(peer.close().expect("the handler ended"), ());
    }

    /// The verbs that spawn a worker (2.5's `next`, the two `set` forms), and
    /// the interim `queued` line that reaches the client before the daemon
    /// blocks (2.6).
    #[test]
    fn the_rotation_verbs_report_the_set_the_worker_made() {
        let scratch = Scratch::new("socket-rotate");
        let (daemon, digest) = set_daemon(&scratch);
        let mut peer = Peer::connect(&daemon);

        peer.send("next");
        let next = peer.answer();
        assert_eq!(next[0], "queued");
        assert_eq!(
            next[1],
            format!("set: {digest} {ORIGIN_KEY} source /cache/sha256/aa/aa/aa.jpg")
        );
        assert_eq!(next.last().map(String::as_str), Some("OK"));

        // The state change is visible on the next request, on the same
        // connection: the worker's set became the anchor and a history entry.
        peer.send("status");
        let status = peer.answer();
        assert!(
            status
                .iter()
                .any(|line| line == &format!("last_digest: {digest}")),
            "{status:?}"
        );
        assert!(
            status.iter().any(|line| line == "last_via: source"),
            "{status:?}"
        );
        assert!(
            status.iter().any(|line| line == "history_count: 1"),
            "{status:?}"
        );

        peer.send("history 1");
        let history = peer.answer();
        assert_eq!(history[0], "count: 1");
        assert!(history[1].starts_with("entry: "), "{history:?}");

        // The pin verbs, against the anchor that is now on screen.
        peer.send("favorite");
        let favorited = peer.answer();
        assert_eq!(favorited[0], format!("favorited: {digest} {ORIGIN_KEY}"));
        assert_eq!(favorited[1], "already: 0");
        assert_eq!(favorited.last().map(String::as_str), Some("OK"));

        peer.send("favorite");
        let again = peer.answer();
        assert_eq!(again[1], "already: 1", "6.4's `already` flag");

        peer.send("favorites");
        assert_eq!(peer.answer()[0], "count: 1");

        peer.send("unfavorite pictures:one");
        let unfavorited = peer.answer();
        assert_eq!(unfavorited[0], format!("unfavorited: {digest}"));

        peer.send("favorites");
        assert_eq!(peer.answer()[0], "count: 0");

        // `set id` resolves an `origin_key` through history (2.5's order).
        peer.send("set id pictures:one");
        let set_id = peer.answer();
        assert_eq!(set_id[0], "queued");
        assert!(set_id[1].starts_with("set: "), "{set_id:?}");

        // `set path` refuses a path that is not absolute, and one that does not
        // exist, before any worker is spawned.
        peer.send("set path relative/file.jpg");
        assert!(peer.one().starts_with("ERR bad_args "));
        peer.send("set path /no/such/file/whirl.jpg");
        assert!(peer.one().starts_with("ERR not_found "));

        peer.send("close");
        assert_eq!(peer.one(), "OK");
        assert_eq!(peer.close().expect("the handler ended"), ());
    }

    /// `prev` sets an earlier entry again (2.5): the second rotation uses a
    /// different digest, so history has two entries and `prev` has somewhere to
    /// go. What the daemon chose is visible in the worker's arguments -- 2.5 has
    /// `prev` hand the worker the earlier entry's path -- and the `via` of the
    /// record is `prev`, which is what a client branches on.
    ///
    /// The reported path is under `cache/sha256/`, which is what makes it a path
    /// the entry keeps: 6.1 blanks the path of a `reference`-mode local source,
    /// and a blanked one falls back to the `origin_key` as the target.
    #[test]
    fn prev_sets_the_earlier_entry_again() {
        let scratch = Scratch::new("socket-prev");
        let first = digest('b');
        let second = digest('c');
        let cached = scratch.join("cache/sha256/aa/aa/aa.jpg");
        let cached = cached.display().to_string();
        let program = crate::testkit::script(
            scratch.path(),
            "worker-set.sh",
            &format!("#!/bin/sh\nprintf '%s\\n' 'set: {first} {ORIGIN_KEY} {cached}'\n"),
        );
        let daemon = Arc::new(crate::testkit::daemon(
            scratch.path(),
            whirl_core::config::Config::default(),
            program,
        ));
        let mut peer = Peer::connect(&daemon);

        peer.send("next");
        assert_eq!(
            peer.answer()[1],
            format!("set: {first} {ORIGIN_KEY} source {cached}")
        );

        // Give the daemon a different worker for the rest of the test: it reports
        // its own digest and writes down the arguments the daemon handed it, so
        // the entry `prev` chose is readable from outside the daemon.
        let argv = scratch.join("worker-argv.txt");
        crate::testkit::script(
            scratch.path(),
            "worker-set.sh",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\nprintf '%s\\n' 'set: {second} {ORIGIN_KEY} /cache/second.jpg'\n",
                argv.display()
            ),
        );
        peer.send("next");
        assert!(peer.answer()[1].starts_with(&format!("set: {second} ")));

        peer.send("prev");
        let previous = peer.answer();
        assert_eq!(previous[0], "queued");
        assert_eq!(
            previous[1],
            format!("set: {second} {ORIGIN_KEY} prev /cache/second.jpg"),
            "2.6's record carries the via the daemon decided, not the worker's"
        );
        assert_eq!(previous.last().map(String::as_str), Some("OK"));

        let arguments = std::fs::read_to_string(&argv).expect("the worker's arguments");
        assert!(
            arguments.contains(&format!("--target {cached}")),
            "2.5: `prev` hands the worker the earlier entry's path, not the current one's: {arguments:?}"
        );

        peer.send("close");
        assert_eq!(peer.one(), "OK");
        assert_eq!(peer.close().expect("the handler ended"), ());
    }

    /// `config check` is the only verb whose body is the worker's own lines (2.5),
    /// and `Verb::Check` is the only verb the worker answers that way: a rotation
    /// needs a `set:` line, so the same worker cannot serve both and the rotation
    /// that asks anyway is `worker_failed` (2.7) rather than a body no rule
    /// describes.
    #[test]
    fn config_check_prints_the_workers_lines_and_a_rotation_cannot() {
        let scratch = Scratch::new("socket-check");
        let program = crate::testkit::script(
            scratch.path(),
            "worker-check.sh",
            "#!/bin/sh\nprintf '%s\\n' 'source: pictures local weight=1 enabled=1 last=- candidates=3 reason=-'\nprintf '%s\\n' 'plan: schedule.interval_seconds=1800 backend=noop sources=1'\n",
        );
        let daemon = Arc::new(daemon(
            scratch.path(),
            whirl_core::config::Config::default(),
            program,
        ));
        let mut peer = Peer::connect(&daemon);

        peer.send("config check");
        let check = peer.answer();
        assert_eq!(check[0], "queued");
        assert_eq!(
            check[1],
            "source: pictures local weight=1 enabled=1 last=- candidates=3 reason=-"
        );
        assert_eq!(
            check[2],
            "plan: schedule.interval_seconds=1800 backend=noop sources=1"
        );
        assert_eq!(check.last().map(String::as_str), Some("OK"));

        peer.send("next");
        let refused = peer.answer();
        assert_eq!(refused[0], "queued");
        assert_eq!(
            refused[1],
            "ERR worker_failed the worker exited 0 without a set: line; last line was \
             \"plan: schedule.interval_seconds=1800 backend=noop sources=1\"",
            "a rotation with no set: line has no answer to give (2.7)"
        );

        peer.send("close");
        assert_eq!(peer.one(), "OK");
        assert_eq!(peer.close().expect("the handler ended"), ());
    }

    /// 2.3 rule 4: the client died mid-frame. A partial line is not a request
    /// and not an error: the connection ends and nothing is written.
    #[test]
    fn a_peer_that_disconnects_mid_frame_ends_the_connection_without_an_error() {
        let scratch = Scratch::new("socket-mid-frame");
        let (daemon, _) = set_daemon(&scratch);
        let mut peer = Peer::connect(&daemon);
        peer.send_raw(b"stat");

        // Nothing more arrives, and the socket is closed: `read_request_line`
        // sees EOF rather than a broken frame.
        peer.client
            .shutdown(std::net::Shutdown::Write)
            .expect("a half close");
        assert_eq!(peer.line(), "", "EOF, not an answer");
        assert_eq!(peer.close().expect("the handler ended"), ());
    }

    /// 2.2: a request line longer than `MAX_REQUEST_LINE` is `too_long`, and the
    /// error closes the connection.
    #[test]
    fn a_request_longer_than_the_frame_is_refused_and_closes() {
        let scratch = Scratch::new("socket-long");
        let (daemon, _) = set_daemon(&scratch);
        let mut peer = Peer::connect(&daemon);

        let mut overlong = vec![b'x'; protocol::MAX_REQUEST_LINE + 1];
        overlong.push(b'\n');
        peer.send_raw(&overlong);

        assert_eq!(
            peer.one(),
            format!(
                "ERR too_long request line exceeds {} bytes",
                protocol::MAX_REQUEST_LINE
            )
        );
        assert_eq!(peer.line(), "", "the error closed the connection");
        assert_eq!(peer.close().expect("the handler ended"), ());
    }

    /// 2.2: a request line that is not UTF-8 is `bad_framing`, and that closes
    /// the connection too, because the framing is out of step from there on.
    #[test]
    fn a_request_that_is_not_utf8_is_refused_as_bad_framing() {
        let scratch = Scratch::new("socket-utf8");
        let (daemon, _) = set_daemon(&scratch);
        let mut peer = Peer::connect(&daemon);

        peer.send_raw(&[0xff, 0xfe, b'\n']);

        assert_eq!(
            peer.one(),
            "ERR bad_framing request line is not UTF-8".to_string()
        );
        assert_eq!(peer.line(), "", "the error closed the connection");
        assert_eq!(peer.close().expect("the handler ended"), ());
    }

    /// 2.4: one protocol version exists, so a `hello` for another one is a
    /// client guessing at a grammar nobody implemented: refuse and close.
    #[test]
    fn a_hello_for_another_protocol_is_refused_and_closes() {
        let scratch = Scratch::new("socket-protocol");
        let (daemon, _) = set_daemon(&scratch);
        let mut peer = Peer::connect(&daemon);

        peer.send("hello 3");
        assert_eq!(
            peer.one(),
            format!(
                "ERR bad_protocol server speaks {}, client asked for 3",
                protocol::PROTOCOL_VERSION
            )
        );
        assert_eq!(peer.line(), "", "the refusal closed the connection");
        assert_eq!(peer.close().expect("the handler ended"), ());
    }

    /// 2.5 and 2.7: an unknown verb is one `ERR`, and the connection is *not*
    /// closed, because nothing about the framing is in doubt. The `ping` after it
    /// is the assertion that matters.
    #[test]
    fn a_request_the_protocol_does_not_know_is_refused_and_the_connection_survives() {
        let scratch = Scratch::new("socket-unknown");
        let (daemon, _) = set_daemon(&scratch);
        let mut peer = Peer::connect(&daemon);

        peer.send("frobnicate");
        assert_eq!(
            peer.one(),
            "ERR unknown_verb unknown command \"frobnicate\"".to_string()
        );
        peer.send("ping");
        assert_eq!(peer.one(), "OK", "a refusal is not a close (2.7)");

        peer.send("close");
        assert_eq!(peer.one(), "OK");
        assert_eq!(peer.close().expect("the handler ended"), ());
    }

    /// 2.3: two connections are two threads on one daemon. The second client's
    /// state change is the first client's event, and neither answer is the
    /// other's.
    #[test]
    fn two_clients_at_once_are_answered_independently() {
        let scratch = Scratch::new("socket-two");
        let (daemon, _) = set_daemon(&scratch);
        let mut subscriber = Peer::connect(&daemon);
        let mut client = Peer::connect(&daemon);

        subscriber.send("subscribe");
        let subscribed = subscriber.line();
        assert!(subscribed.starts_with("subscribed: "), "{subscribed:?}");
        let seq: u64 = subscribed
            .strip_prefix("subscribed: ")
            .expect("the prefix")
            .parse()
            .expect("a sequence number");

        client.send("pause");
        assert_eq!(client.one(), "OK");
        assert_eq!(
            subscriber.line(),
            format!("event: {} paused", seq + 1),
            "2.9: one state change, one event, one seq"
        );

        client.send("status");
        let status = client.answer();
        assert!(status.iter().any(|line| line == "paused: 1"), "{status:?}");
        assert!(status.iter().any(|line| line == "seq: 1"), "{status:?}");

        client.send("resume");
        assert_eq!(client.one(), "OK");
        assert_eq!(subscriber.line(), format!("event: {} resumed", seq + 2));

        subscriber.send("close");
        assert_eq!(subscriber.one(), "OK");
        assert_eq!(subscriber.close().expect("the handler ended"), ());

        client.send("close");
        assert_eq!(client.one(), "OK");
        assert_eq!(client.close().expect("the handler ended"), ());
    }

    /// 2.9: the daemon keeps no event history, so the only thing `since` can
    /// produce is the size of the gap.
    #[test]
    fn subscribing_after_the_stream_moved_reports_the_gap() {
        let scratch = Scratch::new("socket-gap");
        let (daemon, _) = set_daemon(&scratch);
        let mut client = Peer::connect(&daemon);
        for _ in 0..3 {
            client.send("pause");
            assert_eq!(client.one(), "OK");
            client.send("resume");
            assert_eq!(client.one(), "OK");
        }
        client.send("status");
        let seq: u64 = client
            .answer()
            .iter()
            .find_map(|line| line.strip_prefix("seq: ").map(str::to_string))
            .expect("the seq row")
            .parse()
            .expect("a number");
        assert_eq!(seq, 6, "six state changes, six events");

        let mut subscriber = Peer::connect(&daemon);
        subscriber.send("subscribe 1");
        let stream = [subscriber.line(), subscriber.line()];
        assert_eq!(stream[0], "subscribed: 6");
        assert_eq!(stream[1], "gap: 5", "6 - 1: the events this client missed");

        // A `since` at or ahead of the current seq is not a gap.
        subscriber.send("close");
        assert_eq!(subscriber.one(), "OK");
        assert_eq!(subscriber.close().expect("the handler ended"), ());

        client.send("close");
        assert_eq!(client.one(), "OK");
        assert_eq!(client.close().expect("the handler ended"), ());
    }

    /// 2.9: once `subscribe` has taken the connection, every request but `close`
    /// is refused with one `ERR` and the stream continues, which is what lets a
    /// client keep exactly one connection open and still see its own errors.
    #[test]
    fn a_subscribed_connection_refuses_another_request_but_honours_close() {
        let scratch = Scratch::new("socket-handover");
        let (daemon, _) = set_daemon(&scratch);
        let mut peer = Peer::connect(&daemon);

        peer.send("subscribe");
        assert!(peer.line().starts_with("subscribed: "));

        peer.send("status");
        assert_eq!(
            peer.line(),
            "ERR bad_args subscribe takes over this connection"
        );
        // The stream is still a stream: a state change reaches it.
        let mut client = Peer::connect(&daemon);
        client.send("pause");
        assert_eq!(client.one(), "OK");
        assert!(peer.line().starts_with("event: "));

        peer.send("close");
        assert_eq!(peer.line(), "OK");
        assert_eq!(peer.close().expect("the handler ended"), ());

        client.send("resume");
        assert_eq!(client.one(), "OK");
        client.send("close");
        assert_eq!(client.one(), "OK");
        assert_eq!(client.close().expect("the handler ended"), ());
    }

    /// 2.9 and R4: a subscriber that stops reading fills its bounded queue and is
    /// dropped from the bus — "the only failure a subscriber can cause, and it is
    /// confined to that subscriber". The daemon never blocks on it: the other
    /// client is answered throughout. When the subscriber reads again it drains
    /// what it was owed and the stream ends, which is the observable proof that
    /// the bus let go of it; a daemon that had queued without bound would still
    /// be streaming here.
    ///
    /// This is the one test that has to generate enough traffic to fill a socket
    /// buffer *and* the queue: the loop runs until the bus reports the drop, so
    /// no fixed number of events is baked in.
    #[test]
    fn a_subscriber_that_stops_reading_is_dropped_and_its_stream_ends() {
        let scratch = Scratch::new("socket-stuck");
        let (daemon, _) = set_daemon(&scratch);
        let mut stuck = Peer::connect(&daemon);
        let mut driver = Peer::connect(&daemon);

        stuck.send("subscribe");
        assert!(stuck.line().starts_with("subscribed: "));

        let mut events = 0u64;
        while daemon.bus.subscriber_count() > 0 {
            driver.send("pause");
            assert_eq!(driver.one(), "OK", "the daemon keeps answering");
            driver.send("resume");
            assert_eq!(driver.one(), "OK", "the daemon keeps answering");
            events += 2;
            assert!(
                events < 50_000,
                "the bus never dropped the stuck subscriber"
            );
        }
        assert_eq!(
            daemon.bus.subscriber_count(),
            0,
            "the queue filled and the subscriber was dropped (2.9)"
        );
        assert!(events > 256, "the queue is bounded at 256 lines");

        driver.send("status");
        let status = driver.answer();
        assert!(
            status.last().map(String::as_str) == Some("OK"),
            "{status:?}"
        );

        // Draining the stuck client now terminates: the bus is gone, so the
        // remaining lines end in EOF.
        let mut drained = 0u64;
        loop {
            let line = stuck.line();
            if line.is_empty() {
                break;
            }
            assert!(line.starts_with("event: "), "{line:?}");
            drained += 1;
        }
        assert!(
            drained >= 256,
            "the bounded queue is what a reader gets: {drained}"
        );
        assert_eq!(stuck.close().expect("the handler ended"), ());

        driver.send("close");
        assert_eq!(driver.one(), "OK");
        assert_eq!(driver.close().expect("the handler ended"), ());
    }

    /// 6.4 step 3 and 2.5: while `favorites.json` is degraded, pin-changing verbs
    /// are refused and the message names the quarantine, because writing a pin
    /// over a set the daemon could not read would lose it.
    #[test]
    fn a_degraded_pin_set_refuses_a_pin_and_names_the_quarantine() {
        let scratch = Scratch::new("socket-degraded");
        let state_dir = scratch.join("state");
        std::fs::create_dir_all(&state_dir).expect("a state directory");
        std::fs::write(state_dir.join("favorites.json"), "not json at all")
            .expect("a corrupt pin file");
        let daemon = Arc::new(daemon_with_a_set(
            scratch.path(),
            whirl_core::config::Config::default(),
            &digest('d'),
            ORIGIN_KEY,
            "/cache/one.jpg",
        ));
        assert!(
            daemon.state().favorites_degraded,
            "a corrupt pin file is not an empty pin set"
        );
        let mut peer = Peer::connect(&daemon);

        peer.send("favorite");
        let refused = peer.one();
        assert!(
            refused.starts_with("ERR favorites_degraded "),
            "{refused:?}"
        );
        assert!(
            refused.contains("favorites.json.corrupt-"),
            "the message names the quarantine: {refused:?}"
        );

        peer.send("unfavorite pictures:one");
        assert!(peer.one().starts_with("ERR favorites_degraded "));

        peer.send("status");
        let status = peer.answer();
        assert!(
            status.iter().any(|line| line == "favorites_degraded: 1"),
            "2.10's row: {status:?}"
        );

        peer.send("close");
        assert_eq!(peer.one(), "OK");
        assert_eq!(peer.close().expect("the handler ended"), ());
    }

    /// 2.1: a path the kernel cannot address is refused before anything is bound,
    /// and the message names the length and the limit. The refusal is the reason
    /// `bind` can be tested at all in this binary: it returns before
    /// `with_socket_umask`, which nothing in a test binary may call.
    #[test]
    fn bind_refuses_a_path_the_kernel_cannot_address() {
        let overlong = "x".repeat(SUN_PATH_LIMIT);
        let error = bind(Path::new(&overlong)).expect_err("a path past sun_path cannot bind");
        assert!(
            error.contains(&format!("the limit is {SUN_PATH_LIMIT}")),
            "{error}"
        );
    }
}

/// The mask's own test. It is named `socket_mode_tests` rather than the
/// file-wide `tests` because the interrupted-read change (580dffb) adds a second
/// `mod tests` to this file in the same window, and two modules cannot share a name.
#[cfg(test)]
mod socket_mode_tests {
    use super::*;

    /// The mode of 2.1 is a property of the mask (cbefc4c): a `bind` creates
    /// the socket as `0777 & ~umask`, so this arithmetic is what the daemon
    /// relies on, and the `0o077` this card replaced -- which leaves the owner's
    /// execute bit -- fails here instead of in the whole-workspace run, where it
    /// showed up as a client reading `0700` between the `bind` and the `fchmod`.
    ///
    /// Deliberately arithmetic and not a `bind`: `umask` is process-wide and not
    /// thread-local, and this binary runs its unit tests on parallel threads, so
    /// the only safe place to *call* it is a single-threaded startup path. (A
    /// `bind` here does not only narrow the socket: it narrows every file the
    /// state, statefile and cache tests create while it is in force, and those
    /// fail with `PermissionDenied`.) That the kernel honours the mask is
    /// measured in docs/architecture.md 2.1 (`[L 3]`, with the 3 s stall
    /// recorded there) and asserted end to end by `control_socket`'s
    /// `the_socket_is_0600_in_a_0700_directory`.
    #[test]
    fn the_mask_is_the_mode_the_socket_is_created_with() {
        assert_eq!(
            0o777 & !SOCKET_UMASK,
            0o600,
            "`0o077` leaves the owner's execute bit: the socket exists as `0700` until the fchmod"
        );
    }
}
