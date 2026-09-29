//! `whirl` as a program, not as a library: the verb table and the exit codes of
//! docs/architecture.md 2.5.1.
//!
//! The tests live in this package because they spawn this package's own binary:
//! `CARGO_BIN_EXE_whirl` is what makes `cargo test --workspace` build
//! `target/debug/whirl`, which the daemon's integration tests then drive over a
//! real socket. Without a test here, `cargo test --workspace` on a cold checkout
//! builds the daemon's binary only, and every integration test that reaches for
//! the CLI or the worker fails on a machine that has never run `cargo build`.
//!
//! Three of the four outcomes of 2.5.1 need no daemon, and the fourth is the one
//! that says the daemon is not there: so nothing here opens a socket, and every
//! assertion holds on Windows too, where the transport is a later card.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The CLI binary cargo just built, next to the other bins.
fn whirl() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_whirl"))
}

/// A directory of this test's own. `WHIRL_SOCKET` points at a path inside it that
/// nothing creates: a client that finds a live daemon there has invented one.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("whirl-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    dir
}

/// One command line. The environment is scrubbed of anything that could reach a
/// real daemon: no `WHIRL_SOCKET` outside the scratch directory, and a
/// `WHIRL_CONFIG` that does not exist.
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(whirl())
        .args(args)
        .env("WHIRL_SOCKET", dir.join("absent.sock"))
        .env("WHIRL_CONFIG", dir.join("absent.json"))
        .env("HOME", dir)
        .env_remove("WHIRL_STATE_DIR")
        .env_remove("WHIRL_CACHE_DIR")
        .env_remove("WHIRL_BACKEND")
        .output()
        .expect("the CLI runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn help_prints_the_verb_table_and_exits_zero() {
    let dir = scratch("help");
    let flag = run(&dir, &["--help"]);
    assert!(flag.status.success(), "{}", stderr(&flag));
    assert_eq!(flag.status.code(), Some(0));

    // `help` is the name USAGE gives this text on its own last line, so the verb
    // and the two flags are one command line with three spellings: same stdout,
    // same exit code, and nothing on stderr.
    for args in [["-h"], ["help"]] {
        let output = run(&dir, &args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}: {}",
            args[0],
            stderr(&output)
        );
        assert_eq!(stdout(&output), stdout(&flag), "{}", args[0]);
        assert_eq!(stderr(&output), "", "{} writes nothing to stderr", args[0]);
    }

    let text = stdout(&flag);
    assert!(text.starts_with("usage: whirl <command>"), "{text}");
    for verb in [
        "next",
        "set <path|id>",
        "history [n]",
        "config check",
        "ping",
        "help",
    ] {
        assert!(text.contains(verb), "{verb} in {text}");
    }
}

/// The commands the usage text advertises, one argv per command. Each entry is
/// padded to a description column, so the verb spec is everything before the
/// first run of two spaces.
fn advertised(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .filter_map(|line| line.strip_prefix("  "))
        .map(|line| line.split("  ").next().unwrap_or_default().trim())
        .filter(|spec| !spec.is_empty())
        .map(|spec| spec.split(' ').map(str::to_owned).collect())
        .collect()
}

/// A concrete command line for one advertised spec: the words as written, with a
/// value for every placeholder, chosen by the placeholder's name.
fn concrete(spec: &[String]) -> Vec<String> {
    spec.iter()
        .map(|word| {
            let placeholder = word.trim_matches(['<', '>', '[', ']']);
            if placeholder == word {
                return word.clone(); // a literal subcommand: `path`, `check`
            }
            match placeholder.split('|').next().unwrap_or_default() {
                "n" => "10".to_owned(),
                "path" => "/tmp/whirl-advertised.jpg".to_owned(),
                // An id of any shape reaches the arm; the value is not the subject.
                _ => "1".to_owned(),
            }
        })
        .collect()
}

/// Every command USAGE names is one this binary accepts. This is the invariant
/// the `help` line broke: it sat at the end of USAGE while `request()` had no arm
/// for it, so the text that exists to tell a reader what to type sent them to the
/// catch-all and exit 3. The reverse direction (an arm USAGE does not name) is
/// not observable from a spawned binary and is not asserted here.
#[test]
fn every_command_the_usage_names_is_accepted() {
    let dir = scratch("advertised");
    let text = stdout(&run(&dir, &["help"]));
    let commands = advertised(&text);
    assert!(
        commands.len() >= 17,
        "the usage parse found {} commands: {commands:?}",
        commands.len()
    );

    for spec in commands {
        let args = concrete(&spec);
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = run(&dir, &words);
        assert!(
            !stderr(&output).contains("is not a command"),
            "{} is advertised and rejected: {}",
            spec.join(" "),
            stderr(&output)
        );
    }
}

#[test]
fn a_wrong_command_line_exits_three() {
    let dir = scratch("usage");

    let empty = run(&dir, &[]);
    assert_eq!(empty.status.code(), Some(3), "{}", stderr(&empty));
    assert!(
        stderr(&empty).contains("whirl: a command is required"),
        "{}",
        stderr(&empty)
    );
    assert!(stderr(&empty).contains("usage: whirl <command>"));

    let unknown = run(&dir, &["frobnicate"]);
    assert_eq!(unknown.status.code(), Some(3));
    assert!(
        stderr(&unknown).contains("frobnicate is not a command"),
        "{}",
        stderr(&unknown)
    );

    let bad_count = run(&dir, &["history", "abc"]);
    assert_eq!(bad_count.status.code(), Some(3));
    assert!(
        stderr(&bad_count).contains("history: abc is not a count"),
        "{}",
        stderr(&bad_count)
    );

    let half_subcommand = run(&dir, &["config", "bogus"]);
    assert_eq!(half_subcommand.status.code(), Some(3));

    let missing_target = run(&dir, &["set"]);
    assert_eq!(missing_target.status.code(), Some(3));
}

#[test]
fn an_unreachable_daemon_exits_two_and_names_the_socket() {
    let dir = scratch("unreachable");
    let socket = dir.join("absent.sock");
    let output = run(&dir, &["status"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert_eq!(stdout(&output), "", "no daemon, no data lines");
    let errors = stderr(&output);
    assert!(
        errors.contains(&format!("cannot reach the daemon at {}", socket.display())),
        "{errors}"
    );
    // Nothing was created on the way: a client reports, it does not unlink (2.5.1).
    assert!(!socket.exists(), "the CLI must not touch the socket path");
}

#[test]
fn the_usage_goes_out_before_a_connection_is_ever_attempted() {
    let dir = scratch("order");
    // `idle` is a real verb, so this is not a usage error: it reaches the socket
    // and reports the daemon it cannot find, which is the difference 2.5.1 draws
    // between exit 3 and exit 2.
    let output = run(&dir, &["idle"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("cannot reach the daemon"),
        "{}",
        stderr(&output)
    );
}
