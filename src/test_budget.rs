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
//! So the budget stops being a guess and becomes
//! `base x BASE_CALIBRATION x scale`, where `base` is what the operation
//! costs on a quiet machine (the number a test author can actually reason
//! about), `scale` is measured from the load the suite is running under, and
//! the calibration is the one constant that keeps the wall clock where it was
//! when these budgets were derived (#422). The widest any budget stretches is
//! `BASE_CALIBRATION x MAX_SCALE`, deliberately unchanged at 8x, because a
//! broken test must still fail in a time a developer will wait.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Widest a budget may stretch. A failing test must still fail: past this the
/// wait is no longer distinguishing "loaded" from "broken", and every extra
/// second is paid by the developer whose change genuinely broke the thing.
///
/// Halved with the #422 rule change, so the CEILING is unchanged. The total
/// stretch is `BASE_CALIBRATION * MAX_SCALE`, and leaving this at 8 while
/// introducing a x2 calibration would have doubled the worst case with it —
/// a broken test on a 10s base going from 80s to 160s, which is exactly the
/// cost this constant exists to bound. 2 x 4 is the 8 it always was.
const MAX_SCALE: u32 = 4;

/// Narrowest it may shrink. Never below the budgets these tests already had,
/// since every one of them was observed failing at 1x.
const MIN_SCALE: u32 = 2;

/// Recalibration for the #422 rule change, applied once here rather than at
/// sixteen call sites.
///
/// The old scale tracked machine SIZE, so on any box with four or more cores
/// it read at least 4 and a `base` was effectively `base * 4`. Measuring
/// PRESSURE instead correctly reads 2 on a quiet machine — which would have
/// halved every budget in the suite, and the module's own note warned that
/// every one of these waits was observed failing at 1x. Doubling the base
/// holds the wall clock where it was calibrated while the scale goes back to
/// meaning what it says.
///
/// Deliberately central. Doubling sixteen literals by hand invites missing
/// one, and a missed one is a flake that looks like the change under test —
/// the exact failure this module exists to remove. It also keeps `base` the
/// number a test author reasons about ("a shell printing a line costs about
/// a second") rather than a number pre-multiplied for a rule they would have
/// to know about.
const BASE_CALIBRATION: u32 = 2;

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
/// Two signals, and their SUM, because each is blind where the other sees
/// and both contend for the same cores (#422). The suite's own thread count
/// is known before anything runs and covers the self-inflicted case (dozens
/// of test shells at once), where
/// a load average would still be reading the quiet minute before the suite
/// started. The load average covers what no test can know: a second cargo
/// build, another suite, whatever else owns the machine.
pub fn load_scale() -> u32 {
    static SCALE: OnceLock<u32> = OnceLock::new();
    *SCALE.get_or_init(|| {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f64;
        let threads = configured_threads().unwrap_or(cpus);
        // The RAW load average, not a ratio: `scale_from` divides once, by
        // the same `cpus`, after summing. Dividing here too was the unit
        // error (#422).
        let load = load_average().unwrap_or(0.0);
        scale_from(threads, load, cpus)
    })
}

/// How many test threads the harness is actually running.
///
/// BOTH spellings, because they are the same instruction to the harness and
/// were not the same here. `cargo test -- --test-threads=8` does not set
/// `RUST_TEST_THREADS`, so reading only the variable reported this machine's
/// CORE COUNT while the suite ran twice that many threads. The signal meant
/// to cover the self-inflicted case went blind exactly when someone spelled
/// it with the flag, which is how #397's flake was reached: measured on a
/// 4-CPU box at `--test-threads=8` under six spinning hogs, the scale read
/// 4 where the visible count gives 8, so a wait budgeted at 4s should have
/// had 8s.
///
/// THE FLAG FIRST, because that is libtest's own precedence and reading it
/// the other way round would still undercount on the machine that matters
/// most. `library/test/src/lib.rs` resolves the count as
/// `opts.test_threads.unwrap_or_else(get_concurrency)`, and only
/// `get_concurrency` consults `RUST_TEST_THREADS`
/// (`library/test/src/helpers/concurrency.rs:7-16`). CI pins the variable to
/// 4 and CONTRIBUTING teaches the variable, so `RUST_TEST_THREADS=4 cargo
/// test -- --test-threads=8` is the natural way to reproduce #397: the
/// harness runs 8 and an env-first reading believes 4, which is the same
/// undercount this function exists to remove.
fn configured_threads() -> Option<f64> {
    threads_from_args(std::env::args()).or_else(|| {
        std::env::var("RUST_TEST_THREADS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
    })
}

/// `--test-threads N` and `--test-threads=N`, the two spellings the harness
/// accepts, taken from an iterator so the parsing is testable without a
/// process in a particular state.
///
/// The whole argv is scanned and the LAST parseable value wins, rather than
/// returning at the first occurrence. A value that does not parse is not a
/// thread count, and stopping there would let it mask a real one later in
/// the line; libtest itself refuses such a run outright, so the only thing
/// this ordering changes is which answer a malformed argv produces on the
/// way to failing.
fn threads_from_args<I: IntoIterator<Item = String>>(args: I) -> Option<f64> {
    let mut found = None;
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        let value = if let Some(v) = a.strip_prefix("--test-threads=") {
            Some(v.to_string())
        } else if a == "--test-threads" {
            args.next()
        } else {
            None
        };
        if let Some(n) = value.and_then(|v| v.parse::<f64>().ok()) {
            found = Some(n);
        }
    }
    found
}

