//! The control socket, end to end: a real daemon, a real socket, and the
//! quickstart's own commands (docs/development.md section 7).
//!
//! The test spawns `whirld` with all five variables of section 7 pointing into a
//! temporary directory, so it touches neither the user's config, state, cache or
//! socket, and it sets `WHIRL_BACKEND=noop` so no wallpaper is ever set.
//!
//! Run it with `cargo test --workspace`: the daemon looks for `whirl-worker` next
//! to its own executable, and that is where a test build lays the two binaries.
//!
//! Unix only: the transport of docs/architecture.md 2.1 is a Unix domain socket
//! in this build, and the Windows named pipe is a later card. On Windows the
//! whole file compiles to nothing rather than to a test that cannot bind.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    dir: PathBuf,
    socket: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Daemon {
    /// One request, one connection. Every line up to the terminator, including
    /// the greeting, in order.
    fn ask(&self, request: &str) -> Vec<String> {
        let stream = UnixStream::connect(&self.socket).expect("a connection to the daemon");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("a read timeout");
        let mut reader = BufReader::new(stream.try_clone().expect("a clone for reading"));
        let mut writer = stream;
        writeln!(writer, "{request}").expect("the request is written");
        writer.flush().expect("the request is flushed");
        let mut lines = Vec::new();
        let mut line = String::new();
        while reader.read_line(&mut line).expect("a response line") > 0 {
            let text = line.trim_end_matches(['\n', '\r']).to_string();
            let done = text == "OK" || text.starts_with("ERR ");
            lines.push(text);
            line.clear();
            if done {
                return lines;
            }
        }
        panic!("the daemon closed the connection without a terminator: {lines:?}");
    }
}

/// A short, stable digest of a test's name. `sun_path` is 104 bytes on macOS
/// (docs/architecture.md 2.1), the temp directory is already ~50 of them, and
/// the full test name does not fit: this keeps the socket path legal without
/// giving up a directory per test.
fn short(name: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", hash as u32)
}

/// The daemon from docs/development.md section 7, with every path redirected.
///
/// `name` is the test's own name: `cargo test` runs these in parallel inside one
/// process, so a process-wide directory lets five daemons race for one socket
/// and one test's cleanup delete another test's config.
fn start(name: &str) -> Daemon {
    start_prepared(name, |_| {})
}

