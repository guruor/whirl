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
    start_with(name, true, prepare)
}

/// The same daemon with `WHIRL_CONFIG` unset and `HOME` pointed at its own
/// directory, so the config path it resolves is the documented platform default,
/// inside this test's own tree rather than the user's:
/// `config_path_defaults_to_the_documented_platform_location` asserts it.
fn start_without_whirl_config(name: &str) -> Daemon {
    start_with(name, false, |_| {})
}

/// One temporary tree per test, and the one place `spawn_daemon`'s third
/// argument is decided.
fn start_with(name: &str, whirl_config: bool, prepare: impl FnOnce(&Path)) -> Daemon {
    let dir = std::env::temp_dir().join(format!("whirl-t{}-{}", std::process::id(), short(name)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let socket = dir.join("run").join("whirl.sock");
    prepare(&dir);
    Daemon {
        child: spawn_daemon(&dir, &socket, whirl_config),
        dir,
        socket,
    }
}

/// The config file this daemon read: `WHIRL_CONFIG`, resolved in `spawn_daemon`.
fn config_path(daemon: &Daemon) -> PathBuf {
    daemon.dir.join("config.json")
}

/// A config this test owns and can predict, written before the daemon starts and
/// therefore the file it reads instead of 4.2's annotated default.
///
/// It exists because the default's `local` source points at `~/Pictures/Wallpapers`
/// and `/Volumes/Media/walls`, and the `local` kind has an implementation: the
/// `candidates=`/`admitted=` counters in a `config check` response would then be
/// this machine's count, which is the one thing a byte-for-byte assertion cannot
/// be built on. Here the source's directory is inside the test's own tree and is
/// left empty, so every counter is zero on every machine, and the two sources and
/// their weights are 4.2's own (`sources=2` in the `plan:` line is unchanged).
///
/// This file is `#![cfg(unix)]`, so the path needs no JSON escaping.
fn write_local_config(dir: &Path) {
    let walls = dir.join("walls");
    std::fs::create_dir_all(&walls).expect("the source's directory");
    std::fs::write(
        dir.join("config.json"),
        format!(
            "{{\n  \"config_schema\": 1,\n  \"sources\": [\n    \
             {{ \"id\": \"pictures\", \"kind\": \"local\", \"weight\": 1, \"paths\": [\"{}\"] }},\n    \
             {{ \"id\": \"space\", \"kind\": \"wallhaven\", \"weight\": 3, \"query\": \"landscape\" }}\n  ]\n}}\n",
            walls.display()
        ),
    )
    .expect("the test's own config");
}

/// The platform default config path of docs/architecture.md 4.2's `socket`
/// comment, resolved from `home` (the rules live in
/// `crates/whirl-core/src/config.rs::paths`).
///
/// The arms are gated visibly rather than collapsed into one path that happens
/// to hold here: macOS is `~/Library/Application Support/whirl/config.json`,
/// Linux is `$XDG_CONFIG_HOME/whirl/config.json` or `~/.config/whirl/config.json`
/// (`spawn_daemon` removes `XDG_CONFIG_HOME`, so this arm is the `~/.config`
/// one), and Windows is `%APPDATA%\whirl\config.json`, which this file cannot
/// exercise: it is `#![cfg(unix)]` because 2.1's Windows transport, a named
/// pipe, is a later card.
fn default_config_path(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    let relative = "Library/Application Support/whirl/config.json";
    #[cfg(all(unix, not(target_os = "macos")))]
    let relative = ".config/whirl/config.json";
    home.join(relative)
}

/// One daemon child, waited for by its socket. Spawning and waiting are separate
/// functions so a test can restart a daemon over a directory that already has
/// state in it.
///
/// `whirl_config` false omits `WHIRL_CONFIG` (and removes it if this process has
/// one) and points `HOME` at `dir`, so the daemon resolves the documented
/// platform default: inside this test's tree, never the user's own config.
fn spawn_daemon(dir: &Path, socket: &Path, whirl_config: bool) -> Child {
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
    let mut command = Command::new(&executable);
    command
        .env("WHIRL_SOCKET", socket)
        .env("WHIRL_STATE_DIR", dir.join("state"))
        .env("WHIRL_CACHE_DIR", dir.join("cache"))
        .env("WHIRL_BACKEND", "noop")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    if whirl_config {
        command.env("WHIRL_CONFIG", dir.join("config.json"));
    } else {
        command
            .env_remove("WHIRL_CONFIG")
            .env("HOME", dir)
            // Linux resolves the default under `$XDG_CONFIG_HOME` first (4.2's
            // `socket` comment); removing it makes `~/.config` the arm in force.
            .env_remove("XDG_CONFIG_HOME");
    }
    let child = command.spawn().expect("the daemon starts");

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
        "lock_mode: flock",
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

/// The worker really runs and really reports (1.6): a manual `set path` spawns
/// `whirl-worker` with `set --target`, the noop backend leaves the desktop alone,
/// and the daemon records what came back.
///
/// A `next` cannot stand in for this one here. Neither `local` nor `wallhaven`
/// has an implementation in this build, so a rotation asks a source, gets
/// `no_candidates` back from a real worker, and fails; that path is the test
/// below, `a_failed_rotation_is_visible_on_both_planes`. A `set path` is the
/// route that reaches a real worker and a real `set:` line with no source at all.
#[test]
fn a_manual_set_runs_the_worker_through_the_noop_backend() {
    let daemon = start("a_manual_set_runs_the_worker_through_the_noop_backend");
    let file = daemon.dir.join("mine.png");
    std::fs::write(&file, b"a file the user set by hand").expect("a file to set");
    let lines = daemon.ask(&format!("set path {}", file.display()));
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
    assert_eq!(fields[2], "manual", "a `set path` is `via: manual` (2.6)");
    assert!(
        fields[1].starts_with("external:"),
        "a hand-set file's origin is `external:<sha256 of the path>` (2.6): {set}"
    );
    assert_eq!(
        fields[3],
        file.display().to_string(),
        "the user's own file is referenced, never copied (6.4)"
    );

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

/// 4.3's cache root, end to end, in the case the defect reported: no
/// `cache.root` in the config and `WHIRL_CACHE_DIR` in the daemon's environment.
/// `spawn_daemon` puts every daemon in this file under `<dir>/cache`, so the
/// environment is what names the root here and the config's `root` is null.
///
/// Two things are asserted, not one: the answer (`status`'s `cache_dir:`) and
/// the filesystem the answer names. The daemon creates its cache root as it
/// starts and probes it (8.4's `tmp/<run>-probe.part`, written and removed), so
/// `tmp/` existing is the daemon having really opened the directory it reported,
/// rather than a string it can print without touching.
#[test]
fn the_cache_dir_is_whirl_cache_dir_when_the_config_sets_no_root() {
    let daemon = start("the_cache_dir_is_whirl_cache_dir_when_the_config_sets_no_root");
    let config =
        std::fs::read_to_string(config_path(&daemon)).expect("the config the daemon wrote");
    assert!(
        config.contains("\"root\": null"),
        "the default config this test relies on sets no cache root: {config}"
    );

    let lines = daemon.ask("status");
    let environment = daemon.dir.join("cache");
    assert_eq!(
        value(&lines, "cache_dir"),
        environment.display().to_string(),
        "4.3: the environment beats the compiled default"
    );
    assert!(
        environment.join("tmp").is_dir(),
        "the daemon created and probed the root it reported"
    );
    assert_eq!(
        value(&lines, "cache_writable"),
        "1",
        "the root it probed is writable: {lines:?}"
    );
}

/// The same knob with the config file also set: `cache.root` names a second
/// path inside this test's tree, and `WHIRL_CACHE_DIR` still wins. The file's
/// root is asserted absent rather than merely unused, because a daemon that read
/// it would have created it: this is the arm of 4.3 the defect put in doubt
/// (the compiled default winning over the file, and both losing to the
/// environment).
#[test]
fn whirl_cache_dir_beats_a_cache_root_the_config_file_sets() {
    let daemon = start_prepared(
        "whirl_cache_dir_beats_a_cache_root_the_config_file_sets",
        |dir| {
            std::fs::write(
                dir.join("config.json"),
                format!(
                    "{{\n  \"cache\": {{ \"root\": \"{}\" }},\n  \"sources\": []\n}}\n",
                    dir.join("from_the_file").display()
                ),
            )
            .expect("a config that sets cache.root");
        },
    );

    let lines = daemon.ask("status");
    let environment = daemon.dir.join("cache");
    assert_eq!(
        value(&lines, "cache_dir"),
        environment.display().to_string(),
        "4.3: the environment beats the config file"
    );
    assert!(environment.join("tmp").is_dir());
    assert!(
        !daemon.dir.join("from_the_file").exists(),
        "the file's root is not the one the daemon opened"
    );
}

/// 4.3's whole environment layer, one daemon, all five names of the list. The
/// config file is written to disagree with every one of them, so a name the
/// daemon ignored would show up as the file's value in the answer: this is the
/// class of defect the cache root was one instance of.
///
/// What each name is checked against, and why that is the check: `WHIRL_CONFIG`
/// against the file the daemon says it read (`config path`), which is what makes
/// every decoy below reachable at all; `WHIRL_SOCKET` against the socket this
/// test is talking to, with the file's socket asserted unbound, because a daemon
/// that preferred the file would be listening where this test cannot see it;
/// `WHIRL_STATE_DIR` and `WHIRL_CACHE_DIR` against `status`'s two directories,
/// with the file's cache root asserted uncreated; `WHIRL_BACKEND` against the
/// plan `config check` prints, where the file says `native` and the environment
/// says `noop`.
#[test]
fn every_environment_name_in_4_3_beats_the_config_file() {
    let daemon = start_prepared(
        "every_environment_name_in_4_3_beats_the_config_file",
        |dir| {
            std::fs::write(
                dir.join("config.json"),
                format!(
                    "{{\n  \"socket\": \"{}\",\n  \"cache\": {{ \"root\": \"{}\" }},\n  \
                 \"backend\": \"native\",\n  \"sources\": []\n}}\n",
                    dir.join("decoy.sock").display(),
                    dir.join("decoy-cache").display()
                ),
            )
            .expect("a config that disagrees with the environment");
        },
    );

    let config = daemon.ask("config path");
    assert_eq!(
        value(&config, "config"),
        config_path(&daemon).display().to_string(),
        "WHIRL_CONFIG: the daemon read the file the environment named"
    );

    let lines = daemon.ask("status");
    assert!(
        !daemon.dir.join("decoy.sock").exists(),
        "WHIRL_SOCKET wins over `socket`: this daemon bound the environment's path"
    );
    assert_eq!(
        value(&lines, "state_dir"),
        daemon.dir.join("state").display().to_string(),
        "WHIRL_STATE_DIR wins over the platform default"
    );
    assert_eq!(
        value(&lines, "cache_dir"),
        daemon.dir.join("cache").display().to_string(),
        "WHIRL_CACHE_DIR wins over `cache.root`"
    );
    assert!(
        !daemon.dir.join("decoy-cache").exists(),
        "the file's cache root is not the one the daemon opened"
    );

    let check = daemon.ask("config check");
    assert!(
        check
            .iter()
            .any(|line| line.starts_with("plan: ") && line.contains("backend=noop")),
        "WHIRL_BACKEND=noop wins over `backend: native`: {check:?}"
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

/// docs/architecture.md 2.6's `plan:` line, byte for byte, for the config this
/// test's daemon writes: 4.2's annotated example is the file the daemon writes
/// when `config.json` is missing, so its keys are 4.2's keys and its values 4.2's
/// defaults.
///
/// 2.6 fixes the shape and the order: `<config key>=<effective value>`, "the
/// config's own dotted key paths, in file order", one `plan:` line per response
/// (2.5's `config check` row). The order below is 4.2's file order, with
/// `display.mode_effective` beside `display.mode` where 2.6's bullet and 2.10
/// put that effective value.
///
/// `backend` is `noop` and not the file's `native`: 2.6's values are effective
/// values ("what did the daemon actually adopt must be answerable"), and this
/// test runs the daemon and its worker under `WHIRL_BACKEND=noop` (4.3's
/// precedence order ends in the environment and the flags). `sources` is the
/// count of enabled sources, 2.10's own name for that fact.
///
/// It is a literal and not a table the test joins together: a table could hide a
/// reordering of the keys behind a reordering of the table, which is the failure
/// this assertion exists to catch.
const PLAN_CHECK: &str = "plan: schedule.interval_seconds=1800 schedule.worker_deadline_seconds=300 startup.enabled=1 startup.mode=last startup.respect_manual=1 display.mode=all display.mode_effective=all min_width=1600 min_height=900 filters.max_bytes=41943040 filters.ratio_tolerance=0.02 filters.target_ratio=- state.history_entries=50 dedupe.recent_entries=50 cache.root=- cache.max_bytes=2147483648 cache.max_files=500 cache.grace_seconds=600 cache.orphan_grace_seconds=300 backend=noop sources=2";

/// The data lines of the `config check` response of docs/architecture.md 2.5 and
/// 2.6, in order: `queued` first (2.6: "the only interim line", and only for the
/// verbs that spawn a worker), one `source:` record per configured source in
/// config order, then exactly one `plan:` line. The two sources are 4.2's, in its
/// order, with its weights.
///
/// `last` is `-` here because a check has no outcome to report (2.6). The
/// `local` source has `enabled=1` and its counter group, because its kind has an
/// implementation in this build; `wallhaven` keeps `enabled=0` and the reason,
/// because its kind does not. 4.3 fixes the second form and 2.6 says the group is
/// optional, which is what lets one response carry both.
///
/// Every counter is zero and the reason is `-`: `write_local_config` points the
/// source at an empty directory inside this test's own tree, so the assertion is
/// the record's shape rather than a machine's count of its own pictures.
fn config_check_lines() -> Vec<String> {
    vec![
        "queued".to_string(),
        "source: pictures local weight=1 enabled=1 last=- candidates=0 admitted=0 rejected_resolution=0 rejected_ratio=0 rejected_size=0 rejected_type=0 rejected_dedupe=0 reason=-"
            .to_string(),
        "source: space wallhaven weight=3 enabled=0 last=- reason=no implementation for kind wallhaven in this build"
            .to_string(),
        PLAN_CHECK.to_string(),
    ]
}

/// `config check` byte for byte (2.5, 2.6): `queued`, the `source:` records, then
/// the one `plan:` line, and nothing else in the body.
///
/// The whole body is compared as a list, so this fails on a missing or repeated
/// `queued`, on a `source:` record dropped, invented or reordered, on a counter
/// group that appears or disappears, on a `plan:` line moved above the records or
/// duplicated, on a key moved inside the plan, on any value changed, and on any
/// extra line at all. The greeting of 2.4 and the `OK` terminator of 2.6 are
/// asserted separately.
#[test]
fn config_check_reports_the_sources_and_the_plan() {
    let daemon = start_prepared(
        "config_check_reports_the_sources_and_the_plan",
        write_local_config,
    );
    let lines = daemon.ask("config check");
    assert_eq!(lines[0], "OK whirl 0.1.0 protocol 2", "the greeting (2.4)");
    assert_eq!(lines.last().map(String::as_str), Some("OK"));
    assert_eq!(
        body(&lines),
        config_check_lines().as_slice(),
        "2.5's `config check` row, 2.6's `source:` and `plan:` records, in order"
    );
}

/// `config path` with `WHIRL_CONFIG` unset: the documented platform default
/// (2.5 answers `config: <abs path>`, the file in force; 4.2's `socket` comment
/// names the per-platform location, and the rules live in
/// `crates/whirl-core/src/config.rs::paths`).
///
/// `HOME` points at this test's own directory (`start_without_whirl_config`), so
/// the default is resolved inside the temporary tree: the assertion is the
/// platform's rule rather than a path that only holds on this machine, and it
/// never reads or writes the user's real config directory.
#[test]
fn config_path_defaults_to_the_documented_platform_location() {
    let daemon =
        start_without_whirl_config("config_path_defaults_to_the_documented_platform_location");
    let expected = default_config_path(&daemon.dir);
    assert!(
        expected.starts_with(&daemon.dir),
        "the default is resolved from HOME, which this test moved: {}",
        expected.display()
    );
    assert!(
        expected.is_file(),
        "the daemon writes 4.2's annotated file where it resolved the default (1.5): {}",
        expected.display()
    );

    let lines = daemon.ask("config path");
    assert_eq!(lines.last().map(String::as_str), Some("OK"));
    assert_eq!(
        body(&lines),
        [format!("config: {}", expected.display())].as_slice(),
        "the platform default of 4.2, derived from HOME and the platform rule"
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
        assert_eq!(
            fields[2], "external",
            "`kind` names the origin, and a hand-set file's origin is external \
             (state-and-cache's `kind: external` for `history.json`; the \
             `origin_key` prefix `external:` is what the daemon reads, 2.6)"
        );
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
    assert_eq!(
        body(&lines),
        [format!("config: {}", config_path(&daemon).display())].as_slice(),
        "`config path` names the exact file in force (2.5), not a path of the right shape"
    );
}

/// 6.2's ring, end to end: newest first, exactly `state.history_entries` (50)
/// entries, the oldest dropped on write, `history_count` reporting what is in
/// the ring rather than how many rotations have happened, and the whole thing
/// surviving a restart.
///
/// 51 rotations, so the 51st is the one that overflows the bound. What each
/// assertion fails on: a ring with no bound (or one bounded at 51) answers
/// `count: 51` with 51 entries; a ring that drops the newest instead of the
/// oldest loses the last file's digest and keeps the first one; an encoder that
/// wrote the entries oldest-first fails the comparison against the response; a
/// ring that is not read back at startup comes back empty, so both the count and
/// the body of the restart's `history 50` change; and a `history.json` this
/// build cannot parse fails in `HistoryFile::parse` rather than silently.
///
/// The file is read back with the parser the daemon itself uses
/// (`whirl_core::state::HistoryFile`), so the assertion is about the file the
/// next daemon parses rather than about a substring of it, and it is the state
/// layout's own path (`state/history.json`, 6.1).
#[test]
fn the_ring_keeps_the_fifty_newest_entries_and_survives_a_restart() {
    let mut daemon = start("the_ring_keeps_the_fifty_newest_entries_and_survives_a_restart");

    /// The digest of the `set:` record in a rotation's response.
    fn digest_of(lines: &[String]) -> String {
        lines
            .iter()
            .find_map(|line| line.strip_prefix("set: "))
            .unwrap_or_else(|| panic!("a set: line in {lines:?}"))
            .split(' ')
            .next()
            .expect("a digest")
            .to_string()
    }

    /// The digests of a `history` response, in the order it wrote them: field 5
    /// of `entry: <set_at> <via> <kind> <origin_key> <digest> <path|->` (2.6).
    fn recorded_of(lines: &[String]) -> Vec<String> {
        body(lines)
            .iter()
            .filter(|line| line.starts_with("entry: "))
            .map(|line| {
                line["entry: ".len()..]
                    .split(' ')
                    .nth(4)
                    .expect("a digest")
                    .to_string()
            })
            .collect()
    }

    // 51 distinct files, so 51 distinct digests, and the bound is 50.
    let mut digests = Vec::new();
    for index in 0..51 {
        let path = daemon.dir.join(format!("ring-{index:02}.jpg"));
        std::fs::write(&path, format!("ring {index}")).expect("a file to set");
        let lines = daemon.ask(&format!("set path {}", path.display()));
        assert_eq!(
            lines.last().map(String::as_str),
            Some("OK"),
            "rotation {index}: {lines:?}"
        );
        digests.push(digest_of(&lines));
    }
    let mut distinct = digests.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 51, "the 51 sets are distinguishable");
    let oldest = digests[0].clone();
    // `history` is newest first (2.5, 2.6), so the expectation is the reverse of
    // the order the rotations happened in, less the one the ring dropped.
    let expected: Vec<String> = digests[1..].iter().rev().cloned().collect();

    let lines = daemon.ask("history 50");
    assert_eq!(lines.last().map(String::as_str), Some("OK"));
    assert_eq!(
        body(&lines)[0],
        "count: 50",
        "the ring is bounded at state.history_entries (6.2)"
    );
    let recorded = recorded_of(&lines);
    assert_eq!(
        recorded.len(),
        50,
        "the count and the entries agree: {lines:?}"
    );
    assert_eq!(
        recorded, expected,
        "the newest 50, newest first: the 51st set is what dropped the first"
    );
    assert!(
        !recorded.contains(&oldest),
        "the oldest entry is the one the ring dropped (6.2)"
    );

    let status = daemon.ask("status");
    assert_eq!(
        value(&status, "history_entries"),
        "50",
        "the bound 2.10 names"
    );
    assert_eq!(
        value(&status, "history_count"),
        "50",
        "50 entries in the ring after 51 rotations, not 51 (2.10)"
    );

    let history = daemon.dir.join("state").join("history.json");
    let text = std::fs::read_to_string(&history).expect("state/history.json");
    let file = whirl_core::state::HistoryFile::parse(&text)
        .expect("a history file this build wrote")
        .value()
        .expect("schema 1 is this build's schema");
    assert_eq!(
        file.entries.len(),
        50,
        "the file holds the bound, so the 51st rotation dropped one (6.2)"
    );
    let on_disk: Vec<String> = file
        .entries
        .iter()
        .map(|entry| entry.digest.clone().expect("a digest"))
        .collect();
    assert_eq!(
        on_disk, recorded,
        "the file and the response agree, newest first (6.2)"
    );

    // A restart re-reads the file, and nothing here rotates while it does:
    // `set path` is not a slot on the grid, so `next_at` is still in the future
    // and 5.5 rule 6 has no past deadline to rotate on. That is what keeps the
    // two rings comparable.
    daemon.child.kill().expect("the daemon is killed");
    daemon.child.wait().expect("it is reaped");
    daemon.child = spawn_daemon(&daemon.dir, &daemon.socket, true);

    let after = daemon.ask("history 50");
    assert_eq!(after.last().map(String::as_str), Some("OK"));
    assert_eq!(
        body(&after)[0],
        "count: 50",
        "the ring survived the restart"
    );
    assert_eq!(
        recorded_of(&after),
        recorded,
        "same entries, same order, after a restart (6.2, 6.4)"
    );
    let status = daemon.ask("status");
    assert_eq!(value(&status, "history_count"), "50");
    assert_eq!(
        value(&status, "last_digest"),
        expected[0],
        "the anchor is the newest entry's digest (6.1)"
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
    let daemon = start_prepared(
        "the_clis_quickstart_commands_answer_over_the_real_socket",
        write_local_config,
    );

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

    // `set <path>` rather than `next`: this daemon runs 4.2's default config,
    // whose `local` source points into the user's own home and whose `wallhaven`
    // has no implementation in this build. A manual set is the route to a real
    // worker and a real `set:` line that does not depend on the machine.
    let file = daemon.dir.join("quickstart.png");
    std::fs::write(&file, b"a file the quickstart sets").expect("a file to set");
    let (ok, next) = whirl(&daemon, &["set", &file.display().to_string()]);
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
    assert_eq!(
        check,
        format!("{}\n", config_check_lines().join("\n")),
        "the CLI prints the daemon's data lines in order and nothing else (2.5.1)"
    );

    let (ok, path) = whirl(&daemon, &["config", "path"]);
    assert!(ok, "{path}");
    assert_eq!(
        path,
        format!("config: {}\n", config_path(&daemon).display()),
        "the CLI prints the exact path in force (2.5)"
    );

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
        "1",
        "a fresh daemon has seen exactly one event: 5.5's first trigger, the \
         startup sweep's `cache_swept` (2.9)"
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

/// 2.9 end to end, line for line: `subscribed:`, then the exact lines a complete
/// rotation produces (2.11 B is the contract), one event per state change with a
/// `seq` that increases by exactly 1, a command refused inside the stream, and
/// `close` ending it.
///
/// Every line here is compared as a whole string rather than by prefix, and the
/// `rotate_ok` fields are taken from the `set:` record the *other* connection
/// received, so the stream and the response cannot disagree about the digest,
/// the origin, the `via` or the path. What each assertion fails on: a reordered
/// field (the `via` where the path belongs, which a prefix or a field count
/// tolerates), a dropped field, a `-` where a path belongs, a wrong `run`, a
/// wrong `seq`, or an event emitted at a transition that is not the one
/// announced.
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
        "subscribed: 1",
        "`subscribed:` carries the daemon's current seq (2.9); the one event so \
         far is the startup sweep's `cache_swept`, which no client was connected \
         for"
    );
    assert_eq!(
        read_line(&mut reader),
        "gap: 1",
        "the client asked from 0 and the daemon is at 1 (2.9)"
    );

    // A rotation announces exactly two events: the start, whose field is the
    // daemon's monotonic slot counter (1.6), and the outcome. `run` is 1 because
    // this daemon has taken no other slot: `state.slot` starts at 1 and a fresh
    // start arms `next_at` one interval out, so 5.5 rule 6 has no past deadline
    // to rotate on. Two events is what [M 12]'s duplicate wake had to be
    // replaced by (2.9).
    //
    // The state change is a manual `set path`: this daemon runs 4.2's default
    // config, so what a rotation finds is the machine's business (`wallhaven`
    // has no implementation in this build, and the `local` source points into
    // the user's home). Either route takes a slot and produces the two events.
    let file = daemon.dir.join("streamed.png");
    std::fs::write(&file, b"a file the stream set").expect("a file to set");
    let rotation = daemon.ask(&format!("set path {}", file.display()));
    assert_eq!(rotation.last().map(String::as_str), Some("OK"));
    let set = rotation
        .iter()
        .find(|line| line.starts_with("set: "))
        .unwrap_or_else(|| panic!("a set: line in {rotation:?}"));
    let mut fields = set["set: ".len()..].splitn(4, ' ');
    let digest = fields.next().expect("a digest");
    let origin_key = fields.next().expect("an origin_key");
    let via = fields.next().expect("a via");
    let path = fields.next().expect("a path");
    assert_eq!(
        read_line(&mut reader),
        "event: 2 rotate_start 1",
        "the start is announced first, with the slot the worker was given (2.9)"
    );
    assert_eq!(
        read_line(&mut reader),
        format!("event: 3 rotate_ok {digest} {origin_key} {via} {path}"),
        "then the outcome: 2.6's `set:` record, field for field, after `rotate_ok`"
    );
    assert_eq!(
        read_line(&mut reader),
        "event: 4 cache_swept 0 0 0",
        "and 5.5's second trigger, the sweep at the end of the rotation, is the \
         next event and the last one it produces (2.9)"
    );

    // Any other request inside the stream is refused there, and the stream
    // continues: an `ERR` line is distinguishable from an `event` line by prefix.
    writeln!(writer, "status").expect("the request");
    writer.flush().expect("a flush");
    assert_eq!(
        read_line(&mut reader),
        "ERR bad_args subscribe takes over this connection"
    );

    // `pause` and `resume` are one event each, and 2.9 gives both an empty field
    // list: a trailing space or a field here fails on the string.
    assert_eq!(daemon.ask("pause").last().map(String::as_str), Some("OK"));
    assert_eq!(read_line(&mut reader), "event: 5 paused");
    assert_eq!(daemon.ask("resume").last().map(String::as_str), Some("OK"));
    assert_eq!(read_line(&mut reader), "event: 6 resumed");

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

/// 2.11 C3's other half, end to end and line for line: a rotation that produced
/// nothing is visible on both planes, with the same code and the same message --
/// 2.7's `ERR` to the client, and 2.9's `rotate_failed <code> <message>` on the
/// stream -- and `2.10`'s `last_error` carries the code afterwards.
///
/// The failure needs no special worker: the daemon is configured, before it
/// starts, with one `wallhaven` source and no local one, and no `kind` has an
/// implementation in this build (`crates/whirl-worker/src/sources`'s dispatch
/// table), so the worker reports `no_candidates` on stderr with its failing
/// stage. The config is the minimum 4.3 accepts (`sources` is the only key that
/// decides this outcome; the rest take their defaults), so this test does not
/// have to carry 4.2's whole example.
///
/// A wrong code, a message that is not the worker's own stderr line, the two
/// fields in the other order, or a stream event whose `seq` does not follow the
/// `rotate_start` each fail here: the message is compared as a whole string and
/// the stream line is built from the client's own terminator, so the two planes
/// cannot disagree.
#[test]
fn a_failed_rotation_is_visible_on_both_planes() {
    let daemon = start_prepared("a_failed_rotation_is_visible_on_both_planes", |dir| {
        std::fs::write(
            dir.join("config.json"),
            "{\n  \"sources\": [\n    { \"id\": \"space\", \"kind\": \"wallhaven\", \"weight\": 3, \"query\": \"landscape\" }\n  ]\n}\n",
        )
        .expect("a wallhaven-only config");
    });

    let stream = UnixStream::connect(&daemon.socket).expect("a connection");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("a read timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("a clone"));
    let mut writer = stream;
    assert!(read_line(&mut reader).starts_with("OK whirl "));
    writeln!(writer, "subscribe 0").expect("the request");
    writer.flush().expect("a flush");
    assert_eq!(
        read_line(&mut reader),
        "subscribed: 1",
        "the startup sweep's `cache_swept` is the event a client can no longer see (2.9)"
    );
    assert_eq!(
        read_line(&mut reader),
        "gap: 1",
        "asked from 0, and the daemon is at 1 (2.9)"
    );

    let failure = daemon.ask("next");
    assert_eq!(failure[1], "queued", "the worker was started (2.6)");
    let terminator = failure.last().expect("a terminator");
    let code = terminator.split(' ').nth(1).expect("a code");
    assert_eq!(
        code, "no_candidates",
        "2.7's code for a source that yielded nothing: {terminator}"
    );
    let message = terminator
        .strip_prefix(&format!("ERR {code} "))
        .expect("a message after the code");
    assert!(
        message.starts_with("stage=source code=no_candidates "),
        "the worker's failing stage rides the message (1.6): {message}"
    );

    assert_eq!(
        read_line(&mut reader),
        "event: 2 rotate_start 1",
        "the start is announced even though the rotation then fails (2.9)"
    );
    assert_eq!(
        read_line(&mut reader),
        format!("event: 3 rotate_failed {code} {message}"),
        "and the outcome carries the same code and the same message the client got"
    );
    assert_eq!(
        read_line(&mut reader),
        "event: 4 cache_swept 0 0 0",
        "5.5's second trigger runs after a failed rotation too, and it is the last \
         event the attempt produces (2.9)"
    );

    let status = daemon.ask("status");
    assert_eq!(
        value(&status, "last_error"),
        "no_candidates",
        "2.10's key for the last rotation's code"
    );
    assert_eq!(
        value(&status, "rotation_count"),
        "0",
        "a rotation that produced nothing is not a completed rotation"
    );
    assert_eq!(
        value(&status, "rotating"),
        "0",
        "the failure released the slot (1.7)"
    );
    assert_eq!(
        daemon.ask("ping").last().map(String::as_str),
        Some("OK"),
        "a failed rotation is not a broken connection (2.11 C1)"
    );

    writeln!(writer, "close").expect("the request");
    writer.flush().expect("a flush");
    assert_eq!(read_line(&mut reader), "OK");
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
    assert_eq!(read_line(&mut reader), "subscribed: 3");
    assert_eq!(
        read_line(&mut reader),
        "gap: 2",
        "the startup sweep, the pause and the resume have happened, and the client \
         asked from 1 (2.9)"
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
    // A manual `set path` rather than `next`: this daemon runs 4.2's default
    // config, so whether a rotation finds anything is the machine's business. A
    // manual set is the route to a real worker and a real `set:` line to record.
    let file = daemon.dir.join("state.png");
    std::fs::write(&file, b"a file the state test set").expect("a file to set");
    assert_eq!(
        daemon
            .ask(&format!("set path {}", file.display()))
            .last()
            .map(String::as_str),
        Some("OK")
    );
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
    daemon.child = spawn_daemon(&daemon.dir, &daemon.socket, true);

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
    assert_eq!(
        value(&after, "seq"),
        "1",
        "seq is per daemon run (2.9), and this run's one event is its own startup \
         sweep"
    );
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
    // daemon still answers, and the only event on its sequence is the startup
    // sweep's.
    let lines = daemon.ask("status");
    assert_eq!(value(&lines, "seq"), "1");
    assert_eq!(value(&lines, "rotating"), "0");
    assert_eq!(value(&lines, "state_corrupt"), "-");
}
