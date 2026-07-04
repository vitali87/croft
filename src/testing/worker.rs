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
use super::parse::{parse_list_line, parse_test_line};
use crate::output::{self, OutputLevel};
use crate::widgets::testing::TestingPanel;

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
    response_rx: Receiver<TestResponse>,
    // Mirror of the root the loop last saw, for tests that assert the re-root
    // wiring. The loop owns its own copy via `SetRoot`; prod never reads this.
    #[cfg(test)]
    root: PathBuf,
}

impl TestWorker {
    pub fn spawn(workspace_root: PathBuf) -> Self {
        let (request_tx, request_rx) = std::sync::mpsc::channel::<TestRequest>();
        let (response_tx, response_rx) = std::sync::mpsc::channel::<TestResponse>();
        #[cfg(test)]
        let root = workspace_root.clone();
        std::thread::spawn(move || worker_loop(workspace_root, request_rx, response_tx));
        Self {
            request_tx,
            response_rx,
            #[cfg(test)]
            root,
        }
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
    pub fn set_root(&mut self, root: PathBuf) {
        #[cfg(test)]
        {
            self.root = root.clone();
        }
        let _ = self.request_tx.send(TestRequest::SetRoot(root));
    }

    #[cfg(test)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Drain streamed results into the panel. Returns true iff anything was
    /// applied, so the main loop only redraws on a real update.
    pub fn drain(&mut self, panel: &mut TestingPanel) -> bool {
        let mut changed = false;
        while let Ok(resp) = self.response_rx.try_recv() {
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

fn worker_loop(mut root: PathBuf, rx: Receiver<TestRequest>, tx: Sender<TestResponse>) {
    while let Ok(req) = rx.recv() {
        match req {
            TestRequest::RunAll => run_all(&root, &tx),
            TestRequest::RunOne(name) => run_one(&root, &tx, &name),
            TestRequest::RunFilter(pattern) => run_filter(&root, &tx, &pattern),
            TestRequest::Discover => discover(&root, &tx),
            TestRequest::SetRoot(p) => root = p,
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

/// Spawn `cmd`, tee stderr (compile diagnostics) to the OUTPUT channel, and run
/// each stdout line through `parse` — every matched [`TestCase`] is streamed as
/// it arrives. Returns the child's exit success, or `None` if it never spawned.
fn run_streaming(
    tx: &Sender<TestResponse>,
    mut cmd: Command,
    parse: impl Fn(&str) -> Option<TestCase>,
) -> Option<bool> {
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            output::push(
                output::CHANNEL_TESTS,
                OutputLevel::Error,
                &format!("failed to spawn cargo: {e}"),
            );
            return None;
        }
    };
    let stderr_handle = child.stderr.take().map(|err| {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                output::push(output::CHANNEL_TESTS, OutputLevel::Info, &line);
                if let Some(p) = cargo_progress(&line) {
                    let _ = tx.send(TestResponse::Progress(p));
                }
            }
        })
    });
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            output::push(output::CHANNEL_TESTS, OutputLevel::Info, &line);
            if let Some(case) = parse(&line) {
                let _ = tx.send(TestResponse::Case(case));
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

fn run_all(root: &Path, tx: &Sender<TestResponse>) {
    let _ = tx.send(TestResponse::Started(Activity::Running));
    let cmd = cargo_cmd(root, &["test", "--no-fail-fast", "--color=never"]);
    let ok = run_streaming(tx, cmd, parse_test_line).unwrap_or(false);
    let _ = tx.send(TestResponse::Finished { ok: Some(ok) });
}

/// Run a single test by exact name: `cargo test <name> --color=never -- --exact`.
/// `--exact` stops the name being treated as a substring filter. No `Started` is
/// sent: the app marks just this case Running and keeps the rest of the tree, so
/// a single-test run doesn't wipe the discovered list.
fn run_one(root: &Path, tx: &Sender<TestResponse>, name: &str) {
    let cmd = cargo_cmd(root, &["test", name, "--color=never", "--", "--exact"]);
    let ok = run_streaming(tx, cmd, parse_test_line).unwrap_or(false);
    let _ = tx.send(TestResponse::Finished { ok: Some(ok) });
}

/// Run every test matching a name substring: `cargo test <pattern> --no-fail-fast`
/// (no `--exact`, so it selects a whole suite / all fns sharing the name). Like
/// [`run_one`], the app has already marked the affected cases and shown the busy
/// state, so no `Started` is sent.
fn run_filter(root: &Path, tx: &Sender<TestResponse>, pattern: &str) {
    let cmd = cargo_cmd(root, &["test", pattern, "--no-fail-fast", "--color=never"]);
    let ok = run_streaming(tx, cmd, parse_test_line).unwrap_or(false);
    let _ = tx.send(TestResponse::Finished { ok: Some(ok) });
}

/// List tests without running them (`cargo test -- --list`), streaming each as a
/// `NotRun` case. Still compiles the test binary, hence the Discovering state.
fn discover(root: &Path, tx: &Sender<TestResponse>) {
    let _ = tx.send(TestResponse::Started(Activity::Discovering));
    let cmd = cargo_cmd(root, &["test", "--color=never", "--", "--list"]);
    run_streaming(tx, cmd, |line| {
        parse_list_line(line).map(|name| TestCase {
            name,
            status: TestStatus::NotRun,
        })
    });
    let _ = tx.send(TestResponse::Finished { ok: None });
}
