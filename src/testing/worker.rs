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
    Discover,
}

pub enum TestResponse {
    Started(Activity),
    Case(TestCase),
    /// `ok` is the runner's exit success for a run, `None` for discovery.
    Finished {
        ok: Option<bool>,
    },
}

pub struct TestWorker {
    request_tx: Sender<TestRequest>,
    response_rx: Receiver<TestResponse>,
}

impl TestWorker {
    pub fn spawn(workspace_root: PathBuf) -> Self {
        let (request_tx, request_rx) = std::sync::mpsc::channel::<TestRequest>();
        let (response_tx, response_rx) = std::sync::mpsc::channel::<TestResponse>();
        std::thread::spawn(move || worker_loop(workspace_root, request_rx, response_tx));
        Self {
            request_tx,
            response_rx,
        }
    }

    pub fn run_all(&self) {
        let _ = self.request_tx.send(TestRequest::RunAll);
    }

    pub fn discover(&self) {
        let _ = self.request_tx.send(TestRequest::Discover);
    }

    /// Drain streamed results into the panel. Returns true iff anything was
    /// applied, so the main loop only redraws on a real update.
    pub fn drain(&mut self, panel: &mut TestingPanel) -> bool {
        let mut changed = false;
        while let Ok(resp) = self.response_rx.try_recv() {
            match resp {
                TestResponse::Started(activity) => panel.on_busy_started(activity),
                TestResponse::Case(case) => panel.apply_case(case),
                TestResponse::Finished { ok } => panel.on_finished(ok),
            }
            changed = true;
        }
        changed
    }
}

fn worker_loop(root: PathBuf, rx: Receiver<TestRequest>, tx: Sender<TestResponse>) {
    while let Ok(req) = rx.recv() {
        match req {
            TestRequest::RunAll => run_all(&root, &tx),
            TestRequest::Discover => discover(&root, &tx),
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
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                output::push(output::CHANNEL_TESTS, OutputLevel::Info, &line);
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

fn run_all(root: &Path, tx: &Sender<TestResponse>) {
    let _ = tx.send(TestResponse::Started(Activity::Running));
    let cmd = cargo_cmd(root, &["test", "--no-fail-fast", "--color=never"]);
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
