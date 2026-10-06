//! `whirl report`, as a program: the two artefacts it writes, the exit status
//! that says whether the run was made, and the refusals.
//!
//! The emitter ships inside the binary under test, so the tests drive the real
//! binary rather than a function: what a tester on a machine under test runs is
//! what these tests run. The observations are synthetic and say so; the point of
//! these tests is the emitter's behaviour, not any machine's results.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The CLI binary cargo just built.
fn whirl() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_whirl"))
}

/// The exit statuses of docs/architecture.md 2.5.1: 0 the step was done, 1 whirl
/// refused, 3 the command line was wrong.
const DONE: i32 = 0;
const REFUSED: i32 = 1;
const WRONG_COMMAND_LINE: i32 = 3;

/// The checklist of the platform this test runs on. `whirl report` refuses
/// another platform's checklist, because it runs on the machine under test.
fn platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "windows",
        "linux" => "linux",
        _ => "macos",
    }
}

/// The platform's checklist, as the emitter knows it: its size and its ends, so
/// that these tests hold on all three platforms instead of on the one they were
/// written on.
fn items() -> usize {
    match platform() {
        "windows" => 13,
        "linux" => 5,
        _ => 21,
    }
}

fn first_item() -> &'static str {
    match platform() {
        "windows" => "W1",
        "linux" => "gnome",
        _ => "V1",
    }
}

fn last_item() -> &'static str {
    match platform() {
        "windows" => "W13",
        "linux" => "x11",
        _ => "C1",
    }
}

/// A directory of this test's own, holding the run's observations and its log.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("whirl-report-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    std::fs::write(dir.join("run.log"), b"one probe's output\n").expect("a log file");
    dir
}

/// The observations file, written into `dir`.
fn observations(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("observations.json");
    std::fs::write(&path, body).expect("an observations file");
    path
}

/// The command line for a run, less `--observations`. The platform-specific
/// bullet each section needs beyond the run line is supplied here. `build_commit`
/// is a parameter because a run with a missing build identity is one of the
/// refusals the tests prove.
fn command(dir: &Path, build_commit: bool) -> Command {
    let mut command = Command::new(whirl());
    command
        .arg("report")
        .arg("--checklist")
        .arg(platform())
        .arg("--log")
        .arg(dir.join("run.log"))
        .arg("--build-version")
        .arg("9.9.9");
    if build_commit {
        command
            .arg("--build-commit")
            .arg("0123456789abcdef0123456789abcdef01234567");
    }
    command
        .arg("--backend")
        .arg("noop")
        .arg("--machine")
        .arg("test-machine")
        .arg("--os-version")
        .arg("0.0")
        .arg("--by")
        .arg("test-runner")
        .arg("--at")
        .arg("2026-01-02T03:04:05Z")
        .arg("--out")
        .arg(dir.join("report.json"))
        .arg("--row")
        .arg(dir.join("row.md"));
    match platform() {
        "macos" => {
            command
                .arg("--signed")
                .arg("no")
                .arg("--first-run")
                .arg("clear the quarantine attribute");
        }
        "windows" => {
            command.arg("--smartscreen").arg("More info, Run anyway");
        }
        _ => {}
    }
    command
}

/// One run, with the observations body as its only input.
fn run(dir: &Path, body: &str) -> Output {
    command(dir, true)
        .arg("--observations")
        .arg(observations(dir, body))
        .output()
        .expect("the emitter runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("the scratch directory")
        .map(|entry| entry.expect("a directory entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).expect("an artefact the run wrote")
}

#[test]
fn a_run_that_reports_one_item_writes_both_files_and_exits_zero() {
    let dir = scratch("done");
    let body = format!(
        r#"{{ "checklist": "{}", "items": [
            {{ "item": "{}", "verdict": "pass", "evidence": "the probe ran" }}
        ] }}"#,
        platform(),
        first_item()
    );
    let output = run(&dir, &body);

    assert_eq!(output.status.code(), Some(DONE), "{}", stderr(&output));
    assert_eq!(
        names(&dir),
        ["observations.json", "report.json", "row.md", "run.log"]
    );

    let row = read(&dir, "row.md");
    assert!(
        row.contains(&format!("[{}] ", first_item())),
        "the row names the item: {row}"
    );
    assert!(
        row.contains("the probe ran"),
        "the row carries the evidence: {row}"
    );
    assert!(row.contains("1 of the checklist's items passed"), "{row}");
    assert!(row.contains("not run"), "{row}");

    // The report carries the machine, and the log's path, which the public row
    // names by name only.
    let report = read(&dir, "report.json");
    assert!(report.contains("\"machine\": \"test-machine\""), "{report}");
    assert!(report.contains("run.log"), "{report}");
    assert!(report.contains("\"path\""), "{report}");
    assert!(report.contains("\"sha256\""), "{report}");
}

