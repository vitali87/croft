//! Background test-runner worker. Mirrors [`crate::app::git_worker`]: a thread
//! owns the channels, runs `cargo test` off the render loop, streams parsed
//! cases plus raw output, and the app drains the results into the panel each
//! tick. Requests: `RunAll` (execute) and `Discover` (`cargo test -- --list`,
//! populate the tree without running); per-test granularity and other
//! ecosystems come in later milestones.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};

use super::model::{Activity, TestCase, TestStatus};
use super::parse::{
    parse_list_line, parse_pytest_collect_line, parse_pytest_line, parse_test_line,
};
use crate::output::{self, OutputLevel};
use crate::widgets::testing::TestingPanel;

/// Which test tool a workspace uses, detected from its manifest files. Cargo
/// wins when both manifests exist (a mixed repo's root is usually the crate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runner {
    Cargo,
    Pytest,
}

/// The manifest files that mark a Python project pytest can run in.
const PYTHON_MARKERS: [&str; 5] = [
    "pyproject.toml",
    "pytest.ini",
    "setup.cfg",
    "setup.py",
    "tox.ini",
];

/// Detect the workspace's test runner. `None` means no recognised test project,
/// so the Testing view stays empty instead of shelling a tool that would error.
pub fn runner_for(root: &Path) -> Option<Runner> {
    if root.join("Cargo.toml").is_file() {
        return Some(Runner::Cargo);
    }
    PYTHON_MARKERS
        .iter()
        .any(|m| root.join(m).is_file())
        .then_some(Runner::Pytest)
}

pub enum TestRequest {
    RunAll,
    /// Run a single test by its exact name (click-to-run from the tree).
    RunOne(String),
    /// Run every test whose name contains the string (a suite, or run-at-cursor
    /// by function name). cargo's name filter is a substring match.
    RunFilter(String),
    Discover,
    /// Rebind the worker's working directory (Explorer re-root). Without it the
    /// worker keeps shelling cargo in the launch dir captured at spawn, so after
    /// a Make Root into a child repo `cargo test -- --list` errors with "could
    /// not find Cargo.toml" and the tree never populates.
    SetRoot(PathBuf),
}

pub enum TestResponse {
    Started(Activity),
    Case(TestCase),
    /// A cargo build-status line (e.g. "Compiling ratatui v0.29") to show as
    /// live progress while the test binary compiles, so a multi-minute
    /// discovery doesn't look frozen behind a static "Discovering tests".
    Progress(String),
    /// `ok` is the runner's exit success for a run, `None` for discovery.
    Finished {
        ok: Option<bool>,
    },
}

pub struct TestWorker {
    request_tx: Sender<TestRequest>,
    /// Responses arrive tagged with the epoch of the root they ran under; the
    /// drain drops tags older than [`Self::expected_epoch`] so a run still
    /// streaming when the Explorer re-roots can't pollute the new project's
    /// tree (same idea as the commit graph's root-tagged drain).
    response_rx: Receiver<(u64, TestResponse)>,
    /// Bumped on every [`Self::set_root`], in lockstep with the loop's own
    /// counter: the request channel is FIFO, so both sides count the same
    /// `SetRoot`s in the same order.
    expected_epoch: u64,
    // Mirror of the root the loop last saw, for tests that assert the re-root
    // wiring. The loop owns its own copy via `SetRoot`; prod never reads this.
    #[cfg(test)]
    root: PathBuf,
}

impl TestWorker {
    pub fn spawn(workspace_root: PathBuf) -> Self {
        let (request_tx, request_rx) = std::sync::mpsc::channel::<TestRequest>();
        let (response_tx, response_rx) = std::sync::mpsc::channel::<(u64, TestResponse)>();
        #[cfg(test)]
        let root = workspace_root.clone();
        std::thread::spawn(move || worker_loop(workspace_root, request_rx, response_tx));
        Self {
            request_tx,
            response_rx,
            expected_epoch: 0,
            #[cfg(test)]
            root,
        }
    }

