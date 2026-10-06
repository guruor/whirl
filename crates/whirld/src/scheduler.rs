//! The daemon's scheduler: the rules of `crate::schedule` driven by the two real
//! clocks, and the rotation a due slot runs (docs/architecture.md 5.5, 1.7).
//!
//! This is the only code in the daemon that owns an `Instant`, and it is
//! deliberately thin: every decision it makes belongs to `crate::schedule`,
//! which takes both clocks as arguments, so the rules are tested on values and
//! not by waiting. What is left here is the loop, the thread, and the three
//! things a due slot does in order: spend the slot (5.5 rule 4, before the
//! worker, because a crash mid-rotation leaves the slot consumed, 1.7.2), log
//! the plan line of 2.6, and run one rotation through `Daemon::rotation` -
//! the same function a client's `next` runs, so the two cannot answer
//! differently.
//!
//! A rotation started from here has no client, so its outcome goes to the log:
//! success and failure are both one line, and the `rotate_*` events of 2.9 go to
//! the subscribe stream as usual.

use crate::schedule::{Observation, SLICE, Tick};
use crate::state::{Daemon, Rotation};
use crate::worker::Verb;
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use whirl_core::protocol::Via;

/// Run the schedule until the process ends. `daemon` is shared with the accept
/// loop, and the lock is never held across a worker (1.8).
///
/// The first observation happens immediately rather than after one slice, which
/// is 5.5 rule 6: a restart re-derives the deadline and, if it is already in the
/// past, rotates once. Nothing here is a sleep *to* a deadline, so a suspend, a
/// wall-clock jump and an ordinary late slot are all re-tested rather than
/// assumed.
pub fn run(daemon: Arc<Daemon>, started: Instant) {
    let mut previous = observe(started);
    loop {
        // 4.2 and 10.5: the interval is read every pass rather than captured
        // once, so the re-read the last rotation did takes effect on the next
        // slot instead of at the next daemon start.
        let interval = daemon.effective.config().schedule.interval_seconds;
        let current = observe(started);
        step(&daemon, previous, current, interval);
        previous = current;
        thread::sleep(SLICE);
    }
}

/// One reading of both clocks. `started` is the daemon's own origin, so the
/// monotonic reading cannot move backwards and its length is the real elapsed
/// time, which is what 5.5 rules 2 and 5 need.
fn observe(started: Instant) -> Observation {
    Observation::new(crate::events::unix_seconds(), started.elapsed())
}

fn step(daemon: &Arc<Daemon>, previous: Observation, current: Observation, interval: u64) {
    let (paused, next_at) = {
        let state = daemon.state();
        (state.paused, state.next_at)
    };
    // 5.5 rule 8, 2.5: while paused "no rotation starts and `next_at` is not
    // advanced". `previous` is still carried forward by the caller, so the pause
    // itself is not mistaken for a jump when it ends, and `resume` re-arms the
    // deadline from its own wall clock.
    if paused {
        return;
    }
    // No persisted deadline and not paused cannot outlive `Daemon::load`, which
    // arms one; if a state file ever produced it, waiting is the safe reading of
    // `next_in_s: -`.
    let Some(next_at) = next_at else {
        return;
    };
    match crate::schedule::tick(previous, current, next_at, interval) {
        Tick::Wait(_) => {}
        Tick::ClockJump { seconds, next_at } => daemon.record_clock_jump(seconds, next_at),
        Tick::Due { next_at } => {
            // The slot is spent whether or not the rotation inside it succeeds,
            // and before the worker is spawned: this is what makes ten missed
            // slots collapse into one rotation, and what keeps a refused
            // rotation (a client already rotating) from retrying every slice.
            daemon.spend_slot(next_at);
            rotate(daemon);
        }
    }
}

/// One rotation in a due slot: claim it, record the plan, run it, log the
/// outcome.
fn rotate(daemon: &Arc<Daemon>) {
    let run = match daemon.start_rotation() {
        Ok(run) => run,
        Err(running) => {
            // 1.8: a rotation already in flight is refused, never queued. The
            // slot has already been spent, so the next one is a whole interval
            // away: this line is the whole cost, not a retry loop.
            eprintln!("whirld: rotation skipped: run {running} is in flight");
            return;
        }
    };
    // 2.6's plan line, once, at the start of the rotation it describes: the same
    // line `whirl config check` prints, from the same implementation, which is
    // what makes "what did the daemon actually adopt" answerable from the log.
    eprintln!("whirld: {}", daemon.plan_line());
    eprintln!(
        "whirld: rotation {run} starting: verb rotate via {}",
        Via::Source.as_str()
    );
    match daemon.rotation(run, Via::Source, Verb::Rotate, None) {
        Rotation::Set(record) => eprintln!(
            "whirld: rotation {run} ok: set {} {} {} {}",
            record.digest,
            record.origin_key,
            record.via.as_str(),
            record.path.as_deref().unwrap_or("-"),
        ),
        Rotation::Failed { code, message } => {
            eprintln!("whirld: rotation {run} failed: {} {message}", code.as_str())
        }
    }
}

