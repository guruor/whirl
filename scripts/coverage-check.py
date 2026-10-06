#!/usr/bin/env python3
"""The coverage floor and the per-file baseline, read from an llvm-cov export.

`scripts/ci.sh coverage` runs this after the suite has run under coverage, on
the export cargo-llvm-cov writes with `--json --summary-only`. It answers two
questions, and they are two because neither can express the other:

* **the workspace total.** Line coverage must not fall under `floor` in
  `.config/coverage-baseline.json`. One number for the whole workspace, and the
  number it was measured from sits beside it, so the next reader can raise it.
* **each file the baseline names.** The file's uncovered *line count* must not
  grow past its baseline by more than `slack_lines`. The workspace total is a sum,
  so it can sit still while one file slides (another file's gain pays for it), and
  a slide that stays inside the floor's own room is under it. A file at 15% is the
  case this rule exists for: `crates/whirld/src/socket.rs` is 440 lines of which
  almost nothing is covered, and the message names it.

Why uncovered lines and not percents: a line count is an integer, so there is no
rounding to argue about and no float equality to get wrong, and it fails in the
two directions that matter (adding uncovered lines, or removing what covered
them) while passing a change that adds covered lines or deletes dead ones.

Why a slack at all: the daemon spawns the worker, and the worker's profile is
merged from a second process, so a line on that boundary can be covered in one
run and not the next. Measured on this tree on 2026-10-06, seven runs of the same
suite over the same tree: 84.2379, 84.2379, 84.2672, 84.2599, 84.2672, 84.2672 and
84.2672% of lines, which is 2152 uncovered at the worst of them and 2148 at the
best, and three files out of 29 account for all of it:
`crates/whirl-cli/src/daemon/macos.rs` (91 uncovered in five runs, 95 in two),
`crates/whirld/src/worker.rs` (86 or 87) and `crates/whirld/src/plan.rs` (52 or
53). `slack_lines` is the room that noise needs: it is set to the widest per-file
swing seen, 4 lines, with two more, and a slide is wider than it. The floor sits
under the measured number for the same reason: a floor at the measurement is a
red job on a green tree.

The two rules are layered, not duplicated. The floor at 84.1 leaves 18 uncovered
lines of room over the 2152 those runs measured (2170 uncovered is still 84.1% of
13653), which is under the widest workspace swing seen (4 lines) several times
over and above the per-file slack. So a slide inside one file past its slack trips
the per-file rule while the total still holds and the message names the file, and
a slide spread thin across the workspace trips the floor while no file does. A
slide of a few lines spread over several files is under both, and is not caught:
that is the size of the hole, and it is here rather than in a comment on the job.

Why the platform is checked: the numbers are the compiler's, and the platform
backends and the transports are `cfg`-gated, so another operating system's
export counts different lines. A baseline measured on macOS enforced on Linux
would compare two different workspaces. The architecture is recorded and not
enforced: nothing in this workspace is gated on the target architecture
(`grep -rn target_arch crates/` is empty), so arm64 and x86_64 agree on macOS.

`--update` re-measures the baseline from the given export. It writes the file
and leaves `floor` alone: a floor that moved itself every time somebody covered
a line would be a number nobody raises deliberately, and raising it is a
one-line edit of `floor.lines_percent`.

Exit codes: 0 the floor and every baseline hold, 1 one of them was crossed, 2
the check could not be made. The difference is the sentence above it: 1 is a
verdict about coverage and 2 never is. 2 is a report or a baseline that is
missing, unreadable, not an export, or not the platform this run is on, and
every refusal prints its reason, because a reader has the log and not this file.
`scripts/ci.sh` exits 2 for the same thing (its `die`), so the mode and this
check agree, and a job that reads 2 knows the check did not run rather than that
coverage fell.
"""

import argparse
import json
import os
import platform
import subprocess
import sys


