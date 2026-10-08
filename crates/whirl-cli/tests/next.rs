//! `whirl next` against a daemon that queues the rotation and then takes its
//! time: the courtesy line reaches stderr first, and stdout carries exactly the
//! bytes it carried before (docs/architecture.md 2.5.1).
//!
//! The daemon here is a fixture, not the real one. A real rotation can run to
//! `schedule.worker_deadline_seconds` and cannot be made slow on demand, so a
//! `UnixListener` in this test's own tree is the only way to hold the verdict
//! back and watch what the client says while it waits. The fixture greets,
//! accepts one `next`, answers `queued`, sleeps, then writes the verdict.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// The CLI binary cargo just built, next to the other bins (see tests/argv.rs
/// for why this package owns the program-level tests).
fn whirl() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_whirl"))
}

/// How long the fixture holds the verdict back: long enough that a client which
/// stayed silent until the answer would print nothing for this long, short
/// enough that the suite is not waiting on it.
const STALL: Duration = Duration::from_millis(1500);

/// The courtesy line, as the client spells it, and the whole of stderr on a
/// success. The bound is the client's rotation request timeout (2.8).
const WAIT_NOTE: &str = "whirl: working; the rotation may take up to 300 s";

/// A `set:` record the fixture answers a successful rotation with; the CLI
/// prints a data line verbatim, so its shape is all that matters here.
const SET_LINE: &str = "set: 3b29b61764d0a17238f7a51d2585eccf538171d638f210e810f4e8eab970387a pictures:401df7171a5e52d88b2f7f9e9d201308f9600e7034916d7865c883f33ec64dcd manual -";

/// The `ERR` line of a failed rotation, from the transcript of 2.11.
const ERR_LINE: &str = "ERR offline every source that could serve this rotation needed the network";

/// A directory of this test's own.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("whirl-cli-next-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    dir
}

/// Run the client once before anything is timed. macOS puts a freshly linked
/// binary through its own verification on first execution, which here costs over
/// a second; that is a cost of the harness, not of the client, and it must not
/// land inside the window this file measures.
fn warm_up() {
    Command::new(whirl())
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the CLI runs");
}

/// The fixture daemon and the client started against it, with the instant the
/// client was started so a test can time its first line.
fn fixture(name: &str, response: &'static [&'static str]) -> (Child, Instant) {
    let dir = scratch(name);
    let socket = dir.join("whirl.sock");
    let listener = UnixListener::bind(&socket).expect("the fixture binds its socket");

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("one client connects");
        writeln!(stream, "{}", whirl_core::protocol::greeting()).expect("the greeting");
        let mut request = String::new();
        BufReader::new(stream.try_clone().expect("a second handle"))
            .read_line(&mut request)
            .expect("the request");
        assert_eq!(request.trim_end(), "next", "the client sent `next`");
        writeln!(stream, "queued").expect("the interim line");
        thread::sleep(STALL);
        for line in response {
            writeln!(stream, "{line}").expect("a verdict line");
        }
    });

    let started = Instant::now();
    let child = Command::new(whirl())
        .args(["next"])
        .env("WHIRL_SOCKET", &socket)
        .env("WHIRL_CONFIG", dir.join("absent.json"))
        .env("HOME", &dir)
        .env_remove("WHIRL_STATE_DIR")
        .env_remove("WHIRL_CACHE_DIR")
        .env_remove("WHIRL_BACKEND")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the CLI starts");
    (child, started)
}

/// One `whirl next`: its exit status, its stdout, stderr line by line with the
/// instant each line arrived, and the instant the client exited.
fn run_next(
    name: &str,
    response: &'static [&'static str],
) -> (ExitStatus, String, Vec<(Duration, String)>, Duration) {
    warm_up();
    let (mut child, started) = fixture(name, response);

    // stderr is read on its own thread so the test can time the lines rather
    // than read them all at the end, when the client has already exited.
    let stderr = child.stderr.take().expect("piped stderr");
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let line = match line {
                Ok(line) => line,
                Err(_) => break,
            };
            if tx.send((started.elapsed(), line)).is_err() {
                break;
            }
        }
    });

    let output = child.wait_with_output().expect("the CLI exits");
    let total = started.elapsed();
    // The reader's sender drops when the pipe closes, so this collects every
    // line without racing the client's exit.
    let errors: Vec<(Duration, String)> = rx.iter().collect();
    reader.join().expect("the stderr reader finishes");
    (
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        errors,
        total,
    )
}

/// The courtesy line is the first stderr line, and it arrives while the client
/// is still waiting rather than with the verdict, which is the whole point of
/// it: a line that only appeared at the end would not stop the silence reading
/// as a hang.
fn assert_courtesy_first(errors: &[(Duration, String)], total: Duration) {
    let (at, line) = errors.first().expect("the courtesy line on stderr");
    assert_eq!(line, WAIT_NOTE);
    assert!(
        *at < Duration::from_secs(1),
        "the courtesy line arrived after {} ms, not within a second",
        at.as_millis()
    );
    assert!(
        total.saturating_sub(*at) > Duration::from_millis(500),
        "the courtesy line arrived {} ms before the verdict; it has to arrive \
         while the client is still waiting",
        total.saturating_sub(*at).as_millis()
    );
}

/// The measured defect: `whirl next` printed `queued` and then said nothing for
/// over a minute. Against a daemon that stalls, the first line on stderr is the
/// courtesy line and it arrives well inside a second, while the verdict still
/// arrives afterwards on stdout.
#[test]
fn the_courtesy_line_arrives_first_and_the_verdict_still_follows() {
    let (status, stdout, errors, total) = run_next("slow", &[SET_LINE, "OK"]);

    assert!(status.success(), "a queued rotation that succeeds exits 0");
    assert_eq!(
        stdout,
        format!("queued\n{SET_LINE}\n"),
        "stdout is the daemon's data lines and nothing new"
    );
    assert_courtesy_first(&errors, total);
    assert_eq!(errors.len(), 1, "a success writes no other stderr line");
}

/// The failure path the report measured: the `ERR` line and the non-zero exit
/// are unchanged, and the courtesy line is the only thing added before them.
#[test]
fn the_failure_path_keeps_its_err_line_and_its_non_zero_exit() {
    let (status, stdout, errors, total) = run_next("failing", &[ERR_LINE]);

    assert_eq!(status.code(), Some(1), "a refused rotation exits 1");
    assert_eq!(
        stdout, "queued\n",
        "the failure path adds nothing to stdout"
    );
    assert_courtesy_first(&errors, total);
    assert_eq!(
        errors.last().map(|(_, line)| line.as_str()),
        Some(ERR_LINE),
        "the ERR line is the daemon's own, unchanged"
    );
}