#[test]
fn an_item_the_run_did_not_exercise_is_reported_not_run() {
    let dir = scratch("not-run");
    // The last item is observed and passes. The rest are supplied by no
    // observation at all, and the rule is that none of them is a pass.
    let body = format!(
        r#"{{ "items": [ {{ "item": "{}", "verdict": "pass", "evidence": "ran" }} ] }}"#,
        last_item()
    );
    let output = run(&dir, &body);
    assert_eq!(output.status.code(), Some(DONE), "{}", stderr(&output));

    let report = read(&dir, "report.json");
    assert!(report.contains("\"pass\": 1"), "{report}");
    assert!(
        report.contains(&format!("\"not_run\": {}", items() - 1)),
        "every item with no observation is not-run: {report}"
    );
    assert!(
        report.contains(&format!("\"id\": \"{}\"", first_item())),
        "the unobserved item is in the report: {report}"
    );
    assert!(report.contains("\"verdict\": \"not-run\""), "{report}");

    let row = read(&dir, "row.md");
    assert!(row.contains("not run, with the reason for each"), "{row}");
    assert!(
        row.contains(&format!("[{}] ", first_item())),
        "the unobserved item is named under not run: {row}"
    );
}

#[test]
fn a_missing_build_identity_refuses_and_names_it_and_writes_nothing() {
    let dir = scratch("no-build");
    let output = command(&dir, false)
        .arg("--observations")
        .arg(observations(&dir, r#"{ "items": [] }"#))
        .output()
        .expect("the emitter runs");

    assert_eq!(output.status.code(), Some(REFUSED));
    assert!(
        stderr(&output).contains("--build-commit"),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        names(&dir),
        ["observations.json", "run.log"],
        "nothing was written"
    );
}

#[test]
fn a_verdict_with_no_evidence_is_refused_and_writes_nothing() {
    let dir = scratch("no-evidence");
    let body = format!(
        r#"{{ "items": [ {{ "item": "{}", "verdict": "pass" }} ] }}"#,
        first_item()
    );
    let output = run(&dir, &body);

    assert_eq!(output.status.code(), Some(REFUSED));
    assert!(
        stderr(&output).contains("no evidence"),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        names(&dir),
        ["observations.json", "run.log"],
        "nothing was written"
    );
}

#[test]
fn an_unknown_item_refuses_and_names_it() {
    let dir = scratch("unknown-item");
    let output = run(
        &dir,
        r#"{ "items": [ { "item": "V99", "verdict": "pass", "evidence": "x" } ] }"#,
    );

    assert_eq!(output.status.code(), Some(REFUSED));
    assert!(stderr(&output).contains("V99"), "{}", stderr(&output));
    assert_eq!(
        names(&dir),
        ["observations.json", "run.log"],
        "nothing was written"
    );
}

#[test]
fn a_command_line_that_is_not_flags_exits_three() {
    let output = Command::new(whirl())
        .args(["report", "--checklist"])
        .output()
        .expect("the emitter runs");

    assert_eq!(output.status.code(), Some(WRONG_COMMAND_LINE));
    assert!(
        stderr(&output).contains("--checklist needs a value"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_summary_goes_to_stdout_and_the_run_wrote_both_files() {
    let dir = scratch("streams");
    let body = format!(
        r#"{{ "items": [ {{ "item": "{}", "verdict": "pass", "evidence": "ran" }} ] }}"#,
        first_item()
    );
    let output = run(&dir, &body);

    assert_eq!(output.status.code(), Some(DONE), "{}", stderr(&output));
    assert!(stdout(&output).contains("report: "), "{}", stdout(&output));
    assert!(stdout(&output).contains("row: "), "{}", stdout(&output));
    assert!(
        stdout(&output).contains(&format!("items: {}", items())),
        "{}",
        stdout(&output)
    );
    assert_eq!(stderr(&output), "");
    assert!(dir.join("report.json").is_file());
    assert!(dir.join("row.md").is_file());
}
