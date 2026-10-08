# 0004. The log is capped, and the cap trims it in place

- **Status:** accepted
- **Date:** 2026-10-08
- **Deciders:** Guru (project owner, `guruor`)
- **Supersedes:** nothing

## Context

`docs/spec/state-and-cache.md` 1.1 said "The log, one file rotated at 1 MiB, three
kept" and nothing in the tree did it. The daemon neither opened the file nor
bounded it, so the file grew by one rotation's lines per rotation with no ceiling
at all: on this machine `~/Library/Logs/whirl/whirl.log` was 1,525,339 bytes
(1.45 MiB) after 9,738 lines and 851 rotations on 2026-10-08, and the only thing
that had ever bounded it was a reinstall. The configuration had no key for it
either, so there was no place a user could have read the policy.

The log is not the daemon's file to open. The macOS supervisor opens it as the
job's `StandardOutPath` and `StandardErrorPath`
(`crates/whirl-cli/src/daemon/macos.rs`), and launchd opens that descriptor with
`O_APPEND`; `launchd.plist` has no key that rotates, sizes or bounds it. An
`O_APPEND` descriptor holds the inode it was opened on, not the path, so the
obvious implementation, rotate the file and keep three, would leave the daemon
appending to the *renamed* file: the archive would grow, the live log would stay
empty, and every reader of the path would be reading the wrong file. Measured on
this machine in the two arrangements, against a 4096-byte cap: a descriptor that
does not append, truncated under a writer, leaves a file of 21,679 bytes holding
21,143 NUL bytes of padding, while a descriptor opened with `append(true)` ends at
2,104 bytes after 40 rotations and 4 trims, with no NUL byte anywhere in it.

Two measurements sized the cap's floor: the widest line the daemon writes on a
real install is 484 bytes (the `plan:` line, `docs/architecture.md` 4.1) and the
largest whole rotation block in that log is 5,905 bytes.

## Decision

Add `log_max_bytes` to the configuration, default 1 MiB, where `0` means keep
everything and is the only value that means that, and any other value must be at
least 4096. When the file is over the cap the daemon rewrites it in place, keeping
the newest whole lines, at startup and at the end of every rotation, and writes
one line saying how many bytes it dropped.

## Alternatives considered

- **Rotate and keep three files, as 1.1 said.** Lost on the measurement above:
  with an `O_APPEND` descriptor the daemon's own next line lands in the renamed
  file, so the live path stops receiving lines.
- **A `log_max_files`-style ceiling on several files.** Same defect: it is a
  rotation.
- **A cap applied by the supervisor or an external tool** (`newsyslog`,
  `logrotate`). Lost because launchd has no such key and because the cap would
  then be the machine's business rather than the daemon's: a user with no
  external tool would have no cap, which is the state this decision is fixing.
- **Refusing to start, or failing the rotation, when the trim fails.** Lost: the
  log is diagnostics. A log the daemon cannot bound is worse than one it can, and
  it is not a reason to stop rotating wallpaper.
- **A guarantee for a descriptor that does not append.** Not tried, because the
  standard library does not offer it: setting `O_APPEND` on an inherited
  descriptor needs `fcntl(F_SETFL)`, which is an `unsafe` block or a new
  dependency, and `Cargo.lock` names the four workspace members and nothing else.
  The cap therefore holds where a descriptor appends, which is the arrangement
  both supervisors use, and a `>` redirect that does not append is documented
  (`crates/whirld/src/log.rs`, `docs/spec/state-and-cache.md` 1.1) as the one
  setup it does not hold for.

## Consequences

- Easier: the log has a stated bound and a stated policy in the file the user
  edits, next to `log_level`. The oldest diagnostics go first, and the newest
  rotations are the ones a reader keeps, which is the direction that matters when
  something just went wrong.
- Harder: a follower that keeps the file open across a trim (`tail -f`) has the
  file rewritten underneath it. That is the same trade a rotation makes, and the
  cost of the alternative above.
- Forbidden: the log is one file with no rotations to find, so nothing may look
  for `whirl.log.1`. `0` is the only way to say keep everything, and a nonzero cap
  below 4096 is refused rather than read as an approximation of `0`.
- Enforced by `crates/whirld/src/log.rs`'s own tests (a trim keeps the newest whole
  lines, a zero cap never touches the file, an append after a trim lands at the
  new end and not in a NUL hole), by
  `repeated_failing_rotations_leave_the_log_at_its_cap_and_nothing_else_changed`
  in `crates/whirld/tests/control_socket.rs`, by
  `a_rotation_holds_the_log_to_the_cap_the_config_states` in
  `crates/whirld/src/state.rs`, and by the validation in
  `crates/whirl-core/src/config.rs`.
- Reversed by: a supervisor that rotates the file itself and hands the daemon a
  fresh descriptor per run. Then rotation is possible again and this decision
  should be superseded rather than edited.
