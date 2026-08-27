//! Load-scaled deadlines for tests that wait on a real spawned process.
//!
//! A test that spawns a real process and waits a fixed wall-clock budget will
//! flake on a loaded machine, and the budget looks generous right up until it
//! is not (#307). The budget cannot be chosen from inside the test, because
//! the thing it must absorb — contention — is a property of how many *other*
//! tests are spawning processes at the same moment.
//!
//! So the number stops being a per-test guess: callers state the budget that
//! is right for their operation *in isolation*, and [`scaled`] widens it by
//! the parallelism the suite is actually running under.

use std::time::{Duration, Instant};

/// How many test threads this suite is running under.
///
/// `RUST_TEST_THREADS` is what CI pins and what a developer overrides, so it
/// wins; otherwise the harness defaults to the machine's parallelism.
fn test_threads() -> usize {
    std::env::var("RUST_TEST_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        })
}

/// Widen `base` by the suite's parallelism.
///
/// Serial runs keep `base` unchanged: with one test thread there is no
/// suite-induced contention to absorb, so scaling there would only slow a
/// failing test down. Above that the factor grows with the thread count but
/// is capped, because a 64-way machine does not make a shell 64 times slower
/// — past a point the bottleneck stops being CPU and an uncapped budget just
/// converts a real hang into a very long wait.
pub fn scaled(base: Duration) -> Duration {
    scaled_for(base, test_threads())
}

/// [`scaled`]'s arithmetic, as a pure function of the thread count.
///
/// Split out so the scaling policy is testable without mutating
/// `RUST_TEST_THREADS`: env vars are process-global, so a test that sets one
/// races every other test in the binary.
fn scaled_for(base: Duration, threads: usize) -> Duration {
    base * threads.clamp(1, MAX_SCALE) as u32
}

/// Ceiling on the scale factor. Four covers CI's pin and typical dev boxes.
const MAX_SCALE: usize = 4;

/// Poll `cond` until it returns true or the scaled budget expires.
///
/// Returns whether the condition was observed. Callers assert on the result
/// so the failure names the thing that never happened, not the timeout.
pub fn await_cond(base: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = scaled(base);
    let started = Instant::now();
    loop {
        if cond() {
            return true;
        }
        if started.elapsed() >= deadline {
            // Re-check once after the deadline: on a loaded machine the last
            // sleep can overshoot past a condition that became true during it,
            // which would report a timeout for work that actually completed.
            return cond();
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Gap between polls, matching the 20ms the hand-rolled loops used.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of #307: the budget must respond to load rather than
    /// being a constant a test author guessed.
    #[test]
    fn the_budget_widens_when_the_suite_runs_parallel() {
        assert_eq!(
            scaled_for(Duration::from_millis(1000), 4),
            Duration::from_millis(4000),
            "a 4-way suite must widen a 1s isolated budget, or the number is \
             still a per-test guess that flakes under contention"
        );
    }

    /// A serial run has no suite-induced contention, so widening there would
    /// only make a genuinely failing test take longer to say so.
    #[test]
    fn a_serial_run_keeps_the_isolated_budget() {
        assert_eq!(
            scaled_for(Duration::from_millis(1000), 1),
            Duration::from_millis(1000),
            "one test thread means nothing to absorb"
        );
    }

    /// Uncapped scaling turns a real hang into a wait nobody sits through.
    #[test]
    fn a_huge_thread_count_is_capped() {
        assert_eq!(
            scaled_for(Duration::from_millis(1000), 64),
            Duration::from_millis(MAX_SCALE as u64 * 1000),
            "past the cap the bottleneck is not CPU, and an uncapped budget \
             converts a hang into a very long wait"
        );
    }

    /// A condition already true must not pay the poll interval, or every
    /// converted call site gets slower for nothing.
    #[test]
    fn an_already_true_condition_returns_immediately() {
        let started = Instant::now();
        assert!(await_cond(Duration::from_secs(30), || true));
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "took {:?}: the condition was true on entry, so it must not sleep",
            started.elapsed()
        );
    }

    /// The budget must actually bound a condition that never comes true.
    #[test]
    fn a_condition_that_never_holds_times_out_and_reports_false() {
        let started = Instant::now();
        assert!(
            !await_cond(Duration::from_millis(100), || false),
            "a condition that never holds must report false, not hang"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "returned after {:?}: it must wait out the budget before giving up, \
             or it is not a deadline",
            started.elapsed()
        );
    }

    /// A condition that becomes true partway through must be observed.
    #[test]
    fn a_condition_that_turns_true_midway_is_observed() {
        let flip = Instant::now() + Duration::from_millis(60);
        assert!(
            await_cond(Duration::from_secs(5), || Instant::now() >= flip),
            "the condition turned true well inside the budget"
        );
    }
}
