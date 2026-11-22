//! Running tests: everything, one, several, or those matching a filter, with each
//! runner's arguments.

use super::*;

pub(super) fn run_all(root: &Path, tx: &EpochTx) {
    // `None` (no enabled runner claims the root) must never fall through to
    // cargo: the app's entry points refuse first, and this second gate keeps
    // a disabled runner from shelling anything even if a new call site
    // forgets the check. It must also run BEFORE `Started`, which clears the
    // discovered tree — a refusal keeps the panel exactly as it was.
    // Exhaustive matches below, no `_` arm, so a future Runner variant is a
    // compile error here instead of silently cargo.
    let runners = runners_for(tx.extensions, root);
    if runners.is_empty() {
        tx.send(TestResponse::Refused);
        return;
    }
    tx.send(TestResponse::Started(Activity::Running));
    // Every runner runs, even after one fails (#1408).
    let mut ok = true;
    for runner in runners {
        ok &= run_all_with(runner, root, &tx.tagged(runner));
    }
    tx.send(TestResponse::Finished { ok: Some(ok) });
}

/// Run everything `runner` runs; whether it all passed.
pub(super) fn run_all_with(runner: Runner, root: &Path, tx: &EpochTx) -> bool {
    match runner {
        Runner::Pytest => {
            let cmd = pytest_cmd(root, &[PYTEST_RUN_VERBOSITY, "--color=no"]);
            run_streaming(tx, cmd, one(parse_pytest_line))
        }
        Runner::Vitest => run_vitest(
            tx,
            root,
            &[String::from("run"), String::from("--reporter=tap-flat")],
        ),
        Runner::Jest => {
            let cmd = js_cmd(root, "jest", &["--json"]);
            run_streaming(tx, cmd, |line| parse_jest_json(root, line))
        }
        Runner::Cargo => {
            let cmd = cargo_cmd(root, &["test", "--no-fail-fast", "--color=never"]);
            run_streaming(tx, cmd, one(parse_test_line))
        }
        Runner::Go => run_go(tx, root, &[String::from("./...")]),
        Runner::Codeql => {
            let packs = crate::testing::codeqltest::test_packs(root, &codeql_markers());
            match crate::testing::codeqltest::all_args(root, &packs) {
                Some(args) => run_codeql(tx, root, &args),
                // No test pack: nothing to run, and nothing failed.
                None => Some(true),
            }
        }
    }
    .unwrap_or(false)
}

/// Run a single test by exact name: `cargo test <name> --color=never -- --exact`
/// (`--exact` stops the name being treated as a substring filter), or for
/// pytest the node ID itself, which is already exact. No `Started` is sent: the
/// app marks just this case Running and keeps the rest of the tree, so a
/// single-test run doesn't wipe the discovered list.
pub(super) fn run_one(root: &Path, tx: &EpochTx, name: &str) {
    // See run_all: `None` refuses instead of falling through to cargo, and
    // `Refused` (not a bare Finished) rolls back this case's Running mark.
    let Some(runner) = tx.choose(&runners_for(tx.extensions, root), name, None) else {
        tx.send(TestResponse::Refused);
        return;
    };
    let ok = run_one_case(root, tx, runner, name);
    tx.send(TestResponse::Finished { ok: Some(ok) });
}

/// Run each of `names` exactly, one after another, as a single run (#1535):
/// every runner can run one test by name, which no shared multi-name filter
/// spans. All are run whatever an earlier one did, and the run fails if any
/// of them did.
pub(super) fn run_many(root: &Path, tx: &EpochTx, names: &[String]) {
    let runners = runners_for(tx.extensions, root);
    if runners.is_empty() {
        tx.send(TestResponse::Refused);
        return;
    }
    let mut ok = true;
    for name in names {
        // Each test goes back to the runner that listed it (#1408).
        ok &= tx
            .choose(&runners, name, None)
            .is_some_and(|runner| run_one_case_seen(root, tx, runner, name));
    }
    tx.send(TestResponse::Finished { ok: Some(ok) });
}

/// [`run_one_case`], passed only when the runner also reported a result
/// for `name`: a test renamed since discovery matches nothing, and a
/// name-filtered runner exits cleanly having run zero tests.
pub(super) fn run_one_case_seen(root: &Path, tx: &EpochTx, runner: Runner, name: &str) -> bool {
    let (case_tx, case_rx) = std::sync::mpsc::channel::<(u64, TestResponse)>();
    std::thread::scope(|s| {
        let forward = s.spawn(|| {
            let mut seen = false;
            for (epoch, resp) in case_rx {
                if let TestResponse::Case(case) = &resp {
                    seen |= case.name == name;
                }
                let _ = tx.tx.send((epoch, resp));
            }
            seen
        });
        let passed = {
            let etx = EpochTx {
                tx: &case_tx,
                epoch: tx.epoch,
                codeql: tx.codeql,
                extensions: tx.extensions,
                control: tx.control,
                request: tx.request,
                owners: tx.owners,
                tag: tx.tag,
            };
            run_one_case(root, &etx, runner, name)
        };
        drop(case_tx);
        let seen = forward.join().unwrap_or(false);
        passed && seen
    })
}

