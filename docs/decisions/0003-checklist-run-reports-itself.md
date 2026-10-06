# 0003. A checklist run reports itself through the release binary

- **Status:** accepted
- **Date:** 2026-10-06
- **Deciders:** the project owner
- **Supersedes:** nothing

## Context

A release is blocked until each platform's checklist has been run on real
hardware CI cannot reach (`docs/development.md` section 3, "What CI cannot
prove"), and the result is a row in `docs/releases/vX.Y.Z.md`. Measured, from
`docs/releases/v0.1.0.md`: the macOS row is several hundred words with 13 probe
results, 7 items named as not run, and a correction to one probe's own harness.
Every number in it was typed by hand from a run whose evidence already existed on
disk. The person who runs the checklist is on the machine under test, which has
the release archive and nothing else: no checkout, no toolchain, no interpreter.

`docs/architecture.md` 2.5.1 says no protocol verb exists only for the CLI and no
CLI verb needs a protocol verb of its own, and it records one command that is
deliberately not in its verb table: `whirl daemon
install|uninstall|start|stop|status`, which speaks to the platform's supervisor.
A second such command changes that sentence, and that is a change to an item in
section 2.

## Decision

`whirl report` is a second command that is not in the protocol's verb table and
adds no protocol verb: it ships in the release binary, takes one checklist run's
observations and the run's own facts as flags, and writes two files the caller
names, a machine-readable report and the platform's section of
`docs/releases/vX.Y.Z.md` as `.github/release-notes-template.md` requires it. Its
verdicts come from the run, an item the run did not exercise is reported as not
run, and a field it has no value for is a refusal with a message rather than a
blank or a plausible default. It takes its exit codes from 2.5.1's set and adds
none: 0 the run completed and both files were written, 1 the run could not be
made, 3 the command line was wrong.

## Alternatives considered

- **A script under `scripts/`, or a separate tool.** Needs an interpreter on the
  machine under test, which is the machine with the release archive and nothing
  else, and it would be a fourth artefact for the notes to describe. Lost.
- **A CI job that runs the probes.** CI cannot reach these machines at all
  (`docs/development.md` section 3), and a probe that sets a wallpaper may not
  run where the gate runs (`docs/development.md` section 7). Lost.
- **Emitting the report and leaving the row to a person.** The transcription is
  the defect being removed: the row and the report would be free to disagree, and
  one of them would be typed. Lost.
- **A protocol verb, with the daemon writing the file.** The act is a file write
  on the tester's machine, with no setter involved and no resident service to
  justify; it would also break 2.5.1's rule for nothing. Lost.
- **Reading the run's facts from the environment or a config.** The report names
  the machine and the person, and a value read from the environment can be wrong
  in a way no reader can see. Every input is a flag instead. Lost.

## Consequences

- The row for each platform is one command on the machine that ran it, and the
  machine-readable report comes from the same invocation, so the two cannot
  disagree.
- 2.5.1's "one command is deliberately not in the table above" becomes two, and
  `docs/architecture.md` 2.5.1 names both.
- Forbidden from now on: inventing a verdict, a timestamp, a machine name or a
  log digest; reporting an item with no evidence as a pass; writing over an
  existing report.
- Not decided here: the emitter does not run a probe. The person on the machine
  under test supplies the per-item verdicts and their evidence, and no test in
  this repository may set a real wallpaper (`docs/development.md` section 7).
- Reversing this needs the machine under test to have a checkout and a toolchain,
  which is the state the decision exists to leave behind.
- Enforced by `crates/whirl-cli/src/report.rs` and
  `crates/whirl-cli/tests/report.rs`, and by the release workflow's
  "Assemble and check the release notes" step, which still refuses a notes file
  that keeps a `<...>` placeholder.
