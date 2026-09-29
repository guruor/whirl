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
    let interval = daemon.effective.config.schedule.interval_seconds;
    let mut previous = observe(started);
    loop {
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
