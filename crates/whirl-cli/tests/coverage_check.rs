//! `scripts/coverage-check.py` as a program: the two answers the check gives
//! about a coverage export, and the exit code each one carries. The mode that
//! runs it is `scripts/ci.sh coverage`, so the code the check exits with is what
//! a reader of the job's log goes by.
//!
//! **Why the exit code is the subject.** There are three: 0 the floor and every
//! baseline hold, 1 one of them was crossed (a slide), 2 the check could not be
//! made. Every refusal used to leave the interpreter with `sys.exit(<reason>)`,
//! which is status 1, so "the check could not run" and "the floor was crossed"
//! were the same answer and a run that measured nothing reported a slide. The
//! tests below pin each answer on a fixture: every refusal path exits 2 and
//! prints its reason, a slide exits 1 and names the file it fell below, and a
//! run that holds exits 0.
//!
//! The fixtures are hand-written rather than taken from a coverage run. An
//! export is the shape `read_export` reads and the numbers are chosen so one
//! rule fires and the other does not; a real run is what `scripts/ci.sh
//! coverage` does and takes minutes, and this file has to run inside the suite.
//! The test lives in this package because `cargo test --workspace` is the suite
//! the gate runs and this is where its existing program-level tests sit; the
//! script it drives is repository tooling and belongs to no crate.
//!
//! Unix only, like the gate: `scripts/ci.sh` is bash, the mode's `python3` is
//! the runner's, and `coverage` is a macOS job.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The repository root. This file is `crates/<package>/tests/<name>.rs`.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<package>/tests is three levels below the repository root")
        .to_path_buf()
}

/// A directory of this test's own.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("whirl-coverage-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("a temporary directory");
    dir
}

/// The check, run the way the mode runs it: from the repository root, with the
/// two inputs named and `--root` the tree they are relative to.
fn check(baseline: &Path, report: &Path) -> Output {
    Command::new("python3")
        .arg("scripts/coverage-check.py")
        .arg("--baseline")
        .arg(baseline)
        .arg("--report")
        .arg(report)
        .arg("--root")
        .arg(root())
        .current_dir(root())
        .output()
        .expect("python3 runs scripts/coverage-check.py")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The operating system as the check's own `platform.system()` names it. The
/// check compares the first token of `measured_platform` only, and on unix
/// `uname -s` prints what `platform.system()` returns.
fn operating_system() -> String {
    let output = Command::new("uname")
        .arg("-s")
        .output()
        .expect("uname runs");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// An export of one file, in the shape `read_export` reads, with the total the
/// sum of the files, because an export where the two disagree is refused.
fn export(filename: &str, count: u64, covered: u64) -> String {
    r#"{"data": [{"totals": {"lines": {"count": @count@, "covered": @covered@}}, "files": [{"filename": "@filename@", "summary": {"lines": {"count": @count@, "covered": @covered@}}}]}]}"#
        .replace("@filename@", filename)
        .replace("@count@", &count.to_string())
        .replace("@covered@", &covered.to_string())
}

/// The same export with the total it states moved off the files it lists: the
/// check refuses to judge either.
fn export_with_a_wrong_total(filename: &str, count: u64, covered: u64) -> String {
    r#"{"data": [{"totals": {"lines": {"count": @count@, "covered": @total@}}, "files": [{"filename": "@filename@", "summary": {"lines": {"count": @count@, "covered": @covered@}}}]}]}"#
        .replace("@filename@", filename)
        .replace("@count@", &count.to_string())
        .replace("@total@", &(covered + 1).to_string())
        .replace("@covered@", &covered.to_string())
}

/// A baseline with one floor and one file's uncovered-line count.
fn baseline(platform: &str, floor: f64, file: &str, uncovered: u64) -> String {
    r#"{
  "what": "a fixture for crates/whirl-cli/tests/coverage_check.rs, not a measurement",
  "floor": {
    "lines_percent": @floor@,
    "as_measured_percent": 84.24,
    "measured_on": "2026-10-06",
    "measured_commit": "0000000000000000000000000000000000000000"
  },
  "slack_lines": 6,
  "baseline": {
    "measured_on": "2026-10-06",
    "measured_commit": "0000000000000000000000000000000000000000",
    "measured_platform": "@platform@",
    "measured_with": "a fixture",
    "total_lines": 100,
    "total_uncovered": 40,
    "total_lines_percent": 60.0,
    "files": {
      "@file@": @uncovered@
    }
  }
}"#
    .replace("@platform@", platform)
    .replace("@floor@", &floor.to_string())
    .replace("@file@", file)
    .replace("@uncovered@", &uncovered.to_string())
}

/// Every way the check can fail to reach a verdict exits 2 and says why. A code
/// alone is not enough for a reader to tell it from a slide, so each case
/// asserts the reason too.
#[test]
fn a_check_that_could_not_be_made_exits_2_with_its_reason() {
    let dir = scratch("refusal");
    let file = "crates/whirl-cli/src/render.rs";
    let report = dir.join("llvm-cov.json");
    let baseline_path = dir.join("coverage-baseline.json");
    let held = export(&root().join(file).display().to_string(), 100, 90);
    let floor = baseline(&operating_system(), 10.0, file, 14);

    // A report that is not there: nothing was measured to check.
    fs::write(&baseline_path, floor.as_bytes()).expect("the fixture baseline");
    let output = check(&baseline_path, &dir.join("absent.json"));
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("does not exist: nothing was measured to check"),
        "{}",
        stderr(&output)
    );

    // A baseline that is not there: there is no floor to check.
    fs::write(&report, held.as_bytes()).expect("the fixture export");
    let output = check(&dir.join("absent-baseline.json"), &report);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("does not exist: there is no floor to check"),
        "{}",
        stderr(&output)
    );

    // A report that is not JSON at all: the input cannot be read, which is the
    // one case nothing in the check named before this, so it left a traceback
    // and status 1 behind.
    fs::write(&report, b"not json").expect("the fixture");
    let output = check(&baseline_path, &report);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("the check could not be made"),
        "{}",
        stderr(&output)
    );

    // An export whose total is not the sum of its files: it refuses either.
    let wrong = export_with_a_wrong_total(&root().join(file).display().to_string(), 100, 90);
    let wrong_path = dir.join("wrong-total.json");
    fs::write(&wrong_path, wrong.as_bytes()).expect("the fixture export");
    let output = check(&baseline_path, &wrong_path);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("refusing to check either"),
        "{}",
        stderr(&output)
    );

    // Another platform's baseline: the numbers are the compiler's, so this is
    // not a comparison to make.
    fs::write(&report, held.as_bytes()).expect("the fixture export");
    fs::write(
        &baseline_path,
        baseline("Windows NT", 10.0, file, 14).as_bytes(),
    )
    .expect("the fixture baseline");
    let output = check(&baseline_path, &report);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("was measured on Windows NT, and this run is on"),
        "{}",
        stderr(&output)
    );
}