    /// Build a worker around hand-made channels (no thread) so tests can
    /// inject tagged responses straight into the drain.
    #[cfg(test)]
    fn for_test() -> (Self, Sender<(u64, TestResponse)>) {
        let (request_tx, _request_rx) = std::sync::mpsc::channel::<TestRequest>();
        let (response_tx, response_rx) = std::sync::mpsc::channel::<(u64, TestResponse)>();
        (
            Self {
                request_tx,
                response_rx,
                expected_epoch: 0,
                root: PathBuf::new(),
            },
            response_tx,
        )
    }

    pub fn run_all(&self) {
        let _ = self.request_tx.send(TestRequest::RunAll);
    }

    pub fn run_one(&self, name: String) {
        let _ = self.request_tx.send(TestRequest::RunOne(name));
    }

    pub fn run_filter(&self, pattern: String) {
        let _ = self.request_tx.send(TestRequest::RunFilter(pattern));
    }

    pub fn discover(&self) {
        let _ = self.request_tx.send(TestRequest::Discover);
    }

    /// Rebind the worker to a new workspace root after an Explorer re-root.
    /// Everything still streaming for the old root carries the old epoch and
    /// is dropped by [`Self::drain`].
    pub fn set_root(&mut self, root: PathBuf) {
        #[cfg(test)]
        {
            self.root = root.clone();
        }
        self.expected_epoch += 1;
        let _ = self.request_tx.send(TestRequest::SetRoot(root));
    }

    #[cfg(test)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Drain streamed results into the panel. Returns true iff anything was
    /// applied, so the main loop only redraws on a real update. Responses
    /// tagged with an epoch older than the last `set_root` belong to the
    /// previous project and are dropped.
    pub fn drain(&mut self, panel: &mut TestingPanel) -> bool {
        let mut changed = false;
        while let Ok((epoch, resp)) = self.response_rx.try_recv() {
            if epoch != self.expected_epoch {
                continue;
            }
            match resp {
                TestResponse::Started(activity) => panel.on_busy_started(activity),
                TestResponse::Case(case) => panel.apply_case(case),
                TestResponse::Progress(line) => panel.set_progress(line),
                TestResponse::Finished { ok } => panel.on_finished(ok),
            }
            changed = true;
        }
        changed
    }
}

/// A response sender bound to the epoch of the request it serves, so every
/// line a handler streams is tagged without threading the counter through.
struct EpochTx<'a> {
    tx: &'a Sender<(u64, TestResponse)>,
    epoch: u64,
}

impl EpochTx<'_> {
    fn send(&self, resp: TestResponse) {
        let _ = self.tx.send((self.epoch, resp));
    }

    /// An owned clone for the stderr tee thread.
    fn to_owned(&self) -> (Sender<(u64, TestResponse)>, u64) {
        (self.tx.clone(), self.epoch)
    }
}

fn worker_loop(mut root: PathBuf, rx: Receiver<TestRequest>, tx: Sender<(u64, TestResponse)>) {
    let mut epoch = 0u64;
    while let Ok(req) = rx.recv() {
        let etx = EpochTx { tx: &tx, epoch };
        match req {
            TestRequest::RunAll => run_all(&root, &etx),
            TestRequest::RunOne(name) => run_one(&root, &etx, &name),
            TestRequest::RunFilter(pattern) => run_filter(&root, &etx, &pattern),
            TestRequest::Discover => discover(&root, &etx),
            TestRequest::SetRoot(p) => {
                root = p;
                epoch += 1;
            }
        }
    }
}

/// `cargo <args>` rooted at the workspace, with piped stdio. cargo is resolved
/// by absolute path (GUI-launched croft inherits a stripped PATH) and its own
/// dir is prepended to the child PATH so the rustup shim finds its sibling
/// `rustc` (same fix as the dependencies fetcher).
fn cargo_cmd(root: &Path, args: &[&str]) -> Command {
    let cargo = crate::widgets::dependencies::cargo_binary();
    let mut cmd = Command::new(&cargo);
    cmd.args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cargo.parent()
        && let Some(path_var) = std::env::var_os("PATH")
    {
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(&path_var));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }
    cmd
}