/// The same daemon, with a hook that runs once the directory exists and before
/// the daemon does, so a test can plant a corrupt state file for 6.4 to find.
fn start_prepared(name: &str, prepare: impl FnOnce(&Path)) -> Daemon {
    let dir = std::env::temp_dir().join(format!("whirl-t{}-{}", std::process::id(), short(name)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let socket = dir.join("run").join("whirl.sock");
    prepare(&dir);
    Daemon {
        child: spawn_daemon(&dir, &socket),
        dir,
        socket,
    }
}

/// One daemon child, waited for by its socket. Spawning and waiting are separate
/// functions so a test can restart a daemon over a directory that already has
/// state in it.
fn spawn_daemon(dir: &Path, socket: &Path) -> Child {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_whirld"));
    let worker = executable
        .parent()
        .expect("the executable has a directory")
        .join("whirl-worker");
    assert!(
        worker.exists(),
        "{} is missing; build the workspace first (`cargo test --workspace`)",
        worker.display()
    );

    // The daemon's stderr goes to a file, never to this process's: a child that
    // inherited the test harness's pipe would hold it open past the run.
    let log = std::fs::File::create(dir.join("daemon.log")).expect("a log file");
    let child = Command::new(&executable)
        .env("WHIRL_CONFIG", dir.join("config.json"))
        .env("WHIRL_SOCKET", socket)
        .env("WHIRL_STATE_DIR", dir.join("state"))
        .env("WHIRL_CACHE_DIR", dir.join("cache"))
        .env("WHIRL_BACKEND", "noop")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .expect("the daemon starts");

    // Ready means "accepting connections", not "the path exists": a restart over
    // the same directory has a stale socket file from the daemon that just died,
    // and the new one has to remove it before it can bind.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if UnixStream::connect(socket).is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the daemon did not bind {} in 15 s",
            socket.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("the path exists")
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn the_socket_is_0600_in_a_0700_directory() {
    let daemon = start("the_socket_is_0600_in_a_0700_directory");
    assert_eq!(mode(&daemon.socket), 0o600, "the socket is 0600 (2.1)");
    assert_eq!(
        mode(daemon.socket.parent().expect("a parent")),
        0o700,
        "the socket's directory is created 0700 (2.1)"
    );
    assert_eq!(mode(&daemon.dir.join("state")), 0o700, "the state dir");
    assert_eq!(mode(&daemon.dir.join("cache")), 0o700, "the cache dir");
}

#[test]
fn status_answers_the_stable_key_set() {
    let daemon = start("status_answers_the_stable_key_set");
    let lines = daemon.ask("status");
    assert_eq!(lines.last().map(String::as_str), Some("OK"));
    assert_eq!(lines[0], "OK whirl 0.1.0 protocol 2", "the greeting (2.4)");
    for key in [
        "daemon_version: whirl 0.1.0",
        "protocol: 2",
        "paused: 0",
        "rotating: 0",
        "interval_s: 1800",
        "last_error: -",
        "history_entries: 50",
        "lock_mode: none",
    ] {
        assert!(lines.iter().any(|line| line == key), "{key} in {lines:?}");
    }
    for line in &lines[1..lines.len() - 1] {
        assert!(
            line.contains(": "),
            "every data line is `key: value`: {line:?}"
        );
    }
}

#[test]
fn a_rotation_runs_the_worker_through_the_noop_backend() {
    let daemon = start("a_rotation_runs_the_worker_through_the_noop_backend");
    let lines = daemon.ask("next");
    assert_eq!(lines.last().map(String::as_str), Some("OK"));
    assert_eq!(
        lines[1], "queued",
        "the client is told before it waits (2.5)"
    );
    let set = lines
        .iter()
        .find(|line| line.starts_with("set: "))
        .unwrap_or_else(|| panic!("a set line in {lines:?}"));
    let fields: Vec<&str> = set["set: ".len()..].split(' ').collect();
    assert_eq!(fields.len(), 4, "set: <digest> <origin_key> <via> <path>");
    assert_eq!(fields[0].len(), 64, "a content digest is 64 hex chars");
    assert!(fields[0].bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(fields[2], "source");
    assert!(fields[1].contains(':'), "an origin_key carries a source id");

    let status = daemon.ask("status");
    assert!(
        status.iter().any(|line| line == "rotation_count: 1"),
        "{status:?}"
    );
    assert!(
        status.iter().any(|line| line == "rotating: 0"),
        "{status:?}"
    );
    assert!(
        status.iter().any(|line| line.starts_with("last_digest: "))
            && status.iter().any(|line| line == "anchor_verified: 1"),
        "the anchor moved: {status:?}"
    );
}

#[test]
fn pause_and_resume_flip_the_flag_and_move_the_sequence() {
    let daemon = start("pause_and_resume_flip_the_flag_and_move_the_sequence");
    let before = daemon.ask("status");
    let seq = |lines: &[String]| -> u64 {
        lines
            .iter()
            .find_map(|line| line.strip_prefix("seq: "))
            .expect("a seq")
            .parse()
            .expect("a number")
    };
    let first = seq(&before);
    assert_eq!(daemon.ask("pause").last().map(String::as_str), Some("OK"));
    let paused = daemon.ask("status");
    assert!(paused.iter().any(|line| line == "paused: 1"), "{paused:?}");
    assert!(seq(&paused) > first, "pause is a state change (2.10 `seq`)");
    assert_eq!(daemon.ask("resume").last().map(String::as_str), Some("OK"));
    let resumed = daemon.ask("status");
    assert!(
        resumed.iter().any(|line| line == "paused: 0"),
        "{resumed:?}"
    );
}

#[test]
fn config_check_reports_the_sources_and_the_plan() {
    let daemon = start("config_check_reports_the_sources_and_the_plan");
    let lines = daemon.ask("config check");
    assert_eq!(lines.last().map(String::as_str), Some("OK"));
    assert!(
        lines.iter().any(|line| line.starts_with("source: ")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with("plan: ")),
        "{lines:?}"
    );
}

/// `ping`, `history` and `config path` byte for byte (2.5, 2.6, 2.11 A): the
/// record form, the `count:` header, the ordering, and a path that is absolute.
/// Two `set path` requests with two different files, so the entries are
/// distinguishable and the newest-first order is asserted rather than assumed.
#[test]
fn history_reports_the_ring_newest_first_in_the_record_form() {
    let daemon = start("history_reports_the_ring_newest_first_in_the_record_form");

    assert_eq!(
        daemon.ask("ping"),
        vec!["OK whirl 0.1.0 protocol 2", "OK"],
        "`ping` is the bare terminator (2.11 A)"
    );
    assert_eq!(
        daemon.ask("history"),
        vec!["OK whirl 0.1.0 protocol 2", "count: 0", "OK"],
        "an empty list is a count, never a bare `OK` (2.6)"
    );

    // The digest each `set path` reported, oldest first.
    let mut digests = Vec::new();
    for name in ["first.jpg", "second.jpg"] {
        let path = daemon.dir.join(name);
        std::fs::write(&path, name).expect("a file to set");
        let lines = daemon.ask(&format!("set path {}", path.display()));
        assert_eq!(lines.last().map(String::as_str), Some("OK"), "{lines:?}");
        let set = lines
            .iter()
            .find(|line| line.starts_with("set: "))
            .unwrap_or_else(|| panic!("a set line in {lines:?}"));
        digests.push(
            set["set: ".len()..]
                .split(' ')
                .next()
                .expect("a digest")
                .to_string(),
        );
    }

    let lines = daemon.ask("history 2");
    assert_eq!(lines.last().map(String::as_str), Some("OK"));
    assert_eq!(body(&lines)[0], "count: 2", "{lines:?}");
    let entries: Vec<&String> = body(&lines)
        .iter()
        .filter(|line| line.starts_with("entry: "))
        .collect();
    assert_eq!(entries.len(), 2, "{lines:?}");
    for (entry, digest) in entries.iter().zip(digests.iter().rev()) {
        let fields: Vec<&str> = entry["entry: ".len()..].split(' ').collect();
        assert_eq!(
            fields.len(),
            6,
            "entry: <set_at> <via> <kind> <origin_key> <digest> <path|->: {entry}"
        );
        assert_eq!(fields[0].len(), 20, "`set_at` is RFC 3339 UTC: {entry}");
        assert_eq!(fields[1], "manual", "a `set path` is `via: manual` (2.6)");
        assert_eq!(fields[2], "local", "`kind` names the origin (2.6)");
        assert!(
            fields[3].contains(':'),
            "an origin_key carries a source id: {entry}"
        );
        assert_eq!(&fields[4], digest, "newest first (2.5): {entry}");
        assert!(
            fields[5].ends_with(".jpg"),
            "the path takes the rest of the line: {entry}"
        );
    }
    let lines = daemon.ask("history 1");
    assert_eq!(lines.last().map(String::as_str), Some("OK"));
    assert_eq!(body(&lines)[0], "count: 1", "`n` bounds the answer (2.5)");
    assert_eq!(
        body(&lines).len(),
        2,
        "the count and one entry, and nothing else: {lines:?}"
    );

    let lines = daemon.ask("config path");
    assert_eq!(lines.last().map(String::as_str), Some("OK"));
    assert_eq!(body(&lines).len(), 1, "one data line: {lines:?}");
    let path = value(&lines, "config");
    assert!(
        path.starts_with('/') && path.ends_with("config.json"),
        "the absolute path of the config in force (2.5): {path}"
    );
}

#[test]
fn refusals_name_their_code_and_the_line_protocol_holds() {
    let daemon = start("refusals_name_their_code_and_the_line_protocol_holds");
    assert_eq!(
        daemon.ask("history 500").last().map(String::as_str),
        Some("ERR bad_args history: n must be 1..=50, got 500"),
        "the message of docs/architecture.md 2.7"
    );
    assert_eq!(
        daemon.ask("hello 1").last().map(String::as_str),
        Some("ERR bad_protocol server speaks 2, client asked for 1")
    );
    assert_eq!(
        daemon.ask("hello 2"),
        vec!["OK whirl 0.1.0 protocol 2", "protocol: 2", "OK"]
    );
    assert_eq!(daemon.ask("close"), vec!["OK whirl 0.1.0 protocol 2", "OK"]);
    let unknown = daemon.ask("frobnicate");
    assert!(
        unknown.last().expect("a terminator").starts_with("ERR "),
        "{unknown:?}"
    );
    // A second daemon on the same socket refuses rather than unlinks it (1.5).
    let second = Command::new(env!("CARGO_BIN_EXE_whirld"))
        .env("WHIRL_SOCKET", &daemon.socket)
        .env("WHIRL_CONFIG", daemon.dir.join("config.json"))
        .output()
        .expect("the second daemon runs");
    assert!(!second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("another daemon"),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
}

/// `whirl`, next to the daemon, as `cargo build --workspace` lays the two down.
fn cli_binary() -> PathBuf {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_whirld"));
    let cli = executable
        .parent()
        .expect("the executable has a directory")
        .join(if cfg!(windows) { "whirl.exe" } else { "whirl" });
    assert!(
        cli.exists(),
        "{} is missing; build the workspace first (`cargo test --workspace`)",
        cli.display()
    );
    cli
}

/// One `whirl` command against the running daemon, with the five variables of
/// docs/development.md section 7 in its environment. Bounded: a client that waits
/// for a line the daemon will never send must fail this test, not hang the suite.
fn whirl(daemon: &Daemon, args: &[&str]) -> (bool, String) {
    let mut child = Command::new(cli_binary())
        .args(args)
        .env("WHIRL_CONFIG", daemon.dir.join("config.json"))
        .env("WHIRL_SOCKET", &daemon.socket)
        .env("WHIRL_STATE_DIR", daemon.dir.join("state"))
        .env("WHIRL_CACHE_DIR", daemon.dir.join("cache"))
        .env("WHIRL_BACKEND", "noop")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the CLI starts");

    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait().expect("the CLI is waited on") {
            Some(status) => break status,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "`whirl {}` did not answer in 30 s: the client is waiting for a line \
                         that never arrived",
                        args.join(" ")
                    );
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };

    // Read after the exit: the CLI's output is a status block, far below the pipe
    // buffer, and the deadline above is what bounds a client that ignores that.
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    (status.success(), format!("{stdout}{stderr}"))
}

/// The quickstart's own commands, run as `docs/development.md` section 7 spells
/// them, against a daemon on a temporary socket. This is the pair a user meets
/// first, and it is the only test that exercises the CLI's wire side: the client
/// has to terminate its request line, or both ends wait forever.
#[test]
fn the_clis_quickstart_commands_answer_over_the_real_socket() {
    let daemon = start("the_clis_quickstart_commands_answer_over_the_real_socket");

    let (ok, status) = whirl(&daemon, &["status"]);
    assert!(ok, "{status}");
    // The greeting is a handshake the client reads and checks, not a data line it
    // prints; the first line of output is the first key of the status block.
    assert!(
        status.starts_with("daemon_version: whirl 0.1.0\n"),
        "the status block comes first: {status}"
    );
    for expected in [
        "daemon_version: whirl 0.1.0",
        "protocol: 2",
        "paused: 0",
        "rotating: 0",
        "sources: 2",
    ] {
        assert!(status.contains(expected), "{expected} in {status}");
    }
    // `OK` is the terminator and is not printed as a data line (2.5.1).
    assert!(!status.contains("\nOK\n"), "{status}");

    let (ok, next) = whirl(&daemon, &["next"]);
    assert!(ok, "{next}");
    assert!(next.lines().any(|line| line == "queued"), "{next}");
    let set = next
        .lines()
        .find(|line| line.starts_with("set: "))
        .unwrap_or_else(|| panic!("a set line in {next}"));
    assert_eq!(
        set.split(' ').count(),
        5,
        "set: <digest> <origin_key> <via> <path>: {set}"
    );

    let (ok, check) = whirl(&daemon, &["config", "check"]);
    assert!(ok, "{check}");
    assert!(
        check.lines().any(|line| line.starts_with("source: ")),
        "{check}"
    );
    assert!(
        check.lines().any(|line| line.starts_with("plan: ")),
        "{check}"
    );

    let (ok, path) = whirl(&daemon, &["config", "path"]);
    assert!(ok, "{path}");
    assert!(path.contains(&daemon.dir.display().to_string()), "{path}");

    let (ok, ping) = whirl(&daemon, &["ping"]);
    assert!(ok, "{ping}");

    // The client clamps `history 500` to the documented maximum rather than
    // sending it: the daemon's own refusal of n > 50 is asserted over the raw
    // socket in `refusals_name_their_code_and_the_line_protocol_holds`.
    let (ok, history) = whirl(&daemon, &["history", "500"]);
    assert!(ok, "{history}");
    assert!(
        history.lines().any(|line| line.starts_with("count: ")),
        "{history}"
    );

    // A refusal reaches the client as the daemon's own line on stderr, and 1 is
    // the one refusal exit code (2.5.1).
    let (ok, refused) = whirl(&daemon, &["set", "not-an-id"]);
    assert!(!ok, "{refused}");
    assert!(
        refused.contains(
            "ERR not_found not-an-id is not an origin_key, not a digest and not a favorite"
        ),
        "{refused}"
    );
}

/// The key list of docs/architecture.md 2.10, in the order that table and the
/// 2.11 transcript print it. `source:` is appended once per enabled source and is
/// checked separately.
const STATUS_KEYS: [&str; 47] = [
    "daemon_version",
    "protocol",
    "platform",
    "pid",
    "seq",
    "uptime_s",
    "rss_kb",
    "paused",
    "rotating",
    "rotation_count",
    "interval_s",
    "next_at",
    "next_in_s",
    "last_digest",
    "last_origin_key",
    "last_via",
    "last_at",
    "last_error",
    "history_entries",
    "history_count",
    "favorites_count",
    "display_mode",
    "display_mode_effective",
    "display_mode_reason",
    "anchor_digest",
    "anchor_path",
    "anchor_verified",
    "cache_dir",
    "cache_root_id",
    "cache_files",
    "cache_bytes",
    "cache_files_cap",
    "cache_bytes_cap",
    "cache_over_cap",
    "cache_over_reason",
    "cache_writable",
    "sweep_deferred",
    "lock_mode",
    "state_dir",
    "state_corrupt",
    "state_quarantined",
    "state_schema_newer",
    "history_lost",
    "favorites_degraded",
    "clock_jump",
    "respect_manual_effective",
    "sources",
];

/// The data lines of a response, without the greeting and the terminator.
fn body(lines: &[String]) -> &[String] {
    let end = if lines.last().map(String::as_str) == Some("OK") {
        lines.len() - 1
    } else {
        lines.len()
    };
    &lines[1..end]
}

/// One key's value, or a panic naming the key: a `status` response this test
/// cannot read is a failure, not a default.
fn value(lines: &[String], key: &str) -> String {
    let prefix = format!("{key}: ");
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("no {key} in {lines:?}"))
        .to_string()
}

/// One line from a connection a test is holding open, so a subscribing client can
/// be driven line by line.
fn read_line(reader: &mut BufReader<UnixStream>) -> String {
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("a response line");
    assert!(read > 0, "the daemon closed the connection early");
    line.trim_end_matches(['\n', '\r']).to_string()
}

/// 2.10's key set, in its key order, and **both** directions: this fails when a
/// documented key is missing from the response and when the response carries a
/// key that 2.10 does not define (an invented key makes the two lists different
/// lengths and different contents, so neither a dropped nor an added key can
/// pass). The number of `source:` lines is pinned to the `sources` counter, which
/// is what keeps a stray extra line from hiding at the end of the block.
#[test]
fn status_has_exactly_the_documented_keys_in_the_documented_order() {
    let daemon = start("status_has_exactly_the_documented_keys_in_the_documented_order");
    let lines = daemon.ask("status");
    assert_eq!(lines.last().map(String::as_str), Some("OK"));

    let keys: Vec<&str> = body(&lines)
        .iter()
        .map(|line| {
            line.split_once(": ")
                .unwrap_or_else(|| panic!("every data line is `key: value`: {line:?}"))
                .0
        })
        .collect();
    let mut expected: Vec<&str> = STATUS_KEYS.to_vec();
    let sources: usize = value(&lines, "sources").parse().expect("a count");
    let source_lines = body(&lines)
        .iter()
        .filter(|line| line.starts_with("source: "))
        .count();
    assert_eq!(
        source_lines,
        sources,
        "one `source:` line per enabled source: {:?}",
        body(&lines)
    );
    expected.resize(expected.len() + sources, "source");
    assert_eq!(
        keys, expected,
        "the status key set and order of docs/architecture.md 2.10 (dropped or invented keys both land here)"
    );

    // The types 2.10 states, for the keys where a wrong shape would still parse.
    assert_eq!(value(&lines, "daemon_version"), "whirl 0.1.0");
    assert_eq!(value(&lines, "protocol"), "2");
    assert!(["macos", "linux", "windows"].contains(&value(&lines, "platform").as_str()));
    assert!(value(&lines, "pid").parse::<u32>().is_ok());
    assert_eq!(
        value(&lines, "seq"),
        "0",
        "a fresh daemon has seen no event"
    );
    assert_eq!(value(&lines, "interval_s"), "1800");
    assert_eq!(value(&lines, "history_entries"), "50");
    assert_eq!(value(&lines, "rotation_count"), "0");
    assert_eq!(
        value(&lines, "last_error"),
        "-",
        "`-` is the unset marker (2.6)"
    );
    assert_eq!(value(&lines, "last_digest"), "-");
    // The deadline is a persisted timestamp and the countdown is derived from the
    // wall clock when the response is written (2.10), so this is one interval
    // out, less however many whole seconds ticked between `Daemon::load` and
    // this response. The window is that latency and nothing else: the countdown
    // can never be stale-file numbers, zero, or non-numeric.
    assert!(
        (1795..=1800).contains(&value(&lines, "next_in_s").parse().expect("a countdown")),
        "next_in_s: {}",
        value(&lines, "next_in_s")
    );
    let next_at = value(&lines, "next_at");
    assert!(
        next_at.len() == 20 && next_at.ends_with('Z') && next_at.contains('T'),
        "`next_at` is an RFC 3339 UTC timestamp (2.6): {next_at}"
    );
    assert_eq!(value(&lines, "state_corrupt"), "-");
    assert_eq!(value(&lines, "state_quarantined"), "-");
    assert_eq!(value(&lines, "state_schema_newer"), "0");
    assert_eq!(value(&lines, "history_lost"), "0");
    assert_eq!(value(&lines, "favorites_degraded"), "0");
    assert_eq!(value(&lines, "cache_writable"), "1");
    assert_eq!(value(&lines, "clock_jump"), "0");
}

/// The keys that are only meaningful in a state the daemon has to be able to
/// reach, reached: `paused` with the `next_in_s` rule, the counters, and the
/// state-layer keys in the default they report on a clean directory.
#[test]
fn status_reaches_the_states_its_keys_are_named_for() {
    let daemon = start("status_reaches_the_states_its_keys_are_named_for");
    assert_eq!(daemon.ask("pause").last().map(String::as_str), Some("OK"));
    let lines = daemon.ask("status");
    assert_eq!(value(&lines, "paused"), "1");
    assert_eq!(
        value(&lines, "next_in_s"),
        "-",
        "a suspended schedule has no deadline to count down to (2.10, 5.5 rule 8)"
    );
    let next_at = value(&lines, "next_at");
    assert!(
        next_at.len() == 20 && next_at.ends_with('Z') && next_at.contains('T'),
        "`next_at` stays an RFC 3339 UTC timestamp while frozen: {next_at}"
    );
    assert!(value(&lines, "uptime_s").parse::<u64>().is_ok());
    assert_ne!(
        value(&lines, "rss_kb"),
        "-",
        "the daemon measures its own RSS"
    );
    assert_eq!(value(&lines, "rotating"), "0");
    assert_eq!(
        value(&lines, "sweep_deferred"),
        "0",
        "5.5 step 1 is this key's only trigger and no sweep runs yet"
    );
    assert_eq!(
        value(&lines, "cache_over_reason"),
        "-",
        "the four causes of 5.4 are sweep outcomes"
    );
    // `resume` re-arms the deadline from now (2.9).
    assert_eq!(daemon.ask("resume").last().map(String::as_str), Some("OK"));
    let resumed = daemon.ask("status");
    assert_eq!(value(&resumed, "paused"), "0");
    // Re-armed from now, so one interval out less the same start-up latency
    // window as above (2.5, 2.9).
    assert!(
        (1795..=1800).contains(&value(&resumed, "next_in_s").parse().expect("a countdown")),
        "next_in_s: {}",
        value(&resumed, "next_in_s")
    );
}

/// 2.9 end to end: `subscribed:`, one event per state change with a `seq` that
/// increases by exactly 1, commands refused inside the stream, and `close` ending
/// it.
#[test]
fn subscribe_streams_one_event_per_state_change() {
    let daemon = start("subscribe_streams_one_event_per_state_change");
    let stream = UnixStream::connect(&daemon.socket).expect("a connection");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("a read timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("a clone for reading"));
    let mut writer = stream;
    assert_eq!(
        read_line(&mut reader),
        "OK whirl 0.1.0 protocol 2",
        "the greeting comes first (2.4)"
    );

    writeln!(writer, "subscribe 0").expect("the request");
    writer.flush().expect("a flush");
    assert_eq!(
        read_line(&mut reader),
        "subscribed: 0",
        "`subscribed:` carries the daemon's current seq (2.9)"
    );

    // One state change on another connection: one event, one seq.
    assert_eq!(daemon.ask("pause").last().map(String::as_str), Some("OK"));
    assert_eq!(read_line(&mut reader), "event: 1 paused");

    // Any other request inside the stream is refused there, and the stream
    // continues: an `ERR` line is distinguishable from an `event` line by prefix.
    writeln!(writer, "status").expect("the request");
    writer.flush().expect("a flush");
    assert_eq!(
        read_line(&mut reader),
        "ERR bad_args subscribe takes over this connection"
    );

    // A rotation announces exactly two events: the start and the outcome, which
    // is what [M 12]'s duplicate wake had to be replaced by (2.9).
    let rotation = daemon.ask("next");
    assert_eq!(rotation.last().map(String::as_str), Some("OK"));
    let start = read_line(&mut reader);
    assert!(
        start.starts_with("event: 2 rotate_start "),
        "the start is announced first: {start}"
    );
    let ok = read_line(&mut reader);
    assert!(
        ok.starts_with("event: 3 rotate_ok "),
        "then the outcome: {ok}"
    );
    assert!(
        ok.split(' ').count() >= 7,
        "rotate_ok carries digest, origin_key, via and a path: {ok}"
    );

    writeln!(writer, "close").expect("the request");
    writer.flush().expect("a flush");
    assert_eq!(
        read_line(&mut reader),
        "OK",
        "`close` ends the stream (2.9)"
    );
    let mut trailing = String::new();
    assert_eq!(
        reader.read_line(&mut trailing).expect("a read"),
        0,
        "the connection is closed after the terminator"
    );
}

/// `since` produces the gap of 2.9 and nothing more: the daemon keeps no event
/// history, so the count is the whole answer.
#[test]
fn subscribe_reports_the_gap_for_a_resume_point() {
    let daemon = start("subscribe_reports_the_gap_for_a_resume_point");
    assert_eq!(daemon.ask("pause").last().map(String::as_str), Some("OK"));
    assert_eq!(daemon.ask("resume").last().map(String::as_str), Some("OK"));
    let stream = UnixStream::connect(&daemon.socket).expect("a connection");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("a read timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("a clone"));
    let mut writer = stream;
    assert!(read_line(&mut reader).starts_with("OK whirl "));
    writeln!(writer, "subscribe 1").expect("the request");
    writer.flush().expect("a flush");
    assert_eq!(read_line(&mut reader), "subscribed: 2");
    assert_eq!(
        read_line(&mut reader),
        "gap: 1",
        "two events have happened and the client asked from 1 (2.9)"
    );
    writeln!(writer, "close").expect("the request");
    writer.flush().expect("a flush");
    assert_eq!(read_line(&mut reader), "OK");
}

/// The state layer end to end: what the daemon wrote is what a second daemon over
/// the same directory believes, including the anchor, which is rebuilt from
/// `history.json` when it has to be (6.1, 6.4 step 3).
#[test]
fn the_state_files_are_written_and_read_back_across_a_restart() {
    let mut daemon = start("the_state_files_are_written_and_read_back_across_a_restart");
    assert_eq!(daemon.ask("next").last().map(String::as_str), Some("OK"));
    assert_eq!(daemon.ask("pause").last().map(String::as_str), Some("OK"));
    assert_eq!(
        daemon.ask("favorite").last().map(String::as_str),
        Some("OK")
    );

    let before = daemon.ask("status");
    let digest = value(&before, "last_digest");
    assert_ne!(digest, "-");

    let state = daemon.dir.join("state");
    for name in ["current.json", "history.json", "favorites.json"] {
        let path = state.join(name);
        assert!(path.is_file(), "{} was written", path.display());
        assert_eq!(mode(&path), 0o600, "{name} is private");
        let text = std::fs::read_to_string(&path).expect("the file");
        assert!(
            text.contains("\"schema\": 1") && text.contains("\"written_at\": \""),
            "{name} carries the schema and a timestamp: {text}"
        );
    }
    let current = std::fs::read_to_string(state.join("current.json")).expect("current.json");
    assert!(current.contains("\"paused\": true"), "{current}");
    assert!(
        current.contains(&format!("pid={}", daemon.child.id())),
        "`written_by` names the process that wrote it (6.1): {current}"
    );
    let history = std::fs::read_to_string(state.join("history.json")).expect("history.json");
    assert!(history.contains(&digest), "{history}");
    // No temp file survives a successful write (6.3).
    let leftovers: Vec<String> = std::fs::read_dir(&state)
        .expect("the state directory")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "no temp files left: {leftovers:?}");

    // Kill it, and start a second daemon over the same directory.
    daemon.child.kill().expect("the daemon is killed");
    daemon.child.wait().expect("it is reaped");
    daemon.child = spawn_daemon(&daemon.dir, &daemon.socket);

    let after = daemon.ask("status");
    assert_eq!(value(&after, "paused"), "1", "the flag survived (6.1)");
    assert_eq!(value(&after, "rotation_count"), "1");
    assert_eq!(value(&after, "last_digest"), digest);
    assert_eq!(
        value(&after, "last_origin_key"),
        value(&before, "last_origin_key"),
        "the anchor's origin is recovered from the history entry with its digest (6.1)"
    );
    assert_eq!(value(&after, "anchor_verified"), "1");
    assert_eq!(value(&after, "anchor_path"), value(&before, "anchor_path"));
    assert_eq!(value(&after, "history_count"), "1");
    assert_eq!(value(&after, "favorites_count"), "1");
    assert_eq!(value(&after, "history_lost"), "0");
    assert_eq!(value(&after, "favorites_degraded"), "0");
    assert_eq!(value(&after, "state_corrupt"), "-");
    assert_eq!(value(&after, "seq"), "0", "seq is per daemon run (2.9)");
}

/// 6.4 step 3: a corrupt `favorites.json` degrades pin state, keeps the bytes in
/// a quarantine, refuses pin-changing verbs with the quarantine path, and leaves
/// everything else working.
#[test]
fn a_corrupt_favorites_file_degrades_pins_and_keeps_the_bytes() {
    let daemon = start_prepared(
        "a_corrupt_favorites_file_degrades_pins_and_keeps_the_bytes",
        |dir| {
            let state = dir.join("state");
            std::fs::create_dir_all(&state).expect("a state directory");
            std::fs::write(state.join("favorites.json"), "not json at all")
                .expect("a corrupt pin file");
            std::fs::write(
                state.join("history.json"),
                "{\"schema\": 1, \"entries\": [\n",
            )
            .expect("a truncated history");
        },
    );

    let lines = daemon.ask("status");
    assert_eq!(
        value(&lines, "favorites_degraded"),
        "1",
        "a quarantine is not an empty pin set (6.4 step 3)"
    );
    assert_eq!(value(&lines, "history_lost"), "1");
    assert_eq!(
        value(&lines, "cache_over_reason"),
        "favorites_degraded",
        "the daemon reports the cause it is protecting the cache for (5.4, 6.4)"
    );
    // Both files failed; `favorites.json` is loaded last, so it is the one the
    // sticky report names. The other quarantine is still on disk.
    assert_eq!(value(&lines, "state_corrupt"), "favorites.json");
    let quarantined = value(&lines, "state_quarantined");
    assert!(
        quarantined.starts_with(&daemon.dir.join("state").display().to_string()),
        "the path it was moved to, in place: {quarantined}"
    );
    assert_eq!(
        std::fs::read_to_string(&quarantined).expect("the quarantined bytes"),
        "not json at all",
        "the file is moved, never deleted (6.4 step 1)"
    );
    let quarantines: Vec<String> = std::fs::read_dir(daemon.dir.join("state"))
        .expect("the state directory")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".corrupt-"))
        .collect();
    assert_eq!(
        quarantines.len(),
        2,
        "one per file that failed: {quarantines:?}"
    );
    assert!(
        quarantines
            .iter()
            .any(|name| name.starts_with("history.json.corrupt-")),
        "{quarantines:?}"
    );

    // Pin-changing verbs are refused with the quarantine path, and reads are not.
    let refusal = format!("ERR favorites_degraded {quarantined}");
    assert_eq!(
        daemon.ask("favorite").last().map(String::as_str),
        Some(refusal.as_str())
    );
    assert_eq!(
        daemon
            .ask("unfavorite space:whatever")
            .last()
            .map(String::as_str),
        Some(refusal.as_str())
    );
    assert_eq!(daemon.ask("history").last().map(String::as_str), Some("OK"));
    assert_eq!(daemon.ask("ping").last().map(String::as_str), Some("OK"));
    // And the daemon has not written a fresh pin file over the quarantine.
    assert_eq!(
        std::fs::read_dir(daemon.dir.join("state"))
            .expect("the state directory")
            .flatten()
            .filter(|entry| entry.file_name() == "favorites.json")
            .count(),
        0,
        "a degraded pin set is never overwritten"
    );
}

/// A state file that is a directory, and one that does not exist at all: 6.4's
/// first two shapes, neither of which may panic.
#[test]
fn a_directory_where_a_state_file_belongs_degrades_instead_of_panicking() {
    let daemon = start_prepared(
        "a_directory_where_a_state_file_belongs_degrades_instead_of_panicking",
        |dir| {
            std::fs::create_dir_all(dir.join("state").join("current.json"))
                .expect("a directory in the file's place");
        },
    );
    let lines = daemon.ask("status");
    assert_eq!(value(&lines, "state_corrupt"), "current.json");
    let quarantined = value(&lines, "state_quarantined");
    assert!(
        quarantined.contains("current.json.corrupt-"),
        "the timestamped quarantine name of 6.4: {quarantined}"
    );
    assert!(std::path::Path::new(&quarantined).is_dir());
    assert_eq!(
        value(&lines, "paused"),
        "0",
        "defaults, and it answered at all"
    );
    assert_eq!(value(&lines, "history_lost"), "0");
    assert_eq!(daemon.ask("ping").last().map(String::as_str), Some("OK"));
    // The next state change writes a real file in the directory's place.
    assert_eq!(daemon.ask("pause").last().map(String::as_str), Some("OK"));
    assert!(daemon.dir.join("state").join("current.json").is_file());
}

/// The framing rules of 2.2 and the refusal codes of 2.7, at the byte level: an
/// unknown verb is answered and the connection survives, an oversized line and a
/// line that is not UTF-8 each end it.
#[test]
fn the_framing_rules_and_the_surviving_connection_hold() {
    let daemon = start("the_framing_rules_and_the_surviving_connection_hold");

    // An unknown verb: refused, and the connection keeps answering (2.7).
    let stream = UnixStream::connect(&daemon.socket).expect("a connection");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("a read timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("a clone"));
    let mut writer = stream;
    assert!(read_line(&mut reader).starts_with("OK whirl "));
    writeln!(writer, "frobnicate").expect("the request");
    writer.flush().expect("a flush");
    let refused = read_line(&mut reader);
    assert!(
        refused.starts_with("ERR unknown_verb "),
        "the code of 2.7: {refused}"
    );
    writeln!(writer, "ping").expect("the request");
    writer.flush().expect("a flush");
    assert_eq!(read_line(&mut reader), "OK", "the connection survived");
    writeln!(writer, "close").expect("the request");
    writer.flush().expect("a flush");
    assert_eq!(read_line(&mut reader), "OK");

    // A line past `MAX_REQUEST_LINE`: refused, and the connection ends (2.2).
    let stream = UnixStream::connect(&daemon.socket).expect("a connection");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("a read timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("a clone"));
    let mut writer = stream;
    assert!(read_line(&mut reader).starts_with("OK whirl "));
    let oversized = format!("history {}\n", "9".repeat(8300));
    writer
        .write_all(oversized.as_bytes())
        .expect("the oversized request");
    writer.flush().expect("a flush");
    assert_eq!(
        read_line(&mut reader),
        "ERR too_long request line exceeds 8192 bytes"
    );
    let mut trailing = String::new();
    assert_eq!(
        reader.read_line(&mut trailing).expect("a read"),
        0,
        "`too_long` closes the connection (2.7)"
    );

    // A line that is not UTF-8: the framing refusal, and the connection ends.
    let stream = UnixStream::connect(&daemon.socket).expect("a connection");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("a read timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("a clone"));
    let mut writer = stream;
    assert!(read_line(&mut reader).starts_with("OK whirl "));
    writer.write_all(b"\xff\xfe status\n").expect("the bytes");
    writer.flush().expect("a flush");
    assert_eq!(
        read_line(&mut reader),
        "ERR bad_framing request line is not UTF-8"
    );
    let mut trailing = String::new();
    assert_eq!(
        reader.read_line(&mut trailing).expect("a read"),
        0,
        "`bad_framing` closes the connection (2.7)"
    );

    // A NUL byte is the transcript's own malformed line (2.11 C2): refused by
    // code, and the connection ends.
    let stream = UnixStream::connect(&daemon.socket).expect("a connection");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("a read timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("a clone"));
    let mut writer = stream;
    assert!(read_line(&mut reader).starts_with("OK whirl "));
    writer.write_all(b"status\0\n").expect("the bytes");
    writer.flush().expect("a flush");
    assert_eq!(
        read_line(&mut reader),
        "ERR bad_framing NUL in request line"
    );
    let mut trailing = String::new();
    assert_eq!(
        reader.read_line(&mut trailing).expect("a read"),
        0,
        "`bad_framing` closes the connection (2.7)"
    );

    // And every one of those refusals was a request, not a state change: the
    // daemon still answers, and its sequence is still zero.
    let lines = daemon.ask("status");
    assert_eq!(value(&lines, "seq"), "0");
    assert_eq!(value(&lines, "rotating"), "0");
    assert_eq!(value(&lines, "state_corrupt"), "-");
}