/// The rule itself, split out so it can be tested without a machine in a
/// particular state.
///
/// **Runnable work per CPU** (#422). The old rule was `max(threads, load /
/// cpus)`, which compared a COUNT against a RATIO: on a 64-core box running
/// 64 threads with no contention at all it returned the cap of 8, while a
/// genuinely crushed 4-core box returned 4. The scale was tracking machine
/// size rather than pressure, which is the one thing it exists to measure.
///
/// The two signals ADD rather than compete. The suite's own threads and
/// whatever else owns the machine are contending for the same cores, so the
/// runnable count is their sum, and dividing by `cpus` turns it into the
/// factor by which everything is slower than it would be alone. Both inputs
/// keep the property that made the old `max` attractive — each is blind
/// where the other sees, and summing preserves that without pretending a
/// count and a ratio are the same kind of number.
///
/// This deliberately reads LOWER than the old rule on a quiet machine (a
/// quiet 4-CPU box: 4 before, 2 now), which is why the bases moved with it
/// in the same change. See [`await_spawned`] for the ceiling that had to be
/// preserved: #397's measured flake — a 4-CPU box at `--test-threads=8`
/// under six spinning hogs — needed 8 seconds where the base was 4, and
/// with doubled bases it still gets them.
fn scale_from(threads: f64, load: f64, cpus: f64) -> u32 {
    if !(threads.is_finite() && load.is_finite() && cpus.is_finite()) || cpus < 1.0 {
        return MIN_SCALE;
    }
    // `load` is the machine-wide runnable average, `threads` this suite's own
    // contribution; the sum is what is competing for `cpus`.
    //
    // Those overlap once the suite has been running a minute — its own
    // threads then appear in the load average too, so the sum counts them
    // twice. Deliberately left alone. The overlap is bounded (it can only
    // raise the quotient by `threads / cpus`, one clamp step in practice)
    // and it errs toward MORE slack, which is the safe direction for a
    // deadline; subtracting an estimate of our own load would be a guess
    // about a number we cannot observe, and a wrong one shortens budgets.
    // `load_scale` memoises in a `OnceLock`, so in practice the reading is
    // taken at the first wait — early, while the average still describes
    // the machine the suite arrived on.
    let raw = ((threads + load) / cpus).ceil();
    // Belt and braces: the guard above already rules out every input that
    // could make this non-finite, so this branch is unreachable today. Kept
    // because the guard is the thing most likely to be relaxed later.
    if !raw.is_finite() {
        return MIN_SCALE;
    }
    (raw as u32).clamp(MIN_SCALE, MAX_SCALE)
}

