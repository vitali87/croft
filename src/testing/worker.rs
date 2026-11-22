//! Background test-runner worker. Mirrors [`crate::app::git_worker`]: a thread
//! owns the channels, runs `cargo test` off the render loop, streams parsed
//! cases plus raw output, and the app drains the results into the panel each
//! tick. Requests: `RunAll` (execute) and `Discover` (`cargo test -- --list`,
//! populate the tree without running); per-test granularity and other
//! ecosystems come in later milestones.

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
use crate::jobs::{Start, Worker, WorkerPoll};
use crate::output::{self, OutputLevel};
use crate::widgets::testing::TestingPanel;

mod commands;
mod control;
mod coverage;
mod discovery;
mod go_and_codeql;
mod handle;
mod js_args;
mod main_loop;
mod pytest;
mod runners;
mod runs;
mod streaming;
mod test_binaries;
pub(crate) use commands::cargo_progress;
use commands::*;
pub use coverage::CoverageScope;
use coverage::*;
use discovery::*;
use go_and_codeql::*;
use js_args::*;
use main_loop::*;
use pytest::*;
pub use runners::runner_for;
pub use runners::runner_for_file;
pub use runners::runners_for;
use runners::*;
use runs::*;
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
/// many requests the app has cancelled. The app numbers its requests in
/// the order it sends them and the thread serves them in that order, so a
/// cancel covers the request in flight and any the worker has not picked
/// up yet, and never a later one.
#[derive(Clone, Default)]
pub struct RunControl {
    /// The streaming child's pid, which is its process group (it is
    /// spawned as a group leader).
    running: Arc<Mutex<Option<i32>>>,
    /// Requests numbered up to this one are cancelled.
    cancelled_upto: Arc<AtomicU64>,
}

/// A request as the worker thread takes it, numbered and tagged with the
/// root it was sent under by the app. The thread used to count both again
/// on its side, and a fresh thread after a crash (#2592) would have
/// started again from zero: every request numbered up to the last cancel
/// would have read as cancelled, and every answer as from an old root.
struct Sent {
    /// Requests are numbered from 1 in the order they were sent.
    number: u64,
    /// The root the request was sent under: its answers carry it.
    epoch: u64,
    request: TestRequest,
}

pub struct TestWorker {
    /// The thread the runs happen on, started again when it crashes
    /// (#2592).
    worker: Worker<Sent, (u64, TestResponse)>,
    /// Reaches the runner the worker thread is blocked on (#1534).
    control: RunControl,
    /// The runner list every thread the worker runs on shares, for tests
    /// to read.
    #[cfg(test)]
    owners: Arc<Owners>,
    /// Requests sent so far: the number of the last one.
    sent: u64,
    /// A cancel is waiting for its run's end, which the drain reports as
    /// cancelled rather than as a verdict.
    cancel_pending: bool,
    /// A quiet discovery is queued and has not started yet (#1841). Until
    /// the worker reaches it the panel is not busy, so without this every
    /// reopen of the Testing view would queue another.
    discover_queued: bool,
    /// Bumped on every [`Self::set_root`]. Every request carries the epoch
    /// it was sent under and its answers come back tagged with it, so the
    /// drain drops what a run still streaming when the Explorer re-roots
    /// sends for the old project (same idea as the commit graph's
    /// root-tagged drain).
    expected_epoch: u64,
    /// The root, `codeql` program and extensions folder the worker was last
    /// given, given again to a fresh thread after a crash (#2592).
    root: PathBuf,
    codeql: Option<PathBuf>,
    extensions: Option<PathBuf>,
    /// The worker stopped since the last [`Self::take_stopped`].
    stopped_news: bool,
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
    owners: Option<&'a Owners>,
    tag: Option<Runner>,
}

/// Which runner listed or ran each test (#1408). It lives outside the
/// worker's thread, so a thread started again after a crash still sends a
/// test to the runner that listed it (#2592), and is cleared only when the
/// root changes.
#[derive(Default)]
pub(super) struct Owners(Mutex<OwnerMap>);

#[derive(Default)]
pub(super) struct OwnerMap {
    /// The root, by its epoch, the map is for.
    epoch: u64,
    by_test: BTreeMap<String, Runner>,
}

#[cfg(test)]
mod tests;