/// One test by its exact name under `runner`, its result streamed; true
/// when the runner exited cleanly.
pub(super) fn run_one_case(root: &Path, tx: &EpochTx, runner: Runner, name: &str) -> bool {
    match runner {
        Runner::Pytest => {
            let cmd = pytest_cmd(root, &[PYTEST_RUN_VERBOSITY, "--color=no", name]);
            run_streaming(tx, cmd, one(parse_pytest_line))
        }
        Runner::Vitest => run_vitest(tx, root, &vitest_one_args(name)),
        Runner::Jest => {
            let cmd = js_cmd(root, "jest", &jest_one_args(name));
            run_streaming(tx, cmd, |line| parse_jest_json(root, line))
        }
        Runner::Cargo => {
            let cmd = cargo_cmd(root, &["test", name, "--color=never", "--", "--exact"]);
            run_streaming(tx, cmd, one(parse_test_line))
        }
        Runner::Go => run_go(tx, root, &crate::testing::gotest::one_args(name)),
        Runner::Codeql => run_codeql(tx, root, &crate::testing::codeqltest::select_args(name)),
    }
    .unwrap_or(false)
}

/// Run every test matching a name filter. `suite` marks a header click: the
/// cargo arm then anchors the pattern with [`crate::testing::suite_pattern`], since a
/// bare substring like `parse` would also sweep `parse_utils::b`. pytest
/// splits the two shapes: a suite is a node-ID prefix (`tests/test_x.py`,
/// `tests/test_x.py::TestGroup`) passed positionally, a bare function name
/// (run-at-cursor) goes through `-k`, pytest's substring matcher. vitest and
/// jest anchor a suite's describe chain (see [`suite_title_anchor`]), since
/// their `-t` is an unanchored regex over the full name. Like [`run_one`],
/// the app has already marked the affected cases and shown the busy state,
/// so no `Started` is sent.
pub(super) fn run_filter(root: &Path, tx: &EpochTx, pattern: &str, suite: bool) {
    run_filter_in(root, tx, pattern, suite, None);
}

/// [`run_filter`] for a run that came from `file`, whose language picks
/// the runner (#1408).
pub(super) fn run_filter_in(
    root: &Path,
    tx: &EpochTx,
    pattern: &str,
    suite: bool,
    file: Option<&Path>,
) {
    // See run_all: `None` refuses instead of falling through to cargo, and
    // `Refused` (not a bare Finished) rolls back the filtered Running marks.
    let Some(runner) = tx.choose(&runners_for(tx.extensions, root), pattern, file) else {
        tx.send(TestResponse::Refused);
        return;
    };
    let ok = match runner {
        Runner::Pytest => {
            // A suite is always a node-ID prefix; the `.py` sniff keeps a
            // node-ID handed to a plain filter run positional too.
            let cmd = if suite || pattern.contains(".py") {
                pytest_cmd(root, &[PYTEST_RUN_VERBOSITY, "--color=no", pattern])
            } else {
                pytest_cmd(root, &[PYTEST_RUN_VERBOSITY, "--color=no", "-k", pattern])
            };
            run_streaming(tx, cmd, one(parse_pytest_line))
        }
        Runner::Vitest => run_vitest(tx, root, &vitest_filter_args(pattern, suite)),
        Runner::Jest => {
            let cmd = js_cmd(root, "jest", &jest_filter_args(pattern, suite));
            run_streaming(tx, cmd, |line| parse_jest_json(root, line))
        }
        Runner::Cargo => {
            let anchored;
            let pattern = if suite {
                anchored = crate::testing::suite_pattern(pattern);
                &anchored
            } else {
                pattern
            };
            let cmd = cargo_cmd(root, &["test", pattern, "--no-fail-fast", "--color=never"]);
            run_streaming(tx, cmd, one(parse_test_line))
        }
        Runner::Go => run_go(
            tx,
            root,
            &crate::testing::gotest::filter_args(pattern, suite),
        ),
        // A suite is its folder; any other filter is taken as the path (or
        // id) of what to run.
        Runner::Codeql => run_codeql(tx, root, &crate::testing::codeqltest::select_args(pattern)),
    }
    .unwrap_or(false);
    tx.send(TestResponse::Finished { ok: Some(ok) });
}
