//! Background test-runner worker. Mirrors [`crate::app::git_worker`]: a thread
//! owns the channels, runs `cargo test` off the render loop, streams parsed
//! cases plus raw output, and the app drains the results into the panel each
//! tick. Requests: `RunAll` (execute) and `Discover` (`cargo test -- --list`,
//! populate the tree without running); per-test granularity and other
//! ecosystems come in later milestones.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use super::model::{Activity, TestCase, TestStatus};
use super::parse::{
    parse_jest_json, parse_list_line, parse_pytest_collect_line, parse_pytest_line,
    parse_test_line, parse_vitest_list_json, parse_vitest_list_line, parse_vitest_tap_line,
};
use super::regex_escape;
use crate::output::{self, OutputLevel};
use crate::widgets::testing::TestingPanel;

mod commands;
mod control;
mod coverage;
mod go_and_codeql;
mod handle;
mod js_args;
mod pytest;
mod runners;
mod streaming;
mod test_binaries;
pub(crate) use commands::cargo_progress;
use commands::*;
pub use coverage::CoverageScope;
use coverage::*;
use go_and_codeql::*;
use js_args::*;
use pytest::*;
pub use runners::runner_for;
pub use runners::runner_for_file;
pub use runners::runners_for;
use runners::*;
use streaming::*;
pub use test_binaries::build_test_binary;
#[cfg(test)]
use test_binaries::*;

/// Which built-in run mechanism a workspace's tests use. Which one a given
/// root resolves to is manifest data ([`crate::testing::registry`]), not code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runner {
    Cargo,
    Pytest,
    Vitest,
    Jest,
    Go,
    Codeql,
}

pub enum TestRequest {
    RunAll,
    /// Run a single test by its exact name (click-to-run from the tree).
    RunOne(String),
    /// Run every test whose name contains the string (run-at-cursor by
    /// function name). cargo's name filter is a substring match. The file
    /// the run came from, if any, picks the runner (#1408).
    RunFilter(String, Option<PathBuf>),
    /// Run each of these tests by its exact name, one after another, as one
    /// run (#1535: Rerun Failed Tests). Ends once, failed if any failed.
    RunMany(Vec<String>),
    /// Run a whole suite by its unanchored name (a header click). The cargo
    /// arm anchors it with [`crate::testing::suite_pattern`] so `parse` cannot sweep
    /// `parse_utils::b`; pytest gets it positionally as a node-ID prefix.
    RunSuite(String),
    /// Run with the runner's coverage tool on (#263): everything, or only
    /// the tests `scope` names.
    RunCoverage(Option<CoverageScope>),
    /// List the tests without running them. `quiet` is the Testing view's
    /// own discover (#1841): the worker, not the UI thread, finds the
    /// runner, which can mean walking the tree for CodeQL test packs. It
    /// answers [`TestResponse::QuietDiscovery`] first, and a root no runner
    /// claims ends there, without a refusal the status bar reports.
    Discover {
        quiet: bool,
    },
    /// Rebind the worker's working directory (Explorer re-root). Without it the
    /// worker keeps shelling cargo in the launch dir captured at spawn, so after
    /// a Make Root into a child repo `cargo test -- --list` errors with "could
    /// not find Cargo.toml" and the tree never populates.
    SetRoot(PathBuf),
    /// Rebind the `codeql` program CodeQL test runs shell: the app's own, so
    /// the Testing view and the CodeQL side bar run the same CLI (and a test
    /// can stand a script in for it).
    SetCodeqlProgram(PathBuf),
    /// Rebind the folder of the user's added extensions the runners are read
    /// from: the app's own, so a test can add CodeQL, whose runner comes with
    /// the CodeQL extension (#1842), without touching the real one.
    SetExtensionsDir(PathBuf),
}

