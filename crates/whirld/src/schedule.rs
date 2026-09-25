//! The daemon's clock: the rules of `docs/architecture.md` 5.5, as a decision
//! function that takes both clocks as parameters.
//!
//! Rule numbers below are 5.5's. The two the prototype got wrong are the reason
//! this module exists as a function rather than as a loop: rule 4 (`[M 14]`'s
//! drift, `[M 17]`'s retry storm) and rule 5 (`[D 6 §8.6]`'s clock jump).
//!
//! **Nothing here reads a clock.** `Observation` carries a wall-clock reading
//! and a monotonic one, both taken by the caller at the same instant, so every
//! rule is testable without sleeping and without moving the machine's clock.
//! The caller (`crate::scheduler`) is the only code that owns an `Instant`.
//!
//! **The order of the two tests is a decision.** A deadline that has passed goes
//! through rule 4 (rotate once, advance by whole intervals) even when the wall
//! clock moved a long way at the same time, and rule 5's re-anchor is reached
//! only when the deadline has *not* passed and the two clocks disagree. The
//! alternative order (drift first) cannot tell a machine that slept from a clock
//! that moved, and classifying a suspend as a jump means no rotation on wake,
//! which contradicts four separate statements: 5.5 rule 1 ("that is what makes
//! 'the machine slept through the slot' visible"), rule 3 and rule 4 ("ten
//! missed slots collapse into one rotation on wake"), `[D 6 §8.6]` ("a forward
//! jump produces one immediate rotation, which is fine"), and architecture
//! 10.12 ("the persisted `next_at` advances by whole intervals ... which is what
//! ... makes 'one rotation on wake, never a burst' structural"). What the drift
//! test is then left to catch is the case `[D 6 §8.6]` names as the dangerous
//! one: a *backward* jump, where "rotations stall silently for the length of the
//! jump and nothing in the log says why". That case is not due, so it is the one
//! rule 5 sees, and the re-anchor is only ever correct there. Recorded in the
//! handoff as a reading of two rules that overlap.

use std::time::Duration;

/// The bounded slice the scheduler sleeps between two observations.
///
/// 5.5 rule 3: "the daemon therefore sleeps in slices of a few seconds, uses the
/// monotonic clock for the slice length, and re-tests with rule 1's comparison".
/// The prototype's own slice was clamped to 1-20 s (`[M 18]`, `whd.rs:184-191`);
/// this is the middle of that range, and its exact value is not load-bearing:
/// every rule is re-tested on wake.
pub const SLICE: Duration = Duration::from_secs(5);

/// One reading of both clocks, taken at the same moment.
///
/// `wall` is the same integer `state/current.json` persists and 2.6 prints as an
/// RFC 3339 UTC timestamp. `monotonic` is elapsed time since some fixed origin
/// the caller owns (the daemon uses its own start, so the origin cannot move),
/// and it is what does *not* advance across suspend: that difference is what
/// makes rule 5's comparison meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    pub wall: i64,
    pub monotonic: Duration,
}

impl Observation {
    pub fn new(wall: i64, monotonic: Duration) -> Observation {
        Observation { wall, monotonic }
    }
}

/// What one tick decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    /// Not due yet: sleep this long and observe again (rule 3). It is never
    /// longer than `SLICE` and never negative.
    Wait(Duration),
    /// The deadline passed: rotate once, and `next_at` is the value to persist
    /// (rule 4's whole-interval advance, which is also rule 6's startup case).
    Due { next_at: i64 },
    /// The wall clock moved by more than two intervals against the monotonic
    /// clock *and* the deadline has not passed: announce it and re-anchor
    /// (rule 5). This is the backward-jump stall, the one case where
    /// re-anchoring is correct because the relationship between the deadline and
    /// reality is unknown.
    ClockJump { seconds: i64, next_at: i64 },
}

/// The whole-interval advance of rule 4, with the never-in-the-past guarantee:
/// `while next_at <= now { next_at += interval }`.
///
/// This is what makes a machine that slept through three slots rotate once and
/// return to the grid, and it is not `next_at = now + interval`: the original
/// deadline is kept as the grid.
///
/// `interval` is floored at 1 s so a zero interval cannot make this loop
/// forever. `docs/architecture.md` 4.3 refuses `schedule.interval_seconds < 60`
/// before this is ever called, so the floor is a guard and not a policy.
pub fn on_grid_after(next_at: i64, now: i64, interval_seconds: u64) -> i64 {
    let interval = interval_seconds.max(1) as i64;
    let mut value = next_at;
    while value <= now {
        value += interval;
    }
    value
}

