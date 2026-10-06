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
/// read even by accident, and the directory is created empty: a `local` source
/// whose every path is missing is refused as a whole (features.md 2.2), which is
/// its own test below.
fn write_config(dir: &Path) -> PathBuf {
    let path = dir.join("config.json");
    std::fs::create_dir_all(dir.join("pictures")).expect("the source's directory");
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
///
/// Two of the five are named rather than left to `HOME`, because `HOME` is only
/// where 4.3's default lives on unix. On Windows the same two names resolve from
/// `%LOCALAPPDATA%` (`paths::local_appdata`), which nothing here sets, so the
/// tests of this binary shared the runner's real `state/locks/rotate.lock` and
/// one of them reported `busy` against another's worker. Naming them puts every
/// root of every spawn inside this test's own directory on every platform, and
/// leaves the platform default unreached rather than merely elsewhere: 4.3 reads
/// `WHIRL_STATE_DIR` and `WHIRL_CACHE_DIR` **before** the default.
fn run(config: &Path, args: &[&str]) -> Output {
    let dir = config.parent().expect("a parent");
    Command::new(worker())
        .arg("--config")
        .arg(config)
        .args(args)
        .env("WHIRL_BACKEND", "noop")
        .env("WHIRL_STATE_DIR", dir.join("state"))
        .env("WHIRL_CACHE_DIR", dir.join("cache"))
        .env("HOME", dir)
        .output()
        .expect("the worker runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// A rotation over an empty directory is `no_candidates` with the sentence the
/// spec names for it, not a success: features.md 1.4's "the source answered but
/// the filter pipeline left nothing", whose message is "no source produced an
/// admissible image" (`docs/architecture.md` 4.5's table, 2.7's code). The
/// reason names the source and what it yielded.
#[test]
fn an_empty_directory_reports_no_candidates_with_the_reason_the_spec_names() {
    let dir = scratch("rotate");
    let config = write_config(&dir);
    let output = run(&config, &["--verb", "rotate", "--run", "1"]);
    assert!(
        !output.status.success(),
        "a rotation that set nothing is not a success"
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "a stage failure exits 1, and never a panic: {}",
        stderr(&output)
    );
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
        errors.contains("no source produced an admissible image"),
        "the sentence 2.7's table and 4.5's name for the case: {errors}"
    );
    assert!(
        errors.contains(
            "source: pictures local weight=1 enabled=1 last=- candidates=0 admitted=0 \
             rejected_resolution=0 rejected_ratio=0 rejected_size=0 rejected_type=0 \
             rejected_dedupe=0 reason=-"
        ),
        "the reason is 2.6's `source:` record, so it says which filter removed what \
         and what the source itself reported: {errors}"
    );
}

/// A rotation whose only configured path does not exist is `no_candidates` too,
/// with the path in the message: features.md 2.2's "a path that is missing or
/// unreadable is a warning; it is an error only if every path fails", and
/// `docs/architecture.md` 4.3's rule that the fact is the worker's and appears
/// as `enabled=0 reason=<...>`. A panic here would be an exit code of 101 and no
/// `stage=` line at all, which is what this test is for.
#[test]
fn a_path_that_does_not_exist_is_no_candidates_and_names_the_path() {
    let dir = scratch("missing-path");
    let absent = dir.join("pictures");
    let config = dir.join("config.json");
    std::fs::write(
        &config,
        format!(
            "{{\n  \"config_schema\": 1,\n  \"backend\": \"noop\",\n  \"sources\": [\n    \
             {{ \"id\": \"pictures\", \"kind\": \"local\", \"weight\": 1, \"paths\": [{}] }}\n  ]\n}}\n",
            json_string(&absent.display().to_string())
        ),
    )
    .expect("the config is written");

    let output = run(&config, &["--verb", "rotate", "--run", "1"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "the named code, not a panic: {}",
        stderr(&output)
    );
    assert_eq!(stdout(&output), "", "nothing ran past the source stage");
    let errors = stderr(&output);
    assert!(
        errors.contains("stage=source code=no_candidates"),
        "the stage and the code (1.6): {errors}"
    );
    assert!(
        errors.contains(&absent.display().to_string()),
        "the path that failed is named: {errors}"
    );
    // The errno text is the platform's, so the test asks the platform for it
    // rather than hardcoding Unix's: the source reports the same
    // `symlink_metadata` failure this line provokes.
    let why = std::fs::symlink_metadata(&absent)
        .expect_err("the path is still missing")
        .to_string();
    assert!(
        errors.contains(&why),
        "and so is the reason it failed ({why}): {errors}"
    );
}

/// A rotation whose one source cannot even be asked is `no_candidates` too, and
/// the reason is that source's own record: 2.6's second form, `enabled=0` with
/// the source's `reason` and no counter group, because nothing was counted. The
/// reason is free-form prose, which is why 2.6 makes it the record's last field.
#[test]
fn a_rotation_whose_source_cannot_be_asked_carries_that_source_s_own_reason() {
    let dir = scratch("disabled-source");
    std::fs::create_dir_all(dir.join("pictures")).expect("the source's directory");
    let absent = dir.join("pictures").join("gone");
    let config = dir.join("config.json");
    std::fs::write(
        &config,
        format!(
            "{{\n  \"config_schema\": 1,\n  \"backend\": \"noop\",\n  \"sources\": [\n    \
             {{ \"id\": \"pictures\", \"kind\": \"local\", \"weight\": 1, \"paths\": [{}] }}\n  ]\n}}\n",
            json_string(&absent.display().to_string())
        ),
    )
    .expect("the config is written");

    let output = run(&config, &["--verb", "rotate", "--run", "7"]);
    let errors = stderr(&output);
    assert!(
        errors.contains("source: pictures local weight=1 enabled=0 last=- reason="),
        "the reason is 2.6's record, with the source's own words after `reason=`: {errors}"
    );
    assert!(
        errors.contains(&absent.display().to_string()),
        "and those words name the path that failed: {errors}"
    );
}

/// A PNG header the sniffer measures, padded to `size` bytes: the pipeline
/// validates a header and never decodes (features.md 1.4), which is what lets a
/// fixture this small stand in for a wallpaper.
fn png(width: u32, height: u32, size: usize) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    assert!(bytes.len() <= size, "the header fits in the fixture");
    bytes.resize(size, 0);
    bytes
}

/// A rotation that admits nothing names the filter that refused each candidate,
/// in the failure line itself and not only in `whirl config check`.
///
/// Five files, each refused by exactly one stage of features.md 2.5's Layer 2, in
/// the pipeline's own order: 8x8 is under the 16x16 floor, 1000x1000 misses the
/// 16:9 target, 4000 bytes is over `filters.max_bytes`, `.php` names a type the
/// platform does not display, and the fifth is in the recent window. "5
/// candidates, 0 admitted" is true of all five and tells a reader nothing; the
/// counters are what say which stage to look at, and they are the same numbers
/// `whirl config check` prints for the same source, because 2.6's record has one
/// builder. A client that only ever calls `next` never runs the check, which is
/// why the failure carries them.
#[test]
fn a_rotation_that_admits_nothing_names_the_filter_that_refused_each_candidate() {
    let dir = scratch("refusals");
    let walls = dir.join("walls");
    std::fs::create_dir_all(&walls).expect("the source's directory");
    for (name, bytes) in [
        ("small.png", png(8, 8, 64)),
        ("square.png", png(1000, 1000, 64)),
        ("fat.png", png(1600, 900, 4000)),
        ("script.php", png(1600, 900, 64)),
        ("known.png", png(1600, 900, 64)),
    ] {
        std::fs::write(walls.join(name), bytes).expect("a fixture image");
    }
    let config = dir.join("config.json");
    std::fs::write(
        &config,
        format!(
            "{{\n  \"config_schema\": 1,\n  \"backend\": \"noop\",\n  \"min_width\": 16,\n  \
             \"min_height\": 16,\n  \"filters\": {{ \"max_bytes\": 1000, \
             \"ratio_tolerance\": 0.02, \"target_ratio\": 1.7777777777777777 }},\n  \
             \"sources\": [ {{ \"id\": \"pictures\", \"kind\": \"local\", \"weight\": 1, \
             \"paths\": [{}], \"include\": [\"*\"] }} ]\n}}\n",
            json_string(&walls.display().to_string())
        ),
    )
    .expect("the config is written");
    // The recent window of 4.1, as the daemon leaves it: the ring holds the
    // origin_key of a rotation that already set `known.png`. It is written by
    // hand because no daemon runs here, and it is the file the worker really
    // reads, through `Window::load`.
    let identity =
        whirl_core::protocol::sha256_hex(walls.join("known.png").display().to_string().as_bytes());
    let state = dir.join("state");
    std::fs::create_dir_all(&state).expect("the state directory");
    std::fs::write(
        state.join("history.json"),
        format!(
            "{{\n  \"schema\": 1,\n  \"seq\": 2,\n  \"written_at\": \"2026-10-06T00:00:00Z\",\n  \
             \"entries\": [\n    {{ \"kind\": \"local\", \"origin_key\": \"pictures:{identity}\", \
             \"digest\": \"{}\", \"cached_path\": null, \"set_at\": \"2026-10-06T00:00:00Z\", \
             \"via\": \"source\" }}\n  ]\n}}\n",
            "f".repeat(64)
        ),
    )
    .expect("the history ring is written");

    let output = run(&config, &["--verb", "rotate", "--run", "1"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let errors = stderr(&output);
    assert!(
        errors.contains(
            "candidates=5 admitted=0 rejected_resolution=1 rejected_ratio=1 rejected_size=1 \
             rejected_type=1 rejected_dedupe=1"
        ),
        "each refusal names itself, from the failure line alone: {errors}"
    );

    // The same numbers through the tool 1.4 names as the diagnostic, for the
    // same source over the same files: the failure's reason is that record.
    let check = run(&config, &["--verb", "check", "--run", "1"]);
    let line = stdout(&check)
        .lines()
        .find(|line| line.starts_with("source: "))
        .expect("the source record")
        .to_string();
    assert!(
        errors.contains(&line),
        "the failure carries the record `config check` prints:\n  {line}\n  {errors}"
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
/// `enabled=1` and the counter group, because the source's kind has an
/// implementation in this build: the group is `crates/whirl-worker/src/sources/`
/// `mod.rs`'s dispatch table's, one arm per kind, and 2.6 says the bracket group
/// is optional so that a kind without an arm can print `enabled=0 reason=<...>`
/// instead. Every count is zero because the scratch directory `write_config`
/// creates is empty, which is what makes this a literal rather than a machine's
/// own count.
///
/// It is a literal on purpose: the assertion is "these exact bytes", and a table
/// this test joined together could hide a reordering of the keys behind a
/// reordering of the table.
const CHECK_OUTPUT: &str = "source: pictures local weight=1 enabled=1 last=- candidates=0 admitted=0 rejected_resolution=0 rejected_ratio=0 rejected_size=0 rejected_type=0 rejected_dedupe=0 reason=-\nplan: schedule.interval_seconds=1800 schedule.worker_deadline_seconds=300 startup.enabled=1 startup.mode=last startup.respect_manual=1 display.mode=all display.mode_effective=all min_width=1600 min_height=900 filters.max_bytes=41943040 filters.ratio_tolerance=0.02 filters.target_ratio=- state.history_entries=50 dedupe.recent_entries=50 cache.root=- cache.max_bytes=2147483648 cache.max_files=500 cache.grace_seconds=600 cache.orphan_grace_seconds=300 backend=noop sources=1\n";

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
///
/// The two roots this spawn does **not** name are its own as well. 4.3 resolves
/// what a process is not given from the platform default, and on this machine the
/// platform default is the human's own `~/Library/Application Support/whirl` and
/// `~/Library/Caches/whirl`; `set` is a rotation, so the worker took
/// `<that>/locks/rotate.lock` (7.2, and `main.rs` takes it for `Verb::Set` too).
/// A spawn with only the two names below therefore rewrote the human's real lock
/// file on every run of this suite, and contended with his own worker for it if
/// he rotated at that moment. `WHIRL_STATE_DIR` and `HOME` now point inside the
/// same scratch directory, which also stops this test reading whatever history
/// ring the human's state directory happens to hold: the recent window it builds
/// is empty rather than his.
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
        // The other two roots of 4.3, inside this test's own tree: the state
        // directory the lock lives in, and `HOME`, which 4.3's platform default
        // is resolved from (`paths::home`) so no other default escapes either.
        .env("WHIRL_STATE_DIR", dir.join("state"))
        .env("HOME", &dir)
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