/// A slide is a verdict: exit 1, and the file it fell below is named. The
/// refusals above used to wear this code.
#[test]
fn a_slide_exits_1_and_names_the_file_it_fell_below() {
    let dir = scratch("slide");
    let file = "crates/whirl-cli/src/render.rs";
    // The run is 40% of 100 lines against a floor of 50, so the workspace slid;
    // render.rs is 60 uncovered against a baseline of 14 with a slack of 6, so
    // the file slid too and it is the file the message has to name.
    let report = dir.join("llvm-cov.json");
    let held = export(&root().join(file).display().to_string(), 100, 40);
    fs::write(&report, held.as_bytes()).expect("the fixture export");
    let baseline_path = dir.join("coverage-baseline.json");
    let floor = baseline(&operating_system(), 50.0, file, 14);
    fs::write(&baseline_path, floor.as_bytes()).expect("the fixture baseline");

    let output = check(&baseline_path, &report);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains(
            "the workspace is at 40.00% of lines and the floor is 50%: 10.00 points under it"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "crates/whirl-cli/src/render.rs has 60 uncovered lines, and its baseline is 14 \
             (slack 6): 46 more than it had"
        ),
        "{text}"
    );
}

/// A run that holds is exit 0, and the green path still prints the line the
/// mode's log quotes. The distinction is only worth anything if this end of it
/// is unchanged.
#[test]
fn a_run_that_holds_exits_0() {
    let dir = scratch("holds");
    let file = "crates/whirl-cli/src/render.rs";
    let report = dir.join("llvm-cov.json");
    let held = export(&root().join(file).display().to_string(), 100, 90);
    fs::write(&report, held.as_bytes()).expect("the fixture export");
    let baseline_path = dir.join("coverage-baseline.json");
    // 10 uncovered against a baseline of 14 with a slack of 6: inside the slack,
    // and 90% against a floor of 10.
    let floor = baseline(&operating_system(), 10.0, file, 14);
    fs::write(&baseline_path, floor.as_bytes()).expect("the fixture baseline");

    let output = check(&baseline_path, &report);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output)
            .contains("the floor holds; every file the baseline names is at or above it"),
        "{}",
        stdout(&output)
    );
}