#[cfg(test)]
mod tests {
    //! The scheduler's four real decisions, on values rather than by waiting:
    //! a due slot (5.5 rule 4), an interval the schema cannot store, a rotation
    //! the worker refuses, and a deadline that crossed a restart (5.5 rule 6).
    //!
    //! `step`, `observe` and `rotate` are what `run` is made of. `run` itself is
    //! an accept loop's twin: it never returns, so the only way to execute it is
    //! a thread that outlives the test, which no test here may do. Every decision
    //! inside it is exercised here instead.
    use super::*;
    use crate::schedule::{on_grid_after, rearmed_from};
    use crate::testkit::{
        Scratch, daemon, daemon_with_a_set, daemon_without_a_worker, digest, script,
    };
    use std::time::Duration;
    use whirl_core::config::Config;
    use whirl_core::protocol::ErrorCode;

    const INTERVAL: u64 = 1800;
    const ORIGIN_KEY: &str = "pictures:one";
    const CACHED_PATH: &str = "/cache/sha256/aa/aa/aa.jpg";

    /// One reading of both clocks, taken at one instant.
    fn observation(wall: i64, monotonic_seconds: u64) -> Observation {
        Observation::new(wall, Duration::from_secs(monotonic_seconds))
    }

    /// A daemon whose worker reports a successful `set:` of `seed`'s digest.
    fn due_daemon(scratch: &Scratch, name: &str, seed: char) -> (Arc<Daemon>, String) {
        let _ = name;
        let digest = digest(seed);
        let daemon = daemon_with_a_set(
            scratch.path(),
            Config::default(),
            &digest,
            ORIGIN_KEY,
            CACHED_PATH,
        );
        (Arc::new(daemon), digest)
    }

    /// `observe` is the one place the daemon's own `Instant` is read, and it has
    /// to read both clocks at the same moment: the wall reading is what `tick`
    /// compares against the persisted deadline and the monotonic reading is what
    /// it compares against the wall one, so a reading taken from a second `now()`
    /// would make the drift test measure this function's own delay.
    #[test]
    fn an_observation_reads_the_wall_clock_and_the_daemons_own_origin() {
        let started = Instant::now();
        let before = crate::events::unix_seconds();
        let observed = observe(started);
        let after = crate::events::unix_seconds();
        assert!(
            (before..=after).contains(&observed.wall),
            "the wall reading is now: {observed:?}"
        );
        assert!(
            observed.monotonic < Duration::from_secs(60),
            "the monotonic reading is this daemon's own elapsed time: {observed:?}"
        );
    }

    /// 5.5 rule 4 and 1.7.2: a deadline that has passed spends the slot, advances
    /// it by whole intervals, and runs exactly one rotation through
    /// `Daemon::rotation` — the same function a client's `next` runs.
    #[test]
    fn a_due_slot_rotates_once_and_spends_the_slot() {
        let scratch = Scratch::new("sched-due");
        let (daemon, digest) = due_daemon(&scratch, "due", 'a');
        let now = crate::events::unix_seconds();
        let due = now - 10;
        daemon.state().next_at = Some(due);

        step(
            &daemon,
            observation(now - 5, 5),
            observation(now, 10),
            INTERVAL,
        );

        let state = daemon.state();
        assert_eq!(state.rotation_count, 1, "one rotation, not a burst");
        assert!(state.running.is_none(), "the rotation ended");
        assert_eq!(state.last_error, None, "the worker reported a set");
        assert_eq!(
            state.next_at,
            Some(on_grid_after(due, now, INTERVAL)),
            "rule 4's whole-interval advance"
        );
        assert!(
            state.next_at.expect("a deadline") > now,
            "never in the past: {:?}",
            state.next_at
        );
        assert_eq!(
            state
                .history
                .iter()
                .next()
                .map(|entry| entry.digest.clone()),
            Some(Some(digest)),
            "the rotation's own set is in history"
        );
        assert_eq!(
            state
                .current
                .as_ref()
                .map(|current| current.origin_key.clone()),
            Some(Some(ORIGIN_KEY.to_string())),
            "the anchor moved to what the worker set"
        );
    }

