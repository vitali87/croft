//! Deadlines for tests that wait on a spawned process (#307).
//!
//! A test that spawns a real process and waits a fixed wall-clock budget will
//! flake on a loaded machine, and the budget looks generous right up until it
//! isn't. The reason per-test tuning keeps looking reasonable is that each
//! number is chosen against the operation it names - a shell printing a line,
//! a PTY delivering a byte - while what actually blows the budget is
//! CONTENTION, which is not a property of any test. It is a property of how
//! many other tests, and other builds, are spawning in the same moment. The
//! right number is therefore not knowable from inside a single test, which is
//! why raising individual caps has already been tried twice on one of them.
//!
//! So the budget stops being a guess and becomes `base x scale`, where `base`
//! is what the operation costs on a quiet machine (the number a test author
//! can actually reason about) and `scale` is measured from the load the suite
//! is running under.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Widest a budget may stretch. A failing test must still fail: past this the
/// wait is no longer distinguishing "loaded" from "broken", and every extra
/// second is paid by the developer whose change genuinely broke the thing.
const MAX_SCALE: u32 = 8;

/// Narrowest it may shrink. Never below the budgets these tests already had,
/// since every one of them was observed failing at 1x.
const MIN_SCALE: u32 = 2;

/// One-minute load average, where the platform reports one.
fn load_average() -> Option<f64> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut avg = [0f64; 1];
        // SAFETY: getloadavg writes at most `nelem` doubles into the buffer;
        // it is given a one-element buffer and asked for one sample.
        let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 1) };
        return (n == 1).then_some(avg[0]);
    }
    #[allow(unreachable_code)]
    None
}

/// How much slack this machine needs, resolved once per test binary.
///
/// Two signals, and the maximum of them, because each is blind where the
/// other sees. The suite's own thread count is known before anything runs
/// and covers the self-inflicted case (dozens of test shells at once), where
/// a load average would still be reading the quiet minute before the suite
/// started. The load average covers what no test can know: a second cargo
/// build, another suite, whatever else owns the machine.
pub fn load_scale() -> u32 {
    static SCALE: OnceLock<u32> = OnceLock::new();
    *SCALE.get_or_init(|| {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f64;
        let threads = std::env::var("RUST_TEST_THREADS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(cpus);
        let oversubscribed = load_average().map(|l| l / cpus).unwrap_or(1.0);
        scale_from(threads, oversubscribed)
    })
}

/// The rule itself, split out so it can be tested without a machine in a
/// particular state.
fn scale_from(threads: f64, oversubscription: f64) -> u32 {
    let raw = threads.max(oversubscription).ceil();
    if !raw.is_finite() {
        return MIN_SCALE;
    }
    (raw as u32).clamp(MIN_SCALE, MAX_SCALE)
}

/// `base` stretched for the load this machine is under. For waits that hand a
/// timeout to something else (a `recv_timeout`, a probe's own deadline) rather
/// than polling.
pub fn spawn_budget(base: Duration) -> Duration {
    base * load_scale()
}

/// Poll `ready` until it answers true, within a load-scaled `base` budget.
/// Panics naming what was being waited for, the budget, and the scale, so a
/// failure says whether it ran out of time or never happened at all.
///
/// The teeth are unchanged: a genuinely broken behaviour never satisfies
/// `ready` and still fails here. It just takes longer to say so, which is the
/// trade this exists to make - a slow true failure over a fast false one.
#[track_caller]
pub fn await_spawned(base: Duration, what: &str, mut ready: impl FnMut() -> bool) {
    let scale = load_scale();
    let budget = base * scale;
    let started = Instant::now();
    while started.elapsed() < budget {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "timed out after {budget:?} ({base:?} x{scale} for load) waiting for {what}; \
         if this is a real failure it would also fail at 1x, so re-run the full suite \
         on the unmodified merge base under the same load before suspecting your diff"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // The floor matters more than it looks: every budget this replaces was
    // observed failing at 1x, so a quiet machine must still get more room
    // than the constant it replaced, not less.
    #[test]
    fn the_scale_is_clamped_at_both_ends() {
        assert_eq!(
            scale_from(1.0, 0.0),
            MIN_SCALE,
            "a quiet machine still gets the floor"
        );
        assert_eq!(
            scale_from(64.0, 1.0),
            MAX_SCALE,
            "a huge thread count is capped"
        );
        assert_eq!(
            scale_from(1.0, 999.0),
            MAX_SCALE,
            "a crushed machine is capped too"
        );
        assert_eq!(
            scale_from(f64::NAN, f64::NAN),
            MIN_SCALE,
            "a nonsense reading falls back"
        );
    }

    // Each signal is blind where the other sees, so the rule takes whichever
    // is worse rather than averaging them away.
    #[test]
    fn either_signal_alone_can_raise_the_scale() {
        assert_eq!(
            scale_from(4.0, 1.0),
            4,
            "the suite's own parallelism counts"
        );
        assert_eq!(
            scale_from(1.0, 4.0),
            4,
            "so does load from outside the suite"
        );
        assert_eq!(scale_from(4.0, 6.0), 6, "the worse of the two wins");
    }

    #[test]
    fn a_budget_scales_by_exactly_the_scale() {
        let base = Duration::from_millis(500);
        assert_eq!(spawn_budget(base), base * load_scale());
    }

    // The helper must not become a way to pass by waiting: a condition that
    // never comes true still fails, and says so in terms the reader can act on.
    #[test]
    #[should_panic(expected = "waiting for a thing that never happens")]
    fn a_condition_that_never_holds_still_fails() {
        await_spawned(
            Duration::from_millis(1),
            "a thing that never happens",
            || false,
        );
    }

    #[test]
    fn a_condition_already_true_returns_at_once() {
        let started = Instant::now();
        await_spawned(Duration::from_secs(30), "an immediate condition", || true);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