def refuse(message):
    """The check could not be made: this is not a verdict about coverage.

    Exit 2 rather than 1, and the reason on stderr rather than the code alone:
    a run that never measured anything used to be indistinguishable from a
    floor that was crossed, which is how a refused check came to be read as a
    slide. The code is the one `scripts/ci.sh` uses for a mode that cannot run,
    so the gate and this check answer "not run" the same way.
    """
    print(message, file=sys.stderr)
    sys.exit(2)


def relative_to_root(filename, root):
    """The export names absolute paths; the baseline holds paths in the tree.

    An absolute path belongs to the machine it was measured on, so it cannot go
    into a file this repository commits, and it cannot be compared across two
    checkouts either.
    """
    marker = os.sep + "crates" + os.sep
    index = filename.find(marker)
    if index != -1:
        return filename[index + 1:]
    return os.path.relpath(filename, root)


def read_export(path, root):
    """{path relative to the root: uncovered lines} from an llvm-cov export."""
    with open(path) as handle:
        data = json.load(handle)["data"][0]
    files = {}
    counted = {"count": 0, "covered": 0}
    for entry in data["files"]:
        lines = entry["summary"]["lines"]
        files[relative_to_root(entry["filename"], root)] = lines["count"] - lines["covered"]
        counted["count"] += lines["count"]
        counted["covered"] += lines["covered"]
    # The export's own totals are over the same entries. Checked rather than
    # trusted, because the floor is read from one of the two and the files from
    # the other, and a parse that dropped an entry would raise the total quietly.
    totals = data.get("totals", {}).get("lines")
    if totals is not None and (totals["count"], totals["covered"]) != (
        counted["count"],
        counted["covered"],
    ):
        refuse(
            f"coverage-check: the export's total ({totals['count']} lines, "
            f"{totals['covered']} covered) is not the sum of its files "
            f"({counted['count']}, {counted['covered']}): refusing to check either"
        )
    return files, counted


def percent(counted):
    if counted["count"] == 0:
        return 0.0
    return 100.0 * counted["covered"] / counted["count"]