    /// 2.5 and 5.5 rule 8: while paused "no rotation starts and `next_at` is not
    /// advanced". Deleting the `if paused { return; }` guard rotates here and
    /// fails the first assertion.
    #[test]
    fn a_paused_schedule_does_not_rotate_and_does_not_move_the_deadline() {
        let scratch = Scratch::new("sched-paused");
        let (daemon, _) = due_daemon(&scratch, "paused", 'b');
        let now = crate::events::unix_seconds();
        let due = now - 10;
        {
            let mut state = daemon.state();
            state.next_at = Some(due);
            state.paused = true;
        }

        step(
            &daemon,
            observation(now - 5, 5),
            observation(now, 10),
            INTERVAL,
        );

        let state = daemon.state();
        assert_eq!(
            state.rotation_count, 0,
            "a suspended schedule does not rotate"
        );
        assert_eq!(
            state.next_at,
            Some(due),
            "2.5: the deadline is frozen while paused"
        );
    }

    /// The third state of 2.10's `next_at`: no persisted deadline. `Daemon::load`
    /// arms one, so this is only reachable from a state file that produced the
    /// value after a load, and waiting is the safe reading of `next_in_s: -`.
    #[test]
    fn a_daemon_with_no_persisted_deadline_waits_rather_than_guessing() {
        let scratch = Scratch::new("sched-armed");
        let (daemon, _) = due_daemon(&scratch, "armed", 'c');
        let now = crate::events::unix_seconds();
        daemon.state().next_at = None;

        step(
            &daemon,
            observation(now - 5, 5),
            observation(now, 10),
            INTERVAL,
        );

        let state = daemon.state();
        assert_eq!(state.rotation_count, 0);
        assert_eq!(state.next_at, None, "nothing invented a deadline");
    }

    /// 5.5 rule 5's reachable case: the wall clock moved backward by more than
    /// two intervals while the deadline is still ahead, so the deadline stalls
    /// and is re-anchored from the wall clock. A drift test that only looked for
    /// a positive jump leaves this a `Wait` and the schedule stalled.
    #[test]
    fn a_backward_clock_jump_is_counted_and_re_anchors_the_deadline() {
        let scratch = Scratch::new("sched-jump");
        let (daemon, _) = due_daemon(&scratch, "jump", 'd');
        let now = 1_000_000;
        let jumped_to = now - 6_795;
        daemon.state().next_at = Some(now + 100);

        step(
            &daemon,
            observation(now, 0),
            observation(jumped_to, 5),
            INTERVAL,
        );

        let state = daemon.state();
        assert_eq!(state.clock_jump, 1, "2.10's `clock_jump` row");
        assert_eq!(
            state.next_at,
            Some(rearmed_from(jumped_to, INTERVAL)),
            "re-anchored from the wall clock, not pooled with the old grid"
        );
        assert_eq!(state.rotation_count, 0, "a jump is not a rotation");
    }

    /// 4.3 refuses `schedule.interval_seconds < 60` before `tick` is ever called,
    /// so zero is a value this daemon cannot read out of its own config. The
    /// floor in `on_grid_after` is the belt to that pair of braces, and what it
    /// has to guarantee is that a due slot still advances: one rotation, a
    /// deadline strictly in the future, and no spin.
    ///
    /// Deleting `.max(1)` from `on_grid_after` makes this loop forever, which is
    /// the defect the assertion below is written against.
    #[test]
    fn an_interval_the_config_schema_refuses_still_rotates_once_and_terminates() {
        let scratch = Scratch::new("sched-zero");
        let (daemon, _) = due_daemon(&scratch, "zero", 'e');
        let now = crate::events::unix_seconds();
        let due = now - 10;
        daemon.state().next_at = Some(due);

        step(&daemon, observation(now, 0), observation(now, 0), 0);

        let state = daemon.state();
        assert_eq!(state.rotation_count, 1);
        assert_eq!(state.next_at, Some(now + 1), "the floored advance");
        assert!(state.next_at.expect("a deadline") > now);
    }

    /// 1.8: a rotation already in flight is refused, never queued, and the slot
    /// has already been spent, so the next opportunity is a whole interval away.
    /// This is the whole cost of the refusal rather than a retry loop.
    #[test]
    fn a_rotation_already_in_flight_is_refused_and_the_slot_is_spent_anyway() {
        let scratch = Scratch::new("sched-busy");
        let (daemon, _) = due_daemon(&scratch, "busy", 'f');
        let in_flight = daemon.start_rotation().expect("the first claim");
        let now = crate::events::unix_seconds();
        let due = now - 10;
        daemon.state().next_at = Some(due);

        step(
            &daemon,
            observation(now - 5, 5),
            observation(now, 10),
            INTERVAL,
        );

        let state = daemon.state();
        assert_eq!(state.running, Some(in_flight), "refused, never queued");
        assert_eq!(state.rotation_count, 0, "nothing rotated");
        assert_eq!(
            state.next_at,
            Some(on_grid_after(due, now, INTERVAL)),
            "1.7.2: the slot is consumed whether or not the rotation ran"
        );
    }