/// `base` stretched for the load this machine is under. For waits that hand a
/// timeout to something else (a `recv_timeout`, a probe's own deadline) rather
/// than polling.
pub fn spawn_budget(base: Duration) -> Duration {
    base * BASE_CALIBRATION * load_scale()
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
    let budget = base * BASE_CALIBRATION * scale;
    let started = Instant::now();
    while started.elapsed() < budget {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "timed out after {budget:?} ({base:?} x{BASE_CALIBRATION} calibration \
         x{scale} for load) waiting for {what}; \
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
            scale_from(1.0, 0.0, 4.0),
            MIN_SCALE,
            "a quiet machine still gets the floor"
        );
        assert_eq!(
            scale_from(64.0, 0.0, 4.0),
            MAX_SCALE,
            "far more threads than cores is capped"
        );
        assert_eq!(
            scale_from(1.0, 999.0, 4.0),
            MAX_SCALE,
            "a crushed machine is capped too"
        );
        for (t, l, c) in [
            (f64::NAN, f64::NAN, 4.0),
            (4.0, f64::INFINITY, 4.0),
            (4.0, 1.0, 0.0),
            (4.0, 1.0, f64::NAN),
        ] {
            assert_eq!(
                scale_from(t, l, c),
                MIN_SCALE,
                "a nonsense reading falls back: {t} {l} {c}"
            );
        }
    }

    /// #422: the scale measures PRESSURE, not machine size.
    ///
    /// The old rule took `max(threads, load / cpus)` — a count against a
    /// ratio — so it read the core count on any machine big enough, and a
    /// 64-core box idling at one thread per core got the same maximum
    /// stretch as a genuinely crushed one.
    #[test]
    fn a_big_quiet_machine_is_not_a_loaded_one() {
        assert_eq!(
            scale_from(64.0, 1.0, 64.0),
            MIN_SCALE,
            "64 threads on 64 cores with nothing else running is not \
             contention: the old rule returned the CAP here"
        );
        assert_eq!(
            scale_from(8.0, 0.5, 8.0),
            MIN_SCALE,
            "nor is a quiet developer machine at its default thread count"
        );
        assert!(
            scale_from(4.0, 14.0, 4.0) > MIN_SCALE,
            "while a 4-core box under a genuine 3.5x oversubscription is"
        );
    }

    /// Each signal is blind where the other sees, so both count — and they
    /// ADD, because the suite's threads and everything else on the machine
    /// contend for the same cores.
    #[test]
    fn both_signals_contribute_to_the_pressure() {
        let quiet = scale_from(4.0, 0.0, 4.0);
        assert!(
            scale_from(12.0, 0.0, 4.0) > quiet,
            "the suite's own parallelism counts"
        );
        assert!(
            scale_from(4.0, 12.0, 4.0) > quiet,
            "so does load from outside the suite"
        );
        // Strict, on inputs where summing genuinely separates from `max`:
        // (4 + 4) / 4 = 2 against max(4, 4) / 4 = 1. A `>=` here would pass
        // even if the sum were replaced by the old maximum.
        assert!(
            scale_from(6.0, 6.0, 4.0) > scale_from(6.0, 0.0, 4.0),
            "together they are worse than either alone"
        );
    }

    /// The total stretch is what a broken test pays, and it must not have
    /// grown. `BASE_CALIBRATION * MAX_SCALE` was 1 x 8 before #422 and is
    /// 2 x 4 after — the same 8. Pinned because the two constants are only
    /// safe as a PAIR: raising either alone doubles what a developer waits
    /// to be told their change broke something, which is the cost
    /// `MAX_SCALE` exists to bound.
    #[test]
    fn the_worst_case_stretch_is_unchanged() {
        assert_eq!(
            BASE_CALIBRATION * MAX_SCALE,
            8,
            "a broken test on a 10s base still fails in 80s, not 160s"
        );
        assert_eq!(
            BASE_CALIBRATION * MIN_SCALE,
            4,
            "and a quiet machine waits 4x, down from the old 8x"
        );
        // The property `MAX_SCALE`'s doc actually promises, asserted on the
        // OUTPUT rather than on the factors: a third multiplier entering the
        // expression would leave the constants above untouched.
        let base = Duration::from_secs(1);
        assert!(
            spawn_budget(base) <= base * 8,
            "no budget may exceed the 8x ceiling, however it is composed"
        );
    }

    /// A conversion must not be a cut. #397 replaced two `waited < 8000`
    /// loops with `await_spawned`, and the base that does that honestly is
    /// the one whose FLOOR is the constant it replaced: `MIN_SCALE`'s own
    /// doc says a budget may never shrink below what these tests already
    /// had, since every one of them was observed failing at 1x. A 1s base
    /// would have given them 4s on a quiet machine, which is half.
    #[test]
    fn a_converted_wait_keeps_the_budget_it_replaced() {
        let floor = Duration::from_secs(2) * BASE_CALIBRATION * MIN_SCALE;
        assert!(
            floor >= Duration::from_millis(8000),
            "a 2s base must reproduce the 8000ms it replaced at the floor, got {floor:?}"
        );
    }

    /// #397's measured flake, which is the ceiling this rule may not lower:
    /// a 4-CPU box at `--test-threads=8` under six spinning hogs needed a
    /// wait budgeted at 4s to have 8s.
    ///
    /// The old rule reached 8 there by reading the thread count directly.
    /// This one reads 4 — (8 + 6) / 4 — so the bases doubled in the same
    /// change to hold the same wall clock. That trade is the whole point of
    /// doing both together, and this test is what stops the scale half
    /// being changed again without the base half.
    #[test]
    fn the_397_flake_still_gets_its_eight_seconds() {
        let scale = scale_from(8.0, 6.0, 4.0);
        assert_eq!(scale, 4, "runnable work per CPU: ceil((8 + 6) / 4)");
        // That value is also MAX_SCALE, so the assertion above passes via
        // the CLAMP whether or not the formula is right — it survives
        // reverting the sum to the old `max` rule, and survives dropping
        // the division. This companion sits clear of both clamps, so it
        // asserts the formula itself: ceil((6 + 2) / 4) = 2, where the old
        // max(6, 0.5) gave 6.
        assert_eq!(
            scale_from(6.0, 2.0, 4.0),
            MIN_SCALE,
            "away from the clamps, the rule is the sum over cpus"
        );
        // Through the real formula, not a hand-doubled literal: the base a
        // test author writes is 1s, and the module supplies the calibration.
        let base = std::time::Duration::from_secs(1);
        assert!(
            base * BASE_CALIBRATION * scale >= std::time::Duration::from_secs(8),
            "the 1s base a caller writes still clears the 8s that flake needed"
        );
    }

    /// `--test-threads` and `RUST_TEST_THREADS` are the same instruction to
    /// the harness, and only one of them used to reach this module.
    ///
    /// The flag is what `cargo test -- --test-threads=8` sets, and it never
    /// touches the environment, so the suite's own parallelism read as the
    /// machine's core count whenever it was spelled that way. On the box
    /// where #397 reproduces that is the difference between a 4s budget and
    /// an 8s one for the wait that timed out.
    #[test]
    fn both_spellings_of_the_thread_count_are_seen() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            threads_from_args(argv(&["croft", "--test-threads=8"])),
            Some(8.0),
            "the joined spelling"
        );
        assert_eq!(
            threads_from_args(argv(&["croft", "--test-threads", "8"])),
            Some(8.0),
            "and the separated one"
        );
        assert_eq!(
            threads_from_args(argv(&["croft", "--nocapture"])),
            None,
            "absent means absent, so the caller falls back to the CPU count"
        );
        assert_eq!(
            threads_from_args(argv(&["croft", "--test-threads"])),
            None,
            "a flag with no value is not a thread count"
        );
        assert_eq!(
            threads_from_args(argv(&["croft", "--test-threads=zero"])),
            None,
            "nor is one that does not parse"
        );
        assert_eq!(
            threads_from_args(argv(&["croft", "--test-threads=zero", "--test-threads=8"])),
            Some(8.0),
            "a value that does not parse must not mask a real one later on"
        );
    }

    /// The flag beats the variable, because that is libtest's own order.
    ///
    /// `library/test/src/lib.rs` resolves the count as
    /// `opts.test_threads.unwrap_or_else(get_concurrency)`, and only
    /// `get_concurrency` reads `RUST_TEST_THREADS`. Reading them the other
    /// way round undercounts exactly where it matters: CI pins the variable
    /// to 4 and `CONTRIBUTING.md` teaches the variable, so
    /// `RUST_TEST_THREADS=4 cargo test -- --test-threads=8` is the natural
    /// way to reproduce #397, and an env-first reading believes 4 while the
    /// harness runs 8.
    ///
    /// The rule is asserted on the resolver rather than by setting the
    /// variable: this suite shares one process, so mutating the environment
    /// here would change what every other thread reads.
    #[test]
    fn the_flag_outranks_the_environment_variable_as_libtest_does() {
        // The shape of `configured_threads`, with both sources supplied
        // explicitly instead of read from the process.
        fn resolve(flag: Option<f64>, env: Option<f64>) -> Option<f64> {
            flag.or(env)
        }
        assert_eq!(
            resolve(Some(8.0), Some(4.0)),
            Some(8.0),
            "the flag wins when both are given, as libtest resolves it"
        );
        assert_eq!(
            resolve(None, Some(4.0)),
            Some(4.0),
            "the variable is the fallback, not the override"
        );
        assert_eq!(resolve(None, None), None, "neither means fall back to CPUs");
    }

    #[test]
    fn a_budget_scales_by_exactly_the_scale() {
        let base = Duration::from_millis(500);
        assert_eq!(
            spawn_budget(base),
            base * BASE_CALIBRATION * load_scale(),
            "the calibration factor rides with the scale (#422)"
        );
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
