//! Background test-runner worker. Mirrors [`crate::app::git_worker`]: a thread
//! owns the channels, runs `cargo test` off the render loop, streams parsed
//! cases plus raw output, and the app drains the results into the panel each
//! tick. M1 supports one request, `RunAll`; granularity and other ecosystems
//! come in later milestones.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};

use super::model::TestCase;
use super::parse::parse_test_line;
use crate::output::{self, OutputLevel};
use crate::widgets::testing::TestingPanel;

pub enum TestRequest {
    RunAll,
}

pub enum TestResponse {
    Started,
    Case(TestCase),
    Finished { ok: bool },
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

    /// Drain streamed results into the panel. Returns true iff anything was
    /// applied, so the main loop only redraws on a real update.
    pub fn drain(&mut self, panel: &mut TestingPanel) -> bool {
        let mut changed = false;
        while let Ok(resp) = self.response_rx.try_recv() {
            match resp {
                TestResponse::Started => panel.on_run_started(),
                TestResponse::Case(case) => panel.apply_case(case),
                TestResponse::Finished { ok } => panel.on_run_finished(ok),
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
        }
    }
}

fn run_all(root: &Path, tx: &Sender<TestResponse>) {
    let _ = tx.send(TestResponse::Started);
    let cargo = crate::widgets::dependencies::cargo_binary();
    let mut cmd = Command::new(&cargo);
    cmd.args(["test", "--no-fail-fast", "--color=never"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Prepend cargo's own dir so the rustup shim finds its sibling `rustc` even
    // under a GUI-stripped PATH (same fix as the dependencies fetcher).
    if let Some(dir) = cargo.parent()
        && let Some(path_var) = std::env::var_os("PATH")
    {
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(&path_var));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            output::push(
                output::CHANNEL_TESTS,
                OutputLevel::Error,
                &format!("failed to spawn cargo test: {e}"),
            );
            let _ = tx.send(TestResponse::Finished { ok: false });
            return;
        }
    };

    // Compile diagnostics and warnings go to stderr; tee them to the channel on
    // a side thread so a build failure is visible in OUTPUT, then join below.
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
            if let Some(case) = parse_test_line(&line) {
                let _ = tx.send(TestResponse::Case(case));
            }
        }
    }
    if let Some(h) = stderr_handle {
        let _ = h.join();
    }

    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    let _ = tx.send(TestResponse::Finished { ok });
}