pub enum TestResponse {
    Started(Activity),
    Case(TestCase),
    /// A cargo build-status line (e.g. "Compiling ratatui v0.29") to show as
    /// live progress while the test binary compiles, so a multi-minute
    /// discovery doesn't look frozen behind a static "Discovering tests".
    Progress(String),
    /// `ok` is the runner's exit success, for a discovery as for a run: a
    /// listing that errors (a collection error, a build failure) is a failed
    /// discovery, not an empty project (#845). `None` when nothing ran (a
    /// coverage run refused for want of its tool).
    Finished {
        ok: Option<bool>,
    },
    /// The queued request found no enabled runner claiming the root (it was
    /// disabled between the app's entry-point check and the worker draining
    /// the queue). Distinct from `Finished` so the panel can roll back the
    /// Running marks the `start_*` call painted instead of stranding them,
    /// and the app can say why nothing ran.
    Refused,
    /// The worker reached a quiet discovery (#1841). Sent before anything
    /// else it does, so the drain can tell the request the Testing view
    /// queued from any other's answers; when no runner claims the root,
    /// nothing follows it.
    QuietDiscovery,
    /// A coverage run's report, or why there is none. Sent before its
    /// `Finished`.
    Coverage(Result<crate::testing::coverage::Coverage, CoverageError>),
}

/// Why a coverage run produced no report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverageError {
    /// The runner's coverage tool is not installed: its name and the
    /// command that installs it.
    Missing { tool: &'static str, install: String },
    /// The run ended without writing a report (it failed to build, or
    /// the tool errored).
    NoReport,
    /// The runner has no coverage tool croft can drive: its name.
    Unsupported { runner: &'static str },
}

/// What a cancel (#1534) needs to reach the runner the worker thread is
/// blocked on: the process group of the child it is streaming, and how
/// many requests the app has cancelled. Requests are counted on both sides
/// in channel order, so a cancel covers the request in flight and any the
/// worker has not picked up yet, and never a later one.
#[derive(Clone, Default)]
pub struct RunControl {
    /// The streaming child's pid, which is its process group (it is
    /// spawned as a group leader).
    running: Arc<Mutex<Option<i32>>>,
    /// Requests numbered up to this one are cancelled.
    cancelled_upto: Arc<AtomicU64>,
}

pub struct TestWorker {
    request_tx: Sender<TestRequest>,
    /// Reaches the runner the worker thread is blocked on (#1534).
    control: RunControl,
    /// Requests sent so far: the worker numbers them in the same order.
    sent: u64,
    /// A cancel is waiting for its run's end, which the drain reports as
    /// cancelled rather than as a verdict.
    cancel_pending: bool,
    /// Responses arrive tagged with the epoch of the root they ran under; the
    /// drain drops tags older than [`Self::expected_epoch`] so a run still
    /// streaming when the Explorer re-roots can't pollute the new project's
    /// tree (same idea as the commit graph's root-tagged drain).
    response_rx: Receiver<(u64, TestResponse)>,
    /// A quiet discovery is queued and has not started yet (#1841). Until
    /// the worker reaches it the panel is not busy, so without this every
    /// reopen of the Testing view would queue another.
    discover_queued: bool,
    /// Bumped on every [`Self::set_root`], in lockstep with the loop's own
    /// counter: the request channel is FIFO, so both sides count the same
    /// `SetRoot`s in the same order.
    expected_epoch: u64,
    // Mirror of the root the loop last saw, for tests that assert the re-root
    // wiring. The loop owns its own copy via `SetRoot`; prod never reads this.
    #[cfg(test)]
    root: PathBuf,
}

/// A response sender bound to the epoch of the request it serves, so every
/// line a handler streams is tagged without threading the counter through.
struct EpochTx<'a> {
    tx: &'a Sender<(u64, TestResponse)>,
    epoch: u64,
    /// The `codeql` program a CodeQL test run shells (see
    /// [`TestRequest::SetCodeqlProgram`]); carried here because every
    /// handler already takes the sender.
    codeql: &'a Path,
    /// The user's added extensions, whose runners count alongside the
    /// bundled ones (see [`TestRequest::SetExtensionsDir`]).
    extensions: &'a Path,
    /// Where the streaming child is recorded for a cancel (#1534), and the
    /// number of the request being served.
    control: &'a RunControl,
    request: u64,
    /// Which runner listed or ran each test (#1408), so a later run of one
    /// goes back to it; `tag` is the runner whose cases this sender
    /// carries.
    owners: Option<&'a RefCell<BTreeMap<String, Runner>>>,
    tag: Option<Runner>,
}

