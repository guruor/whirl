//! `whirl-worker` as a program, not as a library: the argv, the exit code and
//! the stdout/stderr contract of docs/architecture.md 1.6, driven by hand exactly
//! as docs/development.md section 7 says to drive it.
//!
//! The tests live here and not in `whirld`'s because they spawn this package's
//! own binary: that is what makes `cargo test --workspace` build
//! `target/debug/whirl-worker`, which the daemon's integration tests then find
//! next to the daemon, and it is the reason `cargo test --workspace` on a cold
//! checkout is enough on its own.
//!
//! Nothing here touches a desktop: `WHIRL_BACKEND=noop` is set on every spawn.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The worker binary cargo just built, next to the other bins.
fn worker() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_whirl-worker"))
}

/// A directory of this test's own, short enough for any platform's limits.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("whirl-worker-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    dir
}

/// The config of docs/development.md section 7, with the one local source. The
/// path it names is this test's scratch directory, so nothing on the machine is
/// read even by accident.
fn write_config(dir: &Path) -> PathBuf {
    let path = dir.join("config.json");
    let body = format!(
        "{{\n  \"config_schema\": 1,\n  \"backend\": \"noop\",\n  \"sources\": [\n    \
         {{ \"id\": \"pictures\", \"kind\": \"local\", \"weight\": 1, \"paths\": [{}] }}\n  ]\n}}\n",
        json_string(&dir.join("pictures").display().to_string())
    );
    std::fs::write(&path, body).expect("the config is written");
    path
}

