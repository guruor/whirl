# 0005. The prefetch warms the candidates the next rotation would set

- **Status:** accepted
- **Date:** 2026-10-09
- **Deciders:** Guru (project owner, `guruor`)
- **Supersedes:** nothing

## Context

A rotation set exactly one candidate and stopped. The selection of
`docs/architecture.md` 4.1 already knows the candidates it would offer next - the
recent window and the index name them - but nothing fetched them, so the rotation
after this one paid for its own download from the source. Measured by
`the_prefetch_timings_with_and_without_the_prefetch` in
`crates/whirl-worker/src/pipeline.rs`, against a transport that costs 150 ms an
open: on 2026-10-09, with the prefetch off, the rotation that set from the cache
cost 282 ms; with the prefetch on, the same rotation cost 4 ms.

What the walk needs is a bound. Every candidate warmed is one more download inside
the rotation's own `schedule.worker_deadline_seconds`, one more line on the
worker's stdout (which the daemon reads only after the worker exits), and one more
image held against `cache.max_bytes` until the sweep reclaims it. The deadline is
not negotiable: `docs/architecture.md` 1.7.1 has the daemon kill the worker when
the deadline passes and never read what it wrote, so a prefetch that ran into it
would lose the `set:` line the rotation had already printed and be recorded as
`worker_timeout` instead.

This decides `docs/architecture.md` 1.6 (the worker contract and the shape of its
stdout), 1.7.1 (the deadline the bound is half of), 4.2 (the annotated config
example) and 4.3 (validation), and `docs/spec/state-and-cache.md` 7.3 (what the
daemon records once the worker has exited). It changes sections 1 and 4 of the
architecture, which `docs/development.md` section 6 names as a change that needs
an ADR.

## Decision

Add `prefetch` to the configuration: how many candidates after the one the
rotation set the worker fetches and stores before it exits. It defaults to `2`,
`0` turns it off and is the only value that means that, and a value above
`PREFETCH_MAX` (8) is refused rather than clamped. The worker prints one
`prefetch: <digest> <origin_key> <abs path>` line per candidate it warmed, after
the `set:` line, and spends at most half of `schedule.worker_deadline_seconds` on
them.

## Alternatives considered

- **Clamp a value above the ceiling instead of refusing it.** Lost for the reason
  `sources[0].pages` and `log_max_bytes` already refuse rather than clamp (4.3): a
  config that silently means something other than what it says is worse than one
  that is refused.
- **No ceiling at all.** Lost: the ceiling is a fact about what a rotation can
  afford, not taste. An unbounded `prefetch` puts an unbounded number of downloads
  inside one `worker_deadline_seconds`, and 1.7.1's kill would then discard a
  `set:` line the rotation had already printed.
- **Fetch the next candidates before the set, so the set waits for them.** Lost:
  that would hold the wallpaper change the user asked for behind a speculative
  fetch. The set is the point of the rotation and the prefetch is not, which is
  why the first prefetch byte is read only after the report is on stdout; measured
  above, the set itself is unmoved (304 ms against 300 ms) while the whole
  rotation grows (899 ms against 300 ms).
- **A second walk owned by the daemon, or its own timer.** Lost: the daemon does
  not download - the worker does - and the rotation already holds the selection
  cursor, so a second walk would either duplicate it or need a cursor of its own.
  Not tried, for that reason.

## Consequences

- Easier: the rotations after a warming one set from the cache with no request at
  all. Measured by the timing test above: the second rotation costs 4 ms against
  282 ms, with zero fetches against one.
- Harder: a warming rotation costs more wall time, 899 ms against 300 ms in the
  same test, and the daemon's reply to `next` waits for the worker to exit, so
  that added time is inside the rotation's completion even though the wallpaper
  was already set. The `set:` line is the result wherever it sits in the capture,
  so the daemon records the rotation from it and not from the last line.
- The `prefetch:` line is the shape a human running the worker by hand reads. It
  is not the daemon's log surface for what was warmed: that is
  `whirld: prefetched <origin_key> <digest> <bytes> bytes`, one line per entry,
  written when the index entry is recorded (`crates/whirld/src/state.rs`).
- Forbidden: a value above 8 is refused, not clamped, and `0` is the only way to
  say "fetch nothing ahead". A prefetch may not fail the rotation that triggered
  it, and may not delay the `set:` line.
- Enforced by the validation in `crates/whirl-core/src/config.rs` (the `prefetch`
  row in `wrong_types_and_out_of_range_values_are_refused_with_their_line` and
  `the_prefetch_ceiling_accepts_its_edges_and_refuses_above`), by
  `the_set_happens_before_the_prefetch_starts`,
  `a_prefetch_of_zero_warms_nothing`,
  `a_deadline_with_no_room_stops_the_prefetch_before_it_fetches`,
  `a_prefetch_whose_fetch_fails_leaves_the_rotation_green_and_warms_nothing`,
  `a_prefetch_that_meets_a_source_it_cannot_ask_keeps_walking`,
  `a_prefetch_does_not_fetch_a_candidate_the_index_already_serves` and
  `the_prefetch_timings_with_and_without_the_prefetch` in
  `crates/whirl-worker/src/pipeline.rs`, by
  `the_prefetch_lines_follow_the_set_line_and_are_not_the_result` in
  `crates/whirld/src/worker.rs`, and by
  `a_warmed_rotation_serves_the_next_two_without_reading_the_source` in
  `crates/whirld/tests/control_socket.rs`.
- Reversed by: a daemon that fetches ahead on its own clock, or a worker contract
  that reports its result by a means other than the `set:` line. Then this
  decision is superseded rather than edited.