fn worker_loop(
    mut root: PathBuf,
    rx: Receiver<TestRequest>,
    tx: Sender<(u64, TestResponse)>,
    control: &RunControl,
) {
    let mut epoch = 0u64;
    // Found on PATH, as the CodeQL side bar finds it, until the app says
    // otherwise.
    let mut codeql = PathBuf::from("codeql");
    let mut extensions = crate::lsp::manifest::user_extensions_dir();
    let mut request = 0u64;
    let owners = RefCell::new(BTreeMap::new());
    while let Ok(req) = rx.recv() {
        request += 1;
        let etx = EpochTx {
            tx: &tx,
            epoch,
            codeql: &codeql,
            extensions: &extensions,
            control,
            request,
            owners: Some(&owners),
            tag: None,
        };
        match req {
            TestRequest::RunAll => run_all(&root, &etx),
            TestRequest::RunCoverage(scope) => run_coverage(&root, &etx, scope.as_ref()),
            TestRequest::RunOne(name) => run_one(&root, &etx, &name),
            TestRequest::RunMany(names) => run_many(&root, &etx, &names),
            TestRequest::RunFilter(pattern, file) => {
                run_filter_in(&root, &etx, &pattern, false, file.as_deref())
            }
            TestRequest::RunSuite(suite) => run_filter(&root, &etx, &suite, true),
            TestRequest::Discover { quiet } => discover(&root, &etx, quiet),
            TestRequest::SetRoot(p) => {
                root = p;
                epoch += 1;
                owners.borrow_mut().clear();
            }
            TestRequest::SetCodeqlProgram(p) => codeql = p,
            TestRequest::SetExtensionsDir(p) => extensions = p,
        }
    }
}