/// Minimal JSON string escaping: the scratch path is the only variable, and a
/// Windows path is full of backslashes.
fn json_string(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// One worker run: the five variables of section 7 plus the argv contract.
fn run(config: &Path, args: &[&str]) -> Output {
    Command::new(worker())
        .arg("--config")
        .arg(config)
        .args(args)
        .env("WHIRL_BACKEND", "noop")
        .env("HOME", config.parent().expect("a parent"))
        .output()
        .expect("the worker runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// A rotation with no source that can be asked is `no_candidates`, not a
/// success: features.md 1.4's "every configured source yielded nothing
/// admissible" (2.7). No `kind` has an implementation in this build, so this is
/// what the shipped binary does on every rotation until `local` or `wallhaven`
/// lands, and the message has to say which kind is missing rather than leaving
/// an operator to guess.
#[test]
fn a_rotation_without_a_source_reports_no_candidates_on_stderr() {
    let dir = scratch("rotate");
    let config = write_config(&dir);
    let output = run(&config, &["--verb", "rotate", "--run", "1"]);
    assert!(
        !output.status.success(),
        "a rotation that set nothing is not a success"
    );
    assert_eq!(output.status.code(), Some(1), "a stage failure exits 1");
    assert_eq!(
        stdout(&output),
        "",
        "nothing was downloaded and nothing was set, so stdout is empty"
    );
    let errors = stderr(&output);
    assert!(
        errors.contains("stage=source code=no_candidates"),
        "stderr carries the failing stage and the code (1.6): {errors}"
    );
    assert!(
        errors.contains("no implementation for kind local in this build"),
        "the message names the missing kind: {errors}"
    );
}

/// docs/architecture.md 2.6's `source:` and `plan:` records, byte for byte, for
/// the config `write_config` writes: 4.2's defaults with one local source and
/// `"backend": "noop"`.
///
/// The order of the plan's keys is 4.2's file order, which is what 2.6's "the
/// config's own dotted key paths, in file order" fixes, with
/// `display.mode_effective` beside `display.mode` where 2.6 and 2.10 put that
/// effective value. `backend=noop` and `sources=1` are this config's own
/// `backend` and its one enabled source; the plan's values are effective values,
/// which is why 2.6 exists at all ("what did the daemon actually adopt").
///
/// `enabled=0` and a reason, with no counter group: no `kind` has an
/// implementation in this build, so no source can be enumerated and nothing can
/// be counted. 4.3 fixes that form ("the worker runs the semantic check ... and
/// prints `enabled=0 reason=<...>`") and 2.6 says the bracket group is optional.
/// When `local` lands, this line grows the group and this assertion is the one
/// that has to change.
///
/// It is a literal on purpose: the assertion is "these exact bytes", and a table
/// this test joined together could hide a reordering of the keys behind a
/// reordering of the table.
const CHECK_OUTPUT: &str = "source: pictures local weight=1 enabled=0 last=- reason=no implementation for kind local in this build\nplan: schedule.interval_seconds=1800 schedule.worker_deadline_seconds=300 startup.enabled=1 startup.mode=last startup.respect_manual=1 display.mode=all display.mode_effective=all min_width=1600 min_height=900 filters.max_bytes=41943040 filters.ratio_tolerance=0.02 filters.target_ratio=- state.history_entries=50 dedupe.recent_entries=50 cache.root=- cache.max_bytes=2147483648 cache.max_files=500 cache.grace_seconds=600 cache.orphan_grace_seconds=300 backend=noop sources=1\n";

#[test]
fn check_prints_the_source_and_plan_records() {
    let dir = scratch("check");
    let config = write_config(&dir);
    let output = run(&config, &["--verb", "check", "--run", "1"]);
    assert!(
        output.status.success(),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    // The whole of stdout, which is what the daemon forwards as the body of its
    // `config check` response (2.5's `config check` row): a missing record, an
    // extra line, either record moved, a changed value or a counter group that
    // appears all fail here.
    let text = stdout(&output);
    assert_eq!(text, CHECK_OUTPUT, "the check output of 2.6, byte for byte");
    // A check is not a rotation: no `set:` line, and the setter is never called.
    assert!(!text.contains("set: "), "{text}");
}

#[test]
fn a_config_it_cannot_read_is_a_stage_and_a_code_on_stderr() {
    let dir = scratch("missing-config");
    let absent = dir.join("nope.json");
    let output = run(&absent, &["--verb", "rotate", "--run", "1"]);
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1), "a stage failure exits 1");
    let errors = stderr(&output);
    assert!(
        errors.contains("stage=config code=bad_config"),
        "stderr carries the failing stage and the code (1.6): {errors}"
    );
    assert!(errors.contains("nope.json"), "{errors}");
    assert_eq!(stdout(&output), "", "nothing on stdout when nothing ran");
}

#[test]
fn a_config_that_does_not_parse_names_the_field_and_the_line() {
    let dir = scratch("bad-config");
    let broken = dir.join("config.json");
    std::fs::write(&broken, "{\n  \"config_schema\": 1,\n  \"sources\": 3\n}\n").unwrap();
    let output = run(&broken, &["--verb", "check", "--run", "2"]);
    assert!(!output.status.success());
    let errors = stderr(&output);
    assert!(errors.contains("stage=config code=bad_config"), "{errors}");
    assert!(errors.contains("sources"), "the field is named: {errors}");
    assert!(errors.contains("line 3"), "the line is named: {errors}");
}

#[test]
fn argv_errors_exit_two_with_the_usage_on_stderr() {
    let dir = scratch("argv");
    let config = write_config(&dir);

    let no_verb = run(&config, &["--run", "1"]);
    assert_eq!(no_verb.status.code(), Some(2), "{}", stderr(&no_verb));
    assert!(
        stderr(&no_verb).contains("--verb is required"),
        "{}",
        stderr(&no_verb)
    );

    let bad_verb = run(&config, &["--verb", "frobnicate", "--run", "1"]);
    assert_eq!(bad_verb.status.code(), Some(2));
    assert!(
        stderr(&bad_verb).contains("is not rotate, set or check"),
        "{}",
        stderr(&bad_verb)
    );

    let no_run = run(&config, &["--verb", "rotate"]);
    assert_eq!(no_run.status.code(), Some(2));
    assert!(
        stderr(&no_run).contains("--run is required"),
        "{}",
        stderr(&no_run)
    );

    let relative = Command::new(worker())
        .args(["--config", "config.json", "--verb", "check", "--run", "1"])
        .output()
        .expect("the worker runs");
    assert_eq!(relative.status.code(), Some(2), "{}", stderr(&relative));
    assert!(
        stderr(&relative).contains("--config must be an absolute path"),
        "{}",
        stderr(&relative)
    );
}

#[test]
fn help_prints_the_argv_contract_and_exits_zero() {
    let output = Command::new(worker())
        .arg("--help")
        .output()
        .expect("the worker runs");
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.contains("usage: whirl-worker"), "{text}");
    assert!(text.contains("--verb <rotate|set|check>"), "{text}");
    assert!(text.contains("--run"), "{text}");
}

/// 4.3's cache root, as a process: `WHIRL_CACHE_DIR` names the directory a
/// `set <digest>` looks in. The value is read **verbatim**, relative included,
/// which is the rule the daemon applies to the same name
/// (`env_path`, crates/whirld/src/plan.rs); 1.2's "a relative value is treated
/// as unset" is about the four XDG variables, not about this one.
///
/// A worker that dropped the value the daemon honoured would fall through
/// `cache.root` (null in this config) to the platform default and answer
/// `not_found` for a digest whose file is sitting under the directory the
/// operator named. The relative form is the one that fails that way, which is
/// why the test uses it: the absolute form was honoured even before the two
/// processes agreed. What a relative value costs is visible in the answer - the
/// path in the `set:` line is relative, because the root it came from is - and
/// that is a property of the operator's override rather than of this resolution.
#[test]
fn set_finds_a_digest_under_whirl_cache_dir() {
    const DIGEST: &str = "264e1a838572fcf30c2e019ed8760ea0c8134f318097718fcfac1390af19cd37";
    assert_eq!(DIGEST.len(), 64, "a content digest is 64 hex characters");

    let dir = scratch("cache-dir");
    let config = write_config(&dir);
    // `sha256/<first two characters>/<next two>/<digest>.<ext>`, the two-level
    // fan-out of state-and-cache section 2 and the shape `Cache::find` probes.
    let cached = Path::new("cache")
        .join("sha256")
        .join(&DIGEST[0..2])
        .join(&DIGEST[2..4])
        .join(format!("{DIGEST}.png"));
    let file = dir.join(&cached);
    std::fs::create_dir_all(file.parent().expect("a parent")).expect("the cache tree");
    std::fs::write(&file, b"a file the cache already holds").expect("a cache file");

    let output = Command::new(worker())
        .arg("--config")
        .arg(&config)
        .args(["--verb", "set", "--target", DIGEST, "--run", "3"])
        .env("WHIRL_BACKEND", "noop")
        .env("WHIRL_CACHE_DIR", "cache")
        .current_dir(&dir)
        .output()
        .expect("the worker runs");
    assert!(
        output.status.success(),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stdout(&output).contains(&cached.display().to_string()),
        "the set line names the file under the environment's directory: {}",
        stdout(&output)
    );
}
