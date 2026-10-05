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

/// One command line before arguments, with the environment scrubbed of anything
/// that could reach a real daemon: `WHIRL_SOCKET` points inside the scratch
/// directory, and `WHIRL_CONFIG` at a file that does not exist. A test that needs
/// a different environment (a socket path that exists but is not a socket, an
/// unreadable config) adjusts the returned command.
fn command(dir: &Path) -> Command {
    let mut command = Command::new(whirl());
    command
        .env("WHIRL_SOCKET", dir.join("absent.sock"))
        .env("WHIRL_CONFIG", dir.join("absent.json"))
        .env("HOME", dir)
        .env_remove("WHIRL_STATE_DIR")
        .env_remove("WHIRL_CACHE_DIR")
        .env_remove("WHIRL_BACKEND");
    command
}

/// One command line.
fn run(dir: &Path, args: &[&str]) -> Output {
    command(dir).args(args).output().expect("the CLI runs")
}

/// The names in a directory, sorted, so a test can assert a run created nothing.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("the scratch directory")
        .map(|entry| entry.expect("a directory entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
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

/// This package's own binary is under `target/`, so the one lifecycle verb that
/// writes anything refuses here rather than registering a login item that names
/// a build tree: an accepted `daemon install` in a test run cannot change this
/// machine. `run` also points `HOME` at a scratch directory, so no unit is
/// written into a real home either way.
#[test]
fn install_is_refused_in_a_build_tree_and_writes_nothing() {
    let dir = scratch("install");
    let output = run(&dir, &["daemon", "install"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(!dir.join("Library").exists(), "no unit was written");
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

/// The line `--version` prints, built from this crate's own version rather than
/// a literal, so a version bump cannot make the test stale.
fn version_line() -> String {
    format!("whirl {}\n", env!("CARGO_PKG_VERSION"))
}

/// `--version` answers "what is this binary" from the binary itself. The socket
/// path is occupied by a regular file, so a client that tried to connect would
/// fail instead of answer: exit 0 and an empty stderr are the proof that the
/// static path opened nothing, read nothing and wrote nothing.
#[test]
fn the_version_flag_names_this_build_and_never_reaches_a_daemon() {
    let dir = scratch("version-flag");
    let socket = dir.join("absent.sock");
    std::fs::write(&socket, b"not a socket").expect("a decoy file at the socket path");
    let before = entries(&dir);

    // `-V` is the same answer under the short spelling.
    for flag in ["--version", "-V"] {
        let output = run(&dir, &[flag]);
        assert_eq!(output.status.code(), Some(0), "{flag}: {}", stderr(&output));
        assert_eq!(stderr(&output), "", "{flag} consults no daemon");
        assert_eq!(stdout(&output), version_line(), "{flag}");
        // One line, and its last whitespace-separated field is the bare version,
        // so a shell or the app can take it with `awk '{print $NF}'`.
        assert_eq!(stdout(&output).lines().count(), 1, "{flag}");
        assert_eq!(
            stdout(&output).split_whitespace().last(),
            Some(env!("CARGO_PKG_VERSION")),
            "{flag}"
        );
    }

    // Nothing on the path was touched: no socket unlinked, no file written.
    assert!(socket.exists(), "the decoy file is left where it was");
    assert_eq!(entries(&dir), before, "the --version path writes nothing");
}

/// No configuration is read on the `--version` path either. With `WHIRL_SOCKET`
/// unset, `socket_path()` would fall through to `WHIRL_CONFIG` and print the
/// failed parse on stderr; a garbage config that leaves stderr empty is never
/// read.
#[test]
fn the_version_flag_reads_no_config() {
    let dir = scratch("version-flag-config");
    let config = dir.join("garbage.json");
    std::fs::write(&config, b"{ this is not a whirl config").expect("a decoy config");

    let mut cmd = command(&dir);
    cmd.env_remove("WHIRL_SOCKET").env("WHIRL_CONFIG", &config);
    let output = cmd.arg("--version").output().expect("the CLI runs");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), version_line());
    assert_eq!(stderr(&output), "", "the decoy config is never read");
}

/// The two version answers are told apart by the usage text alone: the flag is
/// the binary's own version, the verb is the daemon's.
#[test]
fn the_usage_tells_the_two_version_answers_apart() {
    let dir = scratch("version-usage");
    let text = stdout(&run(&dir, &["help"]));
    let flag = text
        .lines()
        .find(|line| line.trim_start().starts_with("--version"))
        .unwrap_or_else(|| panic!("no --version line in {text}"));
    let verb = text
        .lines()
        .find(|line| line.trim_start().starts_with("version "))
        .unwrap_or_else(|| panic!("no version line in {text}"));
    assert!(flag.contains("this binary's own version"), "{flag}");
    assert!(verb.contains("then the daemon's"), "{verb}");
    assert_ne!(flag, verb);
}

/// The client's own two lines of `whirl version`, built from this crate rather
/// than written out, so a version bump cannot make the suite stale. Every case
/// below compares against this one string, which is what proves the client's half
/// does not depend on the daemon: no daemon, a stale socket and a reachable
/// daemon all carry the same two lines.
fn client_version_lines() -> String {
    format!(
        "client_version: whirl {}\nclient_protocol: {}\n",
        env!("CARGO_PKG_VERSION"),
        whirl_core::protocol::PROTOCOL_VERSION
    )
}

/// With no daemon at all, `whirl version` still answers about the client: its two
/// lines on stdout, the unreachable reason on stderr, and exit 2. This is the
/// case the command exists for, a user checking whether the pieces match.
#[test]
fn the_version_verb_answers_about_the_client_with_no_daemon() {
    let dir = scratch("version-verb");
    let socket = dir.join("absent.sock");
    let output = run(&dir, &["version"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        client_version_lines(),
        "the client's half is printed with no daemon"
    );
    let errors = stderr(&output);
    assert!(
        errors.starts_with(&format!(
            "whirl: cannot reach the daemon at {}: ",
            socket.display()
        )),
        "{errors}"
    );
    // Nothing was created on the way (2.5.1): the verb reports, it does not write.
    assert!(!socket.exists(), "the CLI must not touch the socket path");
}

/// A socket path that exists but is not a socket: the same shape as the
/// no-daemon case, with the daemon's own reason and the stale-socket note the
/// unreachable row adds when the path exists.
#[test]
fn the_version_verb_names_a_stale_socket_and_still_answers_about_the_client() {
    let dir = scratch("version-stale");
    let socket = dir.join("absent.sock");
    std::fs::write(&socket, b"not a socket").expect("a decoy file at the socket path");
    let output = run(&dir, &["version"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert_eq!(stdout(&output), client_version_lines());
    let errors = stderr(&output);
    assert!(
        errors.starts_with(&format!(
            "whirl: cannot reach the daemon at {} (stale socket; the daemon is not running): ",
            socket.display()
        )),
        "{errors}"
    );
    assert!(
        socket.exists(),
        "the client leaves a stale socket where it found it"
    );
}

/// The exit code the no-daemon case uses is the one the usage text documents, so
/// a script can tell "the client answered but the daemon did not" from a usage
/// error while still reading the client's half.
#[test]
fn the_usage_documents_the_unreachable_exit_code() {
    let dir = scratch("version-exit-code");
    let text = stdout(&run(&dir, &["help"]));
    assert!(
        text.contains("2 the daemon is unreachable"),
        "the usage text names exit 2 as the unreachable case: {text}"
    );
}