def commit(repo):
    try:
        return subprocess.run(
            ["git", "-C", repo, "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return "unknown"


def here():
    return f"{platform.system()} {platform.machine()}"


def update(baseline, files, counted, repo):
    measured = percent(counted)
    baseline["baseline"] = {
        "measured_on": subprocess.run(
            ["date", "-u", "+%Y-%m-%d"], capture_output=True, text=True, check=True
        ).stdout.strip(),
        "measured_commit": commit(repo),
        "measured_platform": here(),
        "measured_with": "cargo llvm-cov --workspace --json --summary-only",
        "total_lines": counted["count"],
        "total_uncovered": counted["count"] - counted["covered"],
        "total_lines_percent": round(measured, 2),
        "files": dict(sorted(files.items())),
    }
    return baseline


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--baseline", required=True, help="the checked-in baseline file")
    parser.add_argument("--report", required=True, help="an llvm-cov export json")
    parser.add_argument("--root", default=".", help="the workspace root the paths are relative to")
    parser.add_argument(
        "--update",
        action="store_true",
        help="re-measure the baseline from the report and write it back; the floor is left alone",
    )
    args = parser.parse_args()
    root = os.path.abspath(args.root)

    if not os.path.exists(args.report):
        refuse(f"coverage-check: {args.report} does not exist: nothing was measured to check")
    files, counted = read_export(args.report, root)

    if args.update:
        if not os.path.exists(args.baseline):
            refuse(
                f"coverage-check: {args.baseline} does not exist. Write its `what`, its `floor` "
                "(lines_percent, as_measured_percent, measured_on, measured_commit) and its "
                "`slack_lines` first: --update measures the per-file baseline and does not choose "
                "a floor."
            )
        with open(args.baseline) as handle:
            baseline = json.load(handle)
        baseline = update(baseline, files, counted, root)
        with open(args.baseline, "w") as handle:
            json.dump(baseline, handle, indent=2, sort_keys=False)
            handle.write("\n")
        print(
            f"coverage-check: wrote {args.baseline} from {counted['count']} lines, "
            f"{counted['count'] - counted['covered']} uncovered, {percent(counted):.2f}% "
            f"on {baseline['baseline']['measured_platform']}"
        )
        floor = baseline.get("floor", {}).get("lines_percent")
        if floor is not None:
            print(
                f"coverage-check: the floor is unchanged at {floor}%; raise it by hand when a "
                "change earns it"
            )
        return

    try:
        with open(args.baseline) as handle:
            baseline = json.load(handle)
    except FileNotFoundError:
        refuse(f"coverage-check: {args.baseline} does not exist: there is no floor to check")

    floor = baseline["floor"]
    slack = baseline["slack_lines"]
    expected = baseline["baseline"]
    measured_here = here()

    if measured_here.split()[0] != expected["measured_platform"].split()[0]:
        refuse(
            f"coverage-check: the baseline in {args.baseline} was measured on "
            f"{expected['measured_platform']}, and this run is on {measured_here}. The numbers are "
            "the compiler's and the platform backends are cfg-gated, so another operating system's "
            "export counts different lines. Re-measure it here with --update, and keep the floor "
            "for the platform the job runs on."
        )

    total = percent(counted)
    problems = []
    print(
        f"coverage-check: this run is {total:.2f}% of {counted['count']} lines "
        f"({counted['count'] - counted['covered']} uncovered) on {measured_here}"
    )
    print(
        f"coverage-check: the floor is {floor['lines_percent']}%, measured "
        f"{floor['as_measured_percent']}% on {floor['measured_on']} at "
        f"{floor['measured_commit'][:7]}"
    )
    print(
        f"coverage-check: {len(expected['files'])} file(s) in the baseline, "
        f"{len(set(files) - set(expected['files']))} file(s) in the export it does not name"
    )
    for path in sorted(expected["files"]):
        if path not in files:
            problems.append(
                f"{path} is in the baseline and not in the export: a rename, a split or a file "
                "that stopped being compiled needs --update, and a file that lost its last "
                "covered line is the slide this rule exists for, so the two are not told apart "
                "here"
            )
            continue
        was = expected["files"][path]
        now = files[path]
        print(f"  {path:<52} uncovered {now:>5}, baseline {was:>5}")
        if now > was + slack:
            problems.append(
                f"{path} has {now} uncovered lines, and its baseline is {was} (slack {slack}): "
                f"{now - was} more than it had"
            )
    for path in sorted(set(files) - set(expected["files"])):
        print(f"  {path:<52} uncovered {files[path]:>5}, not in the baseline")

    if total < floor["lines_percent"]:
        problems.insert(
            0,
            f"the workspace is at {total:.2f}% of lines and the floor is "
            f"{floor['lines_percent']}%: {floor['lines_percent'] - total:.2f} points under it",
        )

    if problems:
        for problem in problems:
            print(f"::error::coverage {problem}")
        print(
            "coverage-check: cover the lines, or re-measure deliberately "
            f"(python3 scripts/coverage-check.py --update --baseline {args.baseline} "
            f"--report {args.report}), in the change that crosses this"
        )
        sys.exit(1)
    print(
        f"coverage-check: the floor holds; every file the baseline names is at or above it, and "
        f"all {len(files)} file(s) in the export were compared"
    )


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        # Anything that stops the check short of a verdict is a refusal, and a
        # refusal must not wear the code a slide wears: without this, an input
        # that is not an export at all (or a baseline missing a key) left the
        # interpreter with a traceback and status 1, which is the answer "the
        # floor was crossed". The paths above print their own message; this is
        # the backstop for the ones nobody wrote.
        refuse(f"coverage-check: the check could not be made: {type(error).__name__}: {error}")