/// `resume`'s re-arm (2.5, 5.5 rule 8): `next_at = now + interval`.
///
/// Not rule 4's advance: the user asked for a slot from now, not for the old
/// grid to be honoured.
pub fn rearmed_from(now: i64, interval_seconds: u64) -> i64 {
    now + interval_seconds.max(1) as i64
}

/// Compare two consecutive observations against the persisted deadline.
///
/// See the module comment for why the deadline test comes first.
pub fn tick(
    previous: Observation,
    current: Observation,
    next_at: i64,
    interval_seconds: u64,
) -> Tick {
    if current.wall >= next_at {
        return Tick::Due {
            next_at: on_grid_after(next_at, current.wall, interval_seconds),
        };
    }
    let wall_delta = current.wall - previous.wall;
    let monotonic_delta = current
        .monotonic
        .saturating_sub(previous.monotonic)
        .as_secs() as i64;
    // How far the wall clock ran away from real time between the two readings.
    // Saturating, so a jump large enough to overflow an `i64` is still a jump.
    let drift = wall_delta.saturating_sub(monotonic_delta);
    if drift.unsigned_abs() > 2 * interval_seconds {
        return Tick::ClockJump {
            // The wall clock moved by this much: 5.5 rule 5's "delta" and 2.9's
            // `clock_jump <seconds>` row. It is signed, and in this build it is
            // negative in practice, because the reachable case is the backward
            // jump (the module comment says why).
            seconds: wall_delta,
            next_at: rearmed_from(current.wall, interval_seconds),
        };
    }
    let remaining = (next_at - current.wall) as u64;
    Tick::Wait(Duration::from_secs(remaining).min(SLICE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(wall: i64, monotonic_seconds: u64) -> Observation {
        Observation::new(wall, Duration::from_secs(monotonic_seconds))
    }

    /// The ordinary case: five seconds of real time between two readings and a
    /// deadline most of an interval away. It waits, and it waits by the *slice*,
    /// not by the remaining time: a single sleep to the deadline is the shape
    /// rule 3 rejects, and 1790 s of it is the first assertion.
    ///
    /// An implementation that returned `Wait(remaining)`, or `Wait(ZERO)`, or a
    /// `Wait` of any other length, differs from `SLICE` in this comparison.
    #[test]
    fn a_deadline_in_the_future_waits_by_the_slice() {
        let tick = tick(
            observation(1_000_000, 0),
            observation(1_000_005, 5),
            1_001_795,
            1800,
        );
        assert_eq!(tick, Tick::Wait(SLICE));
        assert_eq!(
            tick,
            Tick::Wait(Duration::from_secs(5)),
            "the slice is 5 s, not the 1790 s left"
        );
    }

    /// A deadline inside the slice waits the time left, not the whole slice: the
    /// slice is a cap, not a step.
    #[test]
    fn a_deadline_inside_the_slice_waits_the_time_left() {
        assert_eq!(
            tick(observation(1_000, 0), observation(1_000, 0), 1_001, 1800),
            Tick::Wait(Duration::from_secs(1))
        );
    }

    /// Rule 1 is a wall-clock comparison, so a deadline exactly on `now` is due,
    /// and rule 4 then leaves it strictly in the future.
    #[test]
    fn the_deadline_fires_on_the_second_it_is_reached() {
        assert_eq!(
            tick(observation(1_000, 0), observation(1_000, 0), 1_000, 1800),
            Tick::Due { next_at: 2_800 }
        );
    }

    /// Rule 4's whole-interval advance, which is the shape the sleep case has: a
    /// machine that slept through three slots rotates once and returns to the
    /// grid, with `next_at` strictly in the future.
    ///
    /// Three assertions, each of which a plausible wrong implementation fails:
    /// `next_at = now + interval` (6_407 + 1800 = 8_207) fails the first,
    /// advancing by one interval only (2_800) fails it too, and a re-anchor that
    /// left the grid fails the modulo.
    #[test]
    fn three_missed_slots_advance_once_and_land_back_on_the_grid() {
        let grid = 1_000;
        let now = grid + 5_400 + 7;
        let tick = tick(observation(0, 0), observation(now, 0), grid, 1800);
        assert_eq!(tick, Tick::Due { next_at: 8_200 });
        let Tick::Due { next_at } = tick else {
            panic!("a missed slot is due: {tick:?}")
        };
        assert!(next_at > now, "never in the past: {next_at} <= {now}");
        assert_eq!((next_at - grid) % 1800, 0, "still on the grid");
        assert_ne!(
            next_at,
            now + 1800,
            "not re-anchored at now + interval (rule 4, [M 14])"
        );
    }

    /// A slot a few seconds late advances by one whole interval, so the schedule
    /// does not drift by the lateness of every rotation (`[M 14]`).
    #[test]
    fn a_late_slot_advances_by_one_interval_and_does_not_drift() {
        assert_eq!(
            tick(observation(0, 0), observation(1_812, 0), 1_800, 1800),
            Tick::Due { next_at: 3_600 },
            "1800 + 1800, not 1812 + 1800"
        );
    }

    /// A machine that slept through three slots: the wall clock moved 5,400 s
    /// while the monotonic clock did not move at all. It rotates **once**, on
    /// the grid, and no jump is announced. This is the case the module comment
    /// says rules 4 and 5 overlap on, and it is the behaviour 5.5 rule 4,
    /// `[D 6 §8.6]`, architecture 10.12 and 4.2's own config comment all
    /// require: "a laptop that slept through three slots rotates once on wake and
    /// then returns to the grid".
    ///
    /// A drift-first implementation answers `ClockJump { seconds: 5400, .. }`
    /// here and fails on the variant, not merely on a field.
    #[test]
    fn a_machine_that_slept_rotates_once_and_returns_to_the_grid() {
        let tick = tick(observation(1_000, 0), observation(6_400, 0), 1_800, 1800);
        assert_eq!(tick, Tick::Due { next_at: 7_200 });
        assert!(
            matches!(tick, Tick::Due { .. }),
            "one rotation on wake, never a burst and never a silent re-anchor: {tick:?}"
        );
    }

    /// 5.5 rule 5's case: the wall clock jumped **backward** by nearly two hours
    /// while five seconds of real time passed, and the deadline is now far in the
    /// future on that clock. This is the stall `[D 6 §8.6]` names, so it is
    /// announced with the signed wall delta and re-anchored from the wall clock.
    ///
    /// A positive-only threshold (`drift > 2 * interval` rather than
    /// `abs`) leaves this as a `Wait` and stalls the schedule; the assertion
    /// fails on the variant.
    #[test]
    fn a_backward_clock_jump_is_announced_and_re_anchored() {
        let tick = tick(
            observation(1_000_000, 0),
            observation(993_205, 5),
            1_000_100,
            1800,
        );
        assert_eq!(
            tick,
            Tick::ClockJump {
                seconds: -6_795,
                next_at: 995_005
            }
        );
    }

    /// A small backward move is not a jump and not a stall: the deadline is
    /// still ahead and the daemon waits. 100 s is inside two intervals.
    #[test]
    fn a_small_backward_move_is_not_a_jump() {
        assert_eq!(
            tick(
                observation(1_000_000, 0),
                observation(999_900, 5),
                1_000_500,
                1800
            ),
            Tick::Wait(SLICE)
        );
    }

    /// The boundary is `2 * interval`, exclusive. Both readings here are *before*
    /// the deadline, so rule 5 is the rule in force: a drift of exactly two
    /// intervals waits, one second more is announced. A `<` where `<=` belongs,
    /// or a threshold of one interval, lands on one of the two variants.
    #[test]
    fn exactly_two_intervals_of_drift_is_not_a_jump() {
        let interval = 1800;
        let at_two = tick(
            observation(1_000_000, 0),
            observation(1_003_605, 5),
            1_003_700,
            interval,
        );
        assert_eq!(
            at_two,
            Tick::Wait(SLICE),
            "two intervals of drift is not a jump"
        );
        let past_two = tick(
            observation(1_000_000, 0),
            observation(1_003_606, 5),
            1_003_700,
            interval,
        );
        assert_eq!(
            past_two,
            Tick::ClockJump {
                seconds: 3_606,
                next_at: 1_005_406
            },
            "one second beyond two intervals is a jump"
        );
    }

    /// Rule 6: on start the daemon re-derives rather than re-schedules. The two
    /// observations are the same instant (the first tick of a fresh run), the
    /// persisted deadline is in the past, and the answer is one rotation whose
    /// new deadline is in the future.
    #[test]
    fn a_persisted_deadline_in_the_past_rotates_once_at_startup() {
        assert_eq!(
            tick(observation(5_000, 0), observation(5_000, 0), 4_000, 1800),
            Tick::Due { next_at: 5_800 }
        );
    }

    /// A zero interval cannot spin: the guard floors it at one second, so the
    /// advance terminates and the deadline still moves forward. 4.3 refuses the
    /// config this could come from, so this is the belt to that pair of braces.
    #[test]
    fn a_zero_interval_still_advances_and_terminates() {
        assert_eq!(on_grid_after(1_000, 1_000, 0), 1_001);
        assert_eq!(rearmed_from(1_000, 0), 1_001);
    }
}
