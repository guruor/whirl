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

use std::io::{BufRead, BufReader, Write};
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

    // Short on purpose: the socket path has to stay under `SUN_PATH_LIMIT`.
    let dir = std::env::temp_dir().join(format!("whirl-t{}-{}", std::process::id(), short(name)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    // The socket's own directory is left for the daemon to create, so the 0700
    // it promises in 2.1 is what the test measures.
    let socket = dir.join("run").join("whirl.sock");

    // The daemon's stderr goes to a file, never to this process's: a child that
    // inherited the test harness's pipe would hold it open past the run.
    let log = std::fs::File::create(dir.join("daemon.log")).expect("a log file");
    let child = Command::new(&executable)
        .env("WHIRL_CONFIG", dir.join("config.json"))
        .env("WHIRL_SOCKET", &socket)
        .env("WHIRL_STATE_DIR", dir.join("state"))
        .env("WHIRL_CACHE_DIR", dir.join("cache"))
        .env("WHIRL_BACKEND", "noop")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .expect("the daemon starts");

    let deadline = Instant::now() + Duration::from_secs(15);
    while !socket.exists() {
        assert!(
            Instant::now() < deadline,
            "the daemon did not create {} in 15 s",
            socket.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    Daemon { child, dir, socket }
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