/// Absolute path to the `pytest` binary for a workspace. The project's own
/// `.venv` wins (uv and venv projects put it there, with the project's deps
/// importable); then `PATH`, then the usual user/tool install dirs — resolved
/// absolutely like [`cargo_cmd`] because a GUI-launched croft inherits the
/// stripped launchd PATH.
fn pytest_binary(root: &Path) -> PathBuf {
    let venv = root.join(".venv").join("bin").join("pytest");
    if venv.is_file() {
        return venv;
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("pytest");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home)
            .join(".local")
            .join("bin")
            .join("pytest");
        if candidate.is_file() {
            return candidate;
        }
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin"] {
        let candidate = PathBuf::from(dir).join("pytest");
        if candidate.is_file() {
            return candidate;
        }
    }
    // Last resort: let the OS resolve it and fail honestly into the empty state.
    PathBuf::from("pytest")
}

/// `pytest <args>` rooted at the workspace, with piped stdio.
fn pytest_cmd(root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(pytest_binary(root));
    cmd.args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Spawn `cmd`, tee stderr (compile diagnostics) to the OUTPUT channel, and run
/// each stdout line through `parse` — every matched [`TestCase`] is streamed as
/// it arrives. Returns the child's exit success, or `None` if it never spawned.
fn run_streaming(
    tx: &EpochTx,
    mut cmd: Command,
    parse: impl Fn(&str) -> Option<TestCase>,
) -> Option<bool> {
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            output::push(
                output::CHANNEL_TESTS,
                OutputLevel::Error,
                &format!("failed to spawn the test runner: {e}"),
            );
            return None;
        }
    };
    let stderr_handle = child.stderr.take().map(|err| {
        let (tx, epoch) = tx.to_owned();
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                output::push(output::CHANNEL_TESTS, OutputLevel::Info, &line);
                if let Some(p) = cargo_progress(&line) {
                    let _ = tx.send((epoch, TestResponse::Progress(p)));
                }
            }
        })
    });
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            output::push(output::CHANNEL_TESTS, OutputLevel::Info, &line);
            if let Some(case) = parse(&line) {
                tx.send(TestResponse::Case(case));
            }
        }
    }
    if let Some(h) = stderr_handle {
        let _ = h.join();
    }
    Some(child.wait().map(|s| s.success()).unwrap_or(false))
}

/// The cargo build-status verbs printed to stderr (whitespace-indented on a
/// non-TTY). We surface these as live progress; everything else (diagnostics,
/// the `--list` chrome) stays in the OUTPUT channel only.
const CARGO_PROGRESS_VERBS: [&str; 6] = [
    "Compiling",
    "Building",
    "Downloading",
    "Updating",
    "Finished",
    "Running",
];

/// Turn a cargo stderr line into a short progress string (trimmed), or `None`
/// if it is not a build-status line.
pub(crate) fn cargo_progress(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let verb = trimmed.split_whitespace().next()?;
    CARGO_PROGRESS_VERBS
        .contains(&verb)
        .then(|| trimmed.to_string())
}

fn run_all(root: &Path, tx: &EpochTx) {
    tx.send(TestResponse::Started(Activity::Running));
    let ok = match runner_for(root) {
        Some(Runner::Pytest) => {
            let cmd = pytest_cmd(root, &["-v", "--color=no"]);
            run_streaming(tx, cmd, parse_pytest_line)
        }
        _ => {
            let cmd = cargo_cmd(root, &["test", "--no-fail-fast", "--color=never"]);
            run_streaming(tx, cmd, parse_test_line)
        }
    }
    .unwrap_or(false);
    tx.send(TestResponse::Finished { ok: Some(ok) });
}

/// Run a single test by exact name: `cargo test <name> --color=never -- --exact`
/// (`--exact` stops the name being treated as a substring filter), or for
/// pytest the node ID itself, which is already exact. No `Started` is sent: the
/// app marks just this case Running and keeps the rest of the tree, so a
/// single-test run doesn't wipe the discovered list.
fn run_one(root: &Path, tx: &EpochTx, name: &str) {
    let ok = match runner_for(root) {
        Some(Runner::Pytest) => {
            let cmd = pytest_cmd(root, &["-v", "--color=no", name]);
            run_streaming(tx, cmd, parse_pytest_line)
        }
        _ => {
            let cmd = cargo_cmd(root, &["test", name, "--color=never", "--", "--exact"]);
            run_streaming(tx, cmd, parse_test_line)
        }
    }
    .unwrap_or(false);
    tx.send(TestResponse::Finished { ok: Some(ok) });
}