    /// A verb the worker does not offer, in the only spelling this daemon can
    /// receive one: a worker that exits non-zero and names a code of 2.7. The
    /// daemon records the failure, clears the in-flight claim, and spends the
    /// slot exactly as it does for a success.
    #[test]
    fn a_rotation_the_worker_refuses_is_recorded_and_the_slot_is_still_spent() {
        let scratch = Scratch::new("sched-refused");
        let program = script(
            scratch.path(),
            "worker-refuse.sh",
            "#!/bin/sh\nprintf '%s\\n' 'stage=download code=offline message=no network' >&2\nexit 1\n",
        );
        let daemon = Arc::new(daemon(scratch.path(), Config::default(), program));
        let now = crate::events::unix_seconds();
        let due = now - 10;
        daemon.state().next_at = Some(due);

        step(
            &daemon,
            observation(now - 5, 5),
            observation(now, 10),
            INTERVAL,
        );

        let state = daemon.state();
        assert_eq!(state.rotation_count, 0, "a refusal is not a rotation");
        assert_eq!(
            state.last_error,
            Some(ErrorCode::Offline),
            "1.6's `code=` on stderr is the code the daemon reports"
        );
        assert!(state.running.is_none(), "the failure cleared the claim");
        assert!(
            state.next_at.expect("a deadline") > now,
            "the slot is spent whether or not the rotation succeeded"
        );
    }

    /// The other refusal: the worker program is not there at all. The daemon
    /// reports it as a failed rotation and clears the claim rather than
    /// unwinding, because a rotation nobody can run is still a spent slot.
    #[test]
    fn a_worker_that_cannot_be_spawned_is_a_failed_rotation_and_not_a_panic() {
        let scratch = Scratch::new("sched-nospawn");
        let daemon = Arc::new(daemon_without_a_worker(scratch.path(), Config::default()));
        let now = crate::events::unix_seconds();
        daemon.state().next_at = Some(now - 10);

        step(
            &daemon,
            observation(now - 5, 5),
            observation(now, 10),
            INTERVAL,
        );

        let state = daemon.state();
        assert_eq!(state.last_error, Some(ErrorCode::WorkerFailed));
        assert!(state.running.is_none());
        assert_eq!(state.rotation_count, 0);
    }

    /// 5.5 rule 6 and 10.12: a deadline that crossed a restart is re-derived
    /// rather than re-scheduled. The daemon that persisted it is gone, a second
    /// daemon reads the same directory, and its first observation — taken
    /// immediately, not after one slice — rotates once and lands back on the
    /// original grid. "One rotation on wake, never a burst" is the assertion:
    /// `next_at = now + interval` would pass the `> now` check and fail the
    /// grid check, and rotating per missed slot would fail the count.
    #[test]
    fn a_deadline_that_crossed_a_restart_rotates_once_and_returns_to_the_grid() {
        let scratch = Scratch::new("sched-restart");
        let digest = digest('9');
        // The first daemon leaves a deadline the machine slept through: three
        // slots in the past. `spend_slot` is the write the scheduler itself uses,
        // so the value under test is a value this daemon really can persist.
        {
            let first = daemon_with_a_set(
                scratch.path(),
                Config::default(),
                &digest,
                ORIGIN_KEY,
                CACHED_PATH,
            );
            first.spend_slot(crate::events::unix_seconds() - 3 * INTERVAL as i64);
        }

        // The restart. `Daemon::load` arms a deadline only when there is none, so
        // the past value survives, which is what rule 6's case is made of.
        let daemon = Arc::new(daemon_with_a_set(
            scratch.path(),
            Config::default(),
            &digest,
            ORIGIN_KEY,
            CACHED_PATH,
        ));
        let now = crate::events::unix_seconds();
        let persisted = daemon.state().next_at.expect("the persisted deadline");

        step(&daemon, observation(now, 0), observation(now, 0), INTERVAL);

        let state = daemon.state();
        let next_at = state.next_at.expect("a deadline");
        assert_eq!(
            state.rotation_count, 1,
            "one rotation on wake, never a burst"
        );
        assert!(next_at > now, "never in the past: {next_at} <= {now}");
        assert_eq!(
            (next_at - persisted) % INTERVAL as i64,
            0,
            "still on the grid the persisted deadline defined"
        );
        assert_eq!(
            state
                .history
                .iter()
                .next()
                .map(|entry| entry.digest.clone()),
            Some(Some(digest)),
            "the rotation ran a real worker"
        );
    }
}