fn run_all(root: &Path, tx: &EpochTx) {
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
fn run_all_with(runner: Runner, root: &Path, tx: &EpochTx) -> bool {
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
fn run_one(root: &Path, tx: &EpochTx, name: &str) {
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
fn run_many(root: &Path, tx: &EpochTx, names: &[String]) {
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
fn run_one_case_seen(root: &Path, tx: &EpochTx, runner: Runner, name: &str) -> bool {
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
fn run_one_case(root: &Path, tx: &EpochTx, runner: Runner, name: &str) -> bool {
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
fn run_filter(root: &Path, tx: &EpochTx, pattern: &str, suite: bool) {
    run_filter_in(root, tx, pattern, suite, None);
}

/// [`run_filter`] for a run that came from `file`, whose language picks
/// the runner (#1408).
fn run_filter_in(root: &Path, tx: &EpochTx, pattern: &str, suite: bool, file: Option<&Path>) {
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

/// List tests without running them (`cargo test -- --list`, or pytest's
/// `--collect-only -q`), streaming each as a `NotRun` case. The cargo path
/// still compiles the test binary, hence the Discovering state. The lister's
/// exit status goes back with `Finished`, so a listing that errors (pytest's
/// exit 2 on a collection error, a build failure) reads as a failed
/// discovery rather than a project with no tests (#845).
fn discover(root: &Path, tx: &EpochTx, quiet: bool) {
    if quiet {
        tx.send(TestResponse::QuietDiscovery);
    }
    // See run_all: refuse before `Started` so the panel keeps its tree.
    let runners = runners_for(tx.extensions, root);
    if runners.is_empty() {
        if !quiet {
            tx.send(TestResponse::Refused);
        }
        return;
    }
    tx.send(TestResponse::Started(Activity::Discovering));
    // Each runner lists its own tests into the one tree (#1408).
    let mut ok = true;
    for runner in runners {
        ok &= discover_with(runner, root, &tx.tagged(runner));
    }
    tx.send(TestResponse::Finished { ok: Some(ok) });
}

/// List `runner`'s tests; whether its lister succeeded.
fn discover_with(runner: Runner, root: &Path, tx: &EpochTx) -> bool {
    let not_run = |name: String| TestCase {
        name,
        status: TestStatus::NotRun,
    };
    match runner {
        Runner::Pytest => {
            let cmd = pytest_cmd(root, &["--collect-only", PYTEST_COLLECT_VERBOSITY, "--color=no"]);
            let show = |line: &str| Some(line.to_string());
            run_streaming_exit(tx, cmd, show, |line| {
                parse_pytest_collect_line(line)
                    .map(not_run)
                    .into_iter()
                    .collect()
            })
            // Exit 5 is pytest's "no tests were collected": a project with
            // no tests yet, which lists fine.
            .map(|code| matches!(code, Some(0 | 5)))
        }
        // `--passWithNoTests`: vitest 1 has no `list` command, reads the
        // word as a file filter and, finding no file, exits 1 without it.
        // vitest 2 and later exit 0 with no tests either way, and 1 still
        // on a file that fails to load. jest's `--listTests` exits 0 with
        // nothing to list.
        // A projects setup (#1504) lists as JSON, whose absolute files keep
        // each package's tests apart; the text listing names a test by its
        // project and a file relative to that project.
        Runner::Vitest if vitest_has_projects(root) => {
            let listed = std::cell::RefCell::new(String::new());
            let cmd = js_cmd(root, "vitest", &["list", "--json", "--passWithNoTests"]);
            let ok = run_streaming(tx, cmd, |line| {
                let mut listed = listed.borrow_mut();
                listed.push_str(line);
                listed.push('\n');
                Vec::new()
            });
            for name in parse_vitest_list_json(root, &listed.borrow()) {
                tx.send(TestResponse::Case(not_run(name)));
            }
            ok
        }
        Runner::Vitest => {
            let cmd = js_cmd(root, "vitest", &["list", "--passWithNoTests"]);
            run_streaming(tx, cmd, |line| {
                parse_vitest_list_line(line)
                    .map(not_run)
                    .into_iter()
                    .collect()
            })
        }
        // jest can only cheaply list FILES (`--listTests`, absolute paths);
        // per-test names come from the first run's `--json` document.
        Runner::Jest => {
            let cmd = js_cmd(root, "jest", &["--listTests"]);
            run_streaming(tx, cmd, |line| {
                let rel = Path::new(line.trim())
                    .strip_prefix(root)
                    .map(|p| p.display().to_string());
                match rel {
                    Ok(r) if !r.is_empty() => vec![not_run(r)],
                    _ => Vec::new(),
                }
            })
        }
        Runner::Cargo => {
            let cmd = cargo_cmd(root, &["test", "--color=never", "--", "--list"]);
            run_streaming(tx, cmd, |line| {
                parse_list_line(line).map(not_run).into_iter().collect()
            })
        }
        // `go test -list` compiles each package's tests but runs none.
        Runner::Go => {
            let modules = crate::testing::gotest::Modules::of(root);
            let cmd = go_cmd(root, &modules.expand(&["-list", ".", "./..."]));
            run_streaming_shown(tx, cmd, crate::testing::gotest::shown, |line| {
                crate::testing::gotest::parse_list_event(&modules, line)
                    .map(not_run)
                    .into_iter()
                    .collect()
            })
        }
        // Tests are files; listing them needs no CLI, and cannot fail.
        Runner::Codeql => {
            let packs = crate::testing::codeqltest::test_packs(root, &codeql_markers());
            for id in crate::testing::codeqltest::discover(root, &packs) {
                tx.send(TestResponse::Case(not_run(id)));
            }
            Some(true)
        }
    }
    // A lister that never spawned (pytest not installed) failed too.
    .unwrap_or(false)
}

#[cfg(test)]
mod tests;