/// Run every test matching a name filter. cargo's positional filter is a
/// substring match, so the suite prefix or a bare fn name both work. pytest
/// splits the two shapes: a suite is a node-ID prefix (`tests/test_x.py`,
/// `tests/test_x.py::TestGroup`) passed positionally, a bare function name
/// (run-at-cursor) goes through `-k`, pytest's substring matcher. Like
/// [`run_one`], the app has already marked the affected cases and shown the
/// busy state, so no `Started` is sent.
fn run_filter(root: &Path, tx: &EpochTx, pattern: &str) {
    let ok = match runner_for(root) {
        Some(Runner::Pytest) => {
            let cmd = if pattern.contains(".py") {
                pytest_cmd(root, &["-v", "--color=no", pattern])
            } else {
                pytest_cmd(root, &["-v", "--color=no", "-k", pattern])
            };
            run_streaming(tx, cmd, parse_pytest_line)
        }
        _ => {
            let cmd = cargo_cmd(root, &["test", pattern, "--no-fail-fast", "--color=never"]);
            run_streaming(tx, cmd, parse_test_line)
        }
    }
    .unwrap_or(false);
    tx.send(TestResponse::Finished { ok: Some(ok) });
}

/// List tests without running them (`cargo test -- --list`, or pytest's
/// `--collect-only -q`), streaming each as a `NotRun` case. The cargo path
/// still compiles the test binary, hence the Discovering state.
fn discover(root: &Path, tx: &EpochTx) {
    tx.send(TestResponse::Started(Activity::Discovering));
    match runner_for(root) {
        Some(Runner::Pytest) => {
            let cmd = pytest_cmd(root, &["--collect-only", "-q", "--color=no"]);
            run_streaming(tx, cmd, |line| {
                parse_pytest_collect_line(line).map(|name| TestCase {
                    name,
                    status: TestStatus::NotRun,
                })
            });
        }
        _ => {
            let cmd = cargo_cmd(root, &["test", "--color=never", "--", "--list"]);
            run_streaming(tx, cmd, |line| {
                parse_list_line(line).map(|name| TestCase {
                    name,
                    status: TestStatus::NotRun,
                })
            });
        }
    }
    tx.send(TestResponse::Finished { ok: None });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_detection_prefers_cargo_then_python_markers() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(runner_for(tmp.path()), None, "no manifest, no runner");
        std::fs::write(tmp.path().join("pyproject.toml"), "[project]\n").unwrap();
        assert_eq!(runner_for(tmp.path()), Some(Runner::Pytest));
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\n").unwrap();
        assert_eq!(runner_for(tmp.path()), Some(Runner::Cargo));

        let py = tempfile::tempdir().unwrap();
        std::fs::write(py.path().join("pytest.ini"), "[pytest]\n").unwrap();
        assert_eq!(runner_for(py.path()), Some(Runner::Pytest));
    }

    #[test]
    fn drain_drops_responses_from_before_the_last_set_root() {
        let (mut w, tx) = TestWorker::for_test();
        let mut panel = TestingPanel::new();
        // A run for the OLD root is still streaming when the Explorer re-roots.
        w.set_root(PathBuf::from("/new"));
        tx.send((
            0,
            TestResponse::Case(TestCase {
                name: String::from("old_project::stale"),
                status: TestStatus::Failed,
            }),
        ))
        .unwrap();
        tx.send((0, TestResponse::Finished { ok: Some(false) }))
            .unwrap();
        assert!(
            !w.drain(&mut panel),
            "stale-epoch responses must be dropped, not applied"
        );
        assert!(panel.is_empty(), "the old project's case never lands");

        // Responses for the new root (epoch 1) still flow.
        tx.send((
            1,
            TestResponse::Case(TestCase {
                name: String::from("new_project::fresh"),
                status: TestStatus::Passed,
            }),
        ))
        .unwrap();
        assert!(w.drain(&mut panel));
        assert!(!panel.is_empty());
    }
}
