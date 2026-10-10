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
    parse_jest_json, parse_list_line, parse_pytest_collect_line, parse_pytest_line,
    parse_test_line, parse_vitest_list_line, parse_vitest_tap_line,
};
use super::regex_escape;
use crate::output::{self, OutputLevel};
use crate::widgets::testing::TestingPanel;

/// Which built-in run mechanism a workspace's tests use. Which one a given
/// root resolves to is manifest data ([`super::registry`]), not code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runner {
    Cargo,
    Pytest,
    Vitest,
    Jest,
    Go,
    Codeql,
}

/// Detect the workspace's test runner from the enabled extensions'
/// `[[test_runners]]` declarations. `None` means no recognised test project,
/// so the Testing view stays empty instead of shelling a tool that would error.
pub fn runner_for(root: &Path) -> Option<Runner> {
    super::registry::runner_for(root)
}

pub enum TestRequest {
    RunAll,
    /// Run a single test by its exact name (click-to-run from the tree).
    RunOne(String),
    /// Run every test whose name contains the string (run-at-cursor by
    /// function name). cargo's name filter is a substring match.
    RunFilter(String),
    /// Run a whole suite by its unanchored name (a header click). The cargo
    /// arm anchors it with [`super::suite_pattern`] so `parse` cannot sweep
    /// `parse_utils::b`; pytest gets it positionally as a node-ID prefix.
    RunSuite(String),
    /// Run with the runner's coverage tool on (#263): everything, or only
    /// the tests `scope` names.
    RunCoverage(Option<CoverageScope>),
    Discover,
    /// Rebind the worker's working directory (Explorer re-root). Without it the
    /// worker keeps shelling cargo in the launch dir captured at spawn, so after
    /// a Make Root into a child repo `cargo test -- --list` errors with "could
    /// not find Cargo.toml" and the tree never populates.
    SetRoot(PathBuf),
    /// Rebind the `codeql` program CodeQL test runs shell: the app's own, so
    /// the Testing view and the CodeQL side bar run the same CLI (and a test
    /// can stand a script in for it).
    SetCodeqlProgram(PathBuf),
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
    /// A coverage run's report, or why there is none. Sent before its
    /// `Finished`.
    Coverage(Result<super::coverage::Coverage, CoverageError>),
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

    pub fn run_coverage(&self) {
        let _ = self.request_tx.send(TestRequest::RunCoverage(None));
    }

    /// Run only the tests `scope` names with coverage on (#263).
    pub fn run_coverage_scoped(&self, scope: CoverageScope) {
        let _ = self.request_tx.send(TestRequest::RunCoverage(Some(scope)));
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

    pub fn run_suite(&self, suite: String) {
        let _ = self.request_tx.send(TestRequest::RunSuite(suite));
    }

    pub fn discover(&self) {
        let _ = self.request_tx.send(TestRequest::Discover);
    }

    pub fn set_codeql_program(&self, program: PathBuf) {
        let _ = self.request_tx.send(TestRequest::SetCodeqlProgram(program));
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
                TestResponse::Refused => panel.on_refused(),
                TestResponse::Coverage(result) => panel.on_coverage(result),
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
    /// The `codeql` program a CodeQL test run shells (see
    /// [`TestRequest::SetCodeqlProgram`]); carried here because every
    /// handler already takes the sender.
    codeql: &'a Path,
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
    // Found on PATH, as the CodeQL side bar finds it, until the app says
    // otherwise.
    let mut codeql = PathBuf::from("codeql");
    while let Ok(req) = rx.recv() {
        let etx = EpochTx {
            tx: &tx,
            epoch,
            codeql: &codeql,
        };
        match req {
            TestRequest::RunAll => run_all(&root, &etx),
            TestRequest::RunCoverage(scope) => run_coverage(&root, &etx, scope.as_ref()),
            TestRequest::RunOne(name) => run_one(&root, &etx, &name),
            TestRequest::RunFilter(pattern) => run_filter(&root, &etx, &pattern, false),
            TestRequest::RunSuite(suite) => run_filter(&root, &etx, &suite, true),
            TestRequest::Discover => discover(&root, &etx),
            TestRequest::SetRoot(p) => {
                root = p;
                epoch += 1;
            }
            TestRequest::SetCodeqlProgram(p) => codeql = p,
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

/// How to launch pytest for a workspace: a program, and the arguments that
/// come before pytest's own. A project venv (`.venv/`, then `venv/`) with
/// pytest installed (its `pytest` script, or a python that imports it) wins, run as `<venv>/bin/python -m pytest` the way VS
/// Code runs it: `-m` puts the root on `sys.path`, so tests import the
/// project's own top-level modules, which the `pytest` script alone does
/// not (#845). Without one, `python3 -m pytest` when that `python3` can
/// import pytest: a `pytest` script on `PATH` may belong to another
/// interpreter (a uv tool's), which cannot import the project's
/// dependencies. Then the `pytest` script; with no script anywhere,
/// `python3 -m pytest`, which at least fails with a reason instead of a
/// spawn error.
fn pytest_launch(root: &Path) -> (PathBuf, &'static [&'static str]) {
    pytest_launch_on(root, std::env::var_os("PATH").as_deref())
}

/// [`pytest_launch`] against the given `PATH` value.
fn pytest_launch_on(
    root: &Path,
    path_var: Option<&std::ffi::OsStr>,
) -> (PathBuf, &'static [&'static str]) {
    const MODULE: &[&str] = &["-m", "pytest"];
    for venv in [".venv", "venv"] {
        let bin = root.join(venv).join("bin");
        let python = bin.join("python");
        // `-m pytest` needs the module: the console script says it is
        // installed without a process, and without the script the venv's
        // python is asked.
        if python.is_file() && (bin.join("pytest").is_file() || python_has_pytest(root, &python)) {
            return (python, MODULE);
        }
    }
    let python3 = python3_program(path_var);
    if python_has_pytest(root, &python3) {
        return (python3, MODULE);
    }
    match pytest_script(path_var) {
        Some(script) => (script, &[]),
        None => (python3, MODULE),
    }
}

/// Absolute path to `python3`: `PATH`, then Homebrew's and the usual
/// local prefix, for the GUI-stripped-PATH reason [`pytest_script`]
/// gives. Bare `python3` when none is found.
fn python3_program(path_var: Option<&std::ffi::OsStr>) -> PathBuf {
    let path_dirs = path_var.map(std::env::split_paths).into_iter().flatten();
    let fallback = ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from);
    path_dirs
        .chain(fallback)
        .map(|dir| dir.join("python3"))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from("python3"))
}

/// Whether `python` can import pytest when run in `root`, asked once per
/// root and interpreter for the life of the process: discovery and every
/// run launch pytest, and the answer only changes when pytest is
/// installed or removed.
fn python_has_pytest(root: &Path, python: &Path) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static ANSWERS: OnceLock<Mutex<HashMap<(PathBuf, PathBuf), bool>>> = OnceLock::new();
    let key = (root.to_path_buf(), python.to_path_buf());
    let answers = ANSWERS.get_or_init(Default::default);
    if let Some(&known) = answers.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return known;
    }
    let has = Command::new(python)
        .args(["-c", "import pytest"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    answers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, has);
    has
}

/// Absolute path to a `pytest` script outside any project venv: `PATH`,
/// then the usual user/tool install dirs — resolved absolutely like
/// [`cargo_cmd`] because a GUI-launched croft inherits the stripped
/// launchd PATH.
fn pytest_script(path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    if let Some(path_var) = path_var {
        for dir in std::env::split_paths(path_var) {
            let candidate = dir.join("pytest");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home)
            .join(".local")
            .join("bin")
            .join("pytest");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin"] {
        let candidate = PathBuf::from(dir).join("pytest");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// `pytest <args>` rooted at the workspace, launched as [`pytest_launch`]
/// says, with piped stdio. Discovery, runs and coverage all come through
/// here, so they always agree on the interpreter.
fn pytest_cmd(root: &Path, args: &[&str]) -> Command {
    let (program, lead) = pytest_launch(root);
    let mut cmd = Command::new(program);
    cmd.args(lead)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Absolute path to `go`: `PATH`, then where the official installer and
/// Homebrew put it — absolute for the same GUI-stripped-PATH reason as
/// [`cargo_cmd`].
fn go_binary() -> PathBuf {
    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    path_dirs
        .into_iter()
        .chain(["/usr/local/go/bin", "/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from))
        .map(|d| d.join("go"))
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("go"))
}

/// `go test -json <args>` in `root` (#264).
fn go_cmd<S: AsRef<std::ffi::OsStr>>(root: &Path, args: &[S]) -> Command {
    let mut cmd = Command::new(go_binary());
    cmd.args(["test", "-json"])
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Run `go test -json` and stream its finished tests, showing the text the
/// tests printed (not the JSON events) in the OUTPUT channel.
fn run_go<S: AsRef<std::ffi::OsStr>>(tx: &EpochTx, root: &Path, args: &[S]) -> Option<bool> {
    let module = super::gotest::module_path(root);
    run_streaming_shown(tx, go_cmd(root, args), super::gotest::shown, |line| {
        super::gotest::parse_event(module.as_deref(), line)
            .into_iter()
            .collect()
    })
}

/// Run `codeql <args>` in `root` and stream each test's result (#578). A
/// result is complete once the diff or errors after its line have arrived,
/// so it lands when the next test's line (or the end of output) does; each
/// failure also gets a one-line summary in OUTPUT, above the CLI's own
/// diff-heavy text.
fn run_codeql(tx: &EpochTx, root: &Path, args: &[String]) -> Option<bool> {
    let mut cmd = Command::new(tx.codeql);
    cmd.args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let stream = std::cell::RefCell::new(super::codeqltest::RunStream::new(root));
    let report = |o: super::codeqltest::Outcome| {
        if let Some(summary) = o.failure_summary() {
            output::push(output::CHANNEL_TESTS, OutputLevel::Error, &summary);
        }
        o.case()
    };
    let ok = run_streaming(tx, cmd, |line| {
        stream
            .borrow_mut()
            .feed(line)
            .map(report)
            .into_iter()
            .collect()
    });
    if let Some(o) = stream.borrow_mut().finish() {
        tx.send(TestResponse::Case(report(o)));
    }
    ok
}

/// Absolute path to a JS test runner binary for a workspace: the project's own
/// `node_modules/.bin/<name>` first (the installed version, with the project's
/// config resolvable), then `PATH`, then the usual global dirs — absolute for
/// the same GUI-stripped-PATH reason as [`cargo_cmd`].
fn js_binary(root: &Path, name: &str) -> PathBuf {
    let local = root.join("node_modules").join(".bin").join(name);
    if local.is_file() {
        return local;
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin"] {
        let candidate = PathBuf::from(dir).join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(name)
}

/// `<vitest|jest> <args>` rooted at the workspace, with piped stdio.
/// `NO_COLOR` strips ANSI from the parsed stream and `CI` keeps vitest out of
/// watch/interactive mode.
fn js_cmd<S: AsRef<std::ffi::OsStr>>(root: &Path, runner: &str, args: &[S]) -> Command {
    let mut cmd = Command::new(js_binary(root, runner));
    cmd.args(args)
        .current_dir(root)
        .env("NO_COLOR", "1")
        .env("CI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Split a croft JS node ID (`file::describe...::test`) into its file and an
/// optional title: the first segment is always the test file, the last (when
/// present) the name filter to hand `-t`.
fn js_id_parts(id: &str) -> (&str, Option<&str>) {
    match id.split_once("::") {
        Some((file, rest)) => (file, rest.rsplit("::").next()),
        None => (id, None),
    }
}

/// Whether a run-filter pattern is a node-ID (prefix) rooted at a test file,
/// as opposed to a bare title from run-at-cursor.
fn is_js_file(pattern: &str) -> bool {
    super::parse::is_js_test_file(js_id_parts(pattern).0)
}

/// One test-harness executable out of `cargo test --no-run`'s JSON: its
/// path, the target that built it, the target's root source file, and
/// whether that target is an integration test (kind `test`, one harness per
/// `tests/*.rs` — or per explicit `[[test]]` entry, which can rename the
/// target and point it anywhere; `src_path` is the only reliable link back
/// to the file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestBinary {
    pub path: PathBuf,
    pub target: String,
    pub src_path: PathBuf,
    pub integration: bool,
}

/// The test-profile executables from `cargo test --no-run
/// --message-format=json` output: compiler-artifact lines whose profile is
/// `test` and whose `executable` is set. Ranked lib first (unit tests
/// overwhelmingly live in the lib), then bin, then integration `test`
/// targets, preserving cargo's order within a rank. A src/lib.rs +
/// src/main.rs crate emits both a lib and a bin harness; the old last-wins
/// pick handed a lib test to the bin harness, which filters everything out
/// and exits before a breakpoint can bind.
pub fn test_binary_candidates(output: &str) -> Vec<TestBinary> {
    let mut ranked: Vec<(u8, TestBinary)> = Vec::new();
    for line in output.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact")
            || v.get("profile")
                .and_then(|p| p.get("test"))
                .and_then(|t| t.as_bool())
                != Some(true)
        {
            continue;
        }
        let Some(exe) = v.get("executable").and_then(|e| e.as_str()) else {
            continue;
        };
        let target = v.get("target");
        let has_kind = |want: &str| {
            target
                .and_then(|t| t.get("kind"))
                .and_then(|k| k.as_array())
                .is_some_and(|kinds| kinds.iter().filter_map(|k| k.as_str()).any(|k| k == want))
        };
        let kind_rank = if has_kind("lib") {
            0
        } else if has_kind("bin") {
            1
        } else {
            2
        };
        ranked.push((
            kind_rank,
            TestBinary {
                path: PathBuf::from(exe),
                target: target
                    .and_then(|t| t.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string(),
                src_path: PathBuf::from(
                    target
                        .and_then(|t| t.get("src_path"))
                        .and_then(|s| s.as_str())
                        .unwrap_or(""),
                ),
                integration: has_kind("test"),
            },
        ));
    }
    ranked.sort_by_key(|(r, _)| *r); // stable: cargo order kept within a rank
    ranked.into_iter().map(|(_, c)| c).collect()
}

/// Which candidate harness actually contains `name`: each is asked to
/// `--list` with the name as libtest's filter (fast — nothing runs), and the
/// first listing a test whose final `::` segment IS the name wins. The
/// filter alone is a substring match, so `lib_side_test` would otherwise
/// claim a harness that only knows `lib_side_test_extra`. cargo's JSON does
/// not say which target defines a test.
pub fn binary_containing_test(candidates: &[PathBuf], name: &str) -> Option<PathBuf> {
    let qualified = format!("::{name}");
    for exe in candidates {
        let Ok(out) = Command::new(exe)
            .args([name, "--list"])
            .stdin(Stdio::null())
            .output()
        else {
            continue;
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let listed = stdout.lines().any(|l| {
            l.trim_end()
                .strip_suffix(": test")
                .is_some_and(|t| t == name || t.ends_with(&qualified))
        });
        if listed {
            return Some(exe.clone());
        }
    }
    None
}

/// Build (or reuse) the workspace's test binaries and pick the harness that
/// contains `name`: `cargo test --no-run --message-format=json`, then a
/// `--list` probe when more than one harness could own the test. A nonzero
/// cargo exit is a build failure even if some targets' executables were
/// already emitted — launching a partial build would debug stale code
/// instead of surfacing the compile error. `source` (the file the debug
/// gesture happened in) narrows the harnesses first: a file that IS a
/// target's `src_path` owns that target outright (exact even for a renamed
/// `[[test]] path = ...` target the file stem cannot predict); a module
/// file narrows to its side of the build (`tests/` → integration harnesses,
/// anything else → lib/bin) and never widens further — a lib-first probe
/// over every harness is how a unit test used to steal an integration
/// test's bare name. Blocking — the app runs this on a background thread
/// and launches from the drain.
pub fn build_test_binary(
    root: &Path,
    name: &str,
    source: Option<&Path>,
) -> std::io::Result<PathBuf> {
    let out = cargo_cmd(root, &["test", "--no-run", "--message-format=json"]).output()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "cargo test --no-run failed: {}",
            stderr
                .lines()
                .rev()
                .find(|l| l.starts_with("error"))
                .or_else(|| stderr.lines().last())
                .unwrap_or("")
                .trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let all = test_binary_candidates(&stdout);
    if all.is_empty() {
        return Err(std::io::Error::other(format!(
            "no test binary in cargo's build output: {}",
            stderr.lines().last().unwrap_or("")
        )));
    }
    let pool: Vec<&TestBinary> = match source {
        Some(src) => {
            // Canonicalize both sides: cargo reports resolved paths
            // (/private/var/... on macOS) while the editor may hold the
            // symlinked spelling of the same file.
            let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            let src = canon(src);
            let exact: Vec<&TestBinary> =
                all.iter().filter(|c| canon(&c.src_path) == src).collect();
            if exact.is_empty() {
                let is_integration_src = src.strip_prefix(canon(root)).is_ok_and(|rel| {
                    rel.components()
                        .next()
                        .is_some_and(|c| c.as_os_str() == "tests")
                });
                let side: Vec<&TestBinary> = all
                    .iter()
                    .filter(|c| c.integration == is_integration_src)
                    .collect();
                if side.is_empty() {
                    return Err(std::io::Error::other(format!(
                        "no {} harness in the build owns {}",
                        if is_integration_src {
                            "integration-test"
                        } else {
                            "unit-test"
                        },
                        src.display()
                    )));
                }
                side
            } else {
                exact
            }
        }
        None => all.iter().collect(),
    };
    match pool.as_slice() {
        [only] => Ok(only.path.clone()),
        _ => {
            let paths: Vec<PathBuf> = pool.iter().map(|c| c.path.clone()).collect();
            binary_containing_test(&paths, name).ok_or_else(|| {
                std::io::Error::other(format!(
                    "none of the {} test harnesses lists a test named {name}",
                    paths.len()
                ))
            })
        }
    }
}

/// vitest argv for one exact test: the file scopes the run and `-t` narrows
/// to the test. vitest's `-t/--testNamePattern` is jest-compatible — a
/// REGEX over the full name — so the name is escaped like jest's, and
/// anchored (see [`exact_title_anchor`]).
fn vitest_one_args(name: &str) -> Vec<String> {
    let mut args = vec![String::from("run"), js_id_parts(name).0.to_string()];
    if let Some(anchor) = exact_title_anchor(name) {
        args.push(String::from("-t"));
        args.push(anchor);
    }
    args.push(String::from("--reporter=tap-flat"));
    args
}

/// `-t` regex for one test (#1506): its describe chain and title, escaped,
/// joined with [`JS_NAME_JOIN`] the way vitest and jest build the full name
/// they search `-t` in, and anchored at both ends. The bare title is a
/// search too, so `works` also ran `works with negatives` and a `works` in
/// another describe. `None` for an ID that is only a file.
fn exact_title_anchor(id: &str) -> Option<String> {
    full_name_regex(id).map(|name| format!("^{name}$"))
}

/// What joins a describe and the next title in the full name `-t` is
/// matched against: a space for jest and vitest before 5, ` > ` from vitest
/// 5 on. A title can itself hold `::` (`connects to ::1`), which the node
/// ID cannot tell from a boundary, so `::` matches at a boundary too.
const JS_NAME_JOIN: &str = "(?: | > |::)";

/// The describe and title separator alone, closing a suite's chain.
const JS_SUITE_JOIN: &str = "(?: | > )";

/// The describe chain and title of a JS node ID as the escaped full name
/// the runners match `-t` against; `None` for a bare file.
fn full_name_regex(id: &str) -> Option<String> {
    let (_, rest) = id.split_once("::")?;
    Some(
        rest.split("::")
            .map(regex_escape)
            .collect::<Vec<_>>()
            .join(JS_NAME_JOIN),
    )
}

/// `-t` regex for a suite click's describe chain: every segment after the
/// file, escaped and joined the way vitest and jest build the full name they
/// match `-t` against (describes + title joined by a space, or ` > ` from
/// vitest 5 — vitest's `getTaskFullName`, jest's `getTestID`), anchored at
/// the start and closed with the separator so describe `auth` cannot sweep
/// an `auth-helper` sibling. `None` when the suite is the whole file (no describe segments),
/// where the file argument alone scopes the run.
fn suite_title_anchor(pattern: &str) -> Option<String> {
    full_name_regex(pattern).map(|joined| format!("^{joined}{JS_SUITE_JOIN}"))
}

/// vitest argv for a filter run: a suite click passes a node-ID prefix
/// (`file` or `file::describe`), run-at-cursor a bare title. The file scopes
/// the run when present; a describe chain (anchored, see
/// [`suite_title_anchor`]) or the bare title narrows via `-t`.
fn vitest_filter_args(pattern: &str, suite: bool) -> Vec<String> {
    let mut args = vec![String::from("run")];
    let (file, title) = js_id_parts(pattern);
    if is_js_file(pattern) {
        args.push(file.to_string());
        if suite {
            if let Some(anchor) = suite_title_anchor(pattern) {
                args.push(String::from("-t"));
                args.push(anchor);
            }
        } else if let Some(t) = title {
            args.push(String::from("-t"));
            args.push(regex_escape(t));
        }
    } else {
        args.push(String::from("-t"));
        args.push(regex_escape(pattern));
    }
    args.push(String::from("--reporter=tap-flat"));
    args
}

/// jest's own way to name one test file (#1506). A bare positional file is
/// a `testPathPattern`, a regex searched in each absolute path, so
/// `src/a.test.js` also ran `legacy/src/a.test.js`; `--runTestsByPath`
/// takes it as that exact path.
fn jest_file_args(file: &str) -> [String; 2] {
    [String::from("--runTestsByPath"), file.to_string()]
}

/// jest argv for one exact test, mirroring [`vitest_one_args`]: the exact
/// file and an anchored `-t` (a regex over the full name) for the test.
fn jest_one_args(name: &str) -> Vec<String> {
    let mut args = jest_file_args(js_id_parts(name).0).to_vec();
    if let Some(anchor) = exact_title_anchor(name) {
        args.push(String::from("-t"));
        args.push(anchor);
    }
    args.push(String::from("--json"));
    args
}

/// jest argv for a filter run, mirroring [`vitest_filter_args`]: a node-ID
/// prefix scopes by file (plus an anchored `-t` for a suite's describe
/// chain), a bare title goes through `-t` alone.
fn jest_filter_args(pattern: &str, suite: bool) -> Vec<String> {
    let mut args = Vec::new();
    if is_js_file(pattern) {
        let (file, title) = js_id_parts(pattern);
        args.extend(jest_file_args(file));
        if suite {
            if let Some(anchor) = suite_title_anchor(pattern) {
                args.push(String::from("-t"));
                args.push(anchor);
            }
        } else if let Some(t) = title {
            args.push(String::from("-t"));
            args.push(regex_escape(t));
        }
    } else {
        args.push(String::from("-t"));
        args.push(regex_escape(pattern));
    }
    args.push(String::from("--json"));
    args
}

/// Adapt a one-line-one-case parser to [`run_streaming`]'s many-cases shape
/// (jest's `--json` yields every case from a single stdout line, so the
/// streaming contract is a `Vec` per line).
fn one(parse: fn(&str) -> Option<TestCase>) -> impl Fn(&str) -> Vec<TestCase> {
    move |line| parse(line).into_iter().collect()
}

/// Spawn `cmd`, tee stderr (compile diagnostics) to the OUTPUT channel, and run
/// each stdout line through `parse` — every matched [`TestCase`] is streamed as
/// it arrives. Returns the child's exit success, or `None` if it never spawned.
fn run_streaming(
    tx: &EpochTx,
    cmd: Command,
    parse: impl Fn(&str) -> Vec<TestCase>,
) -> Option<bool> {
    run_streaming_shown(tx, cmd, |line| Some(line.to_string()), parse)
}

/// [`run_streaming`], with `show` choosing what of each stdout line the
/// OUTPUT channel gets (`None` hides it): `go test -json` wraps the tests'
/// own text in events.
fn run_streaming_shown(
    tx: &EpochTx,
    cmd: Command,
    show: impl Fn(&str) -> Option<String>,
    parse: impl Fn(&str) -> Vec<TestCase>,
) -> Option<bool> {
    run_streaming_exit(tx, cmd, show, parse).map(|code| code == Some(0))
}

/// [`run_streaming_shown`], answering the child's exit code rather than
/// its success, for a caller that reads a nonzero code as something other
/// than failure. `Some(None)` when it died to a signal or could not be
/// waited on, `None` if it never spawned.
fn run_streaming_exit(
    tx: &EpochTx,
    mut cmd: Command,
    show: impl Fn(&str) -> Option<String>,
    parse: impl Fn(&str) -> Vec<TestCase>,
) -> Option<Option<i32>> {
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
            if let Some(text) = show(&line) {
                output::push(output::CHANNEL_TESTS, OutputLevel::Info, &text);
            }
            for case in parse(&line) {
                tx.send(TestResponse::Case(case));
            }
        }
    }
    if let Some(h) = stderr_handle {
        let _ = h.join();
    }
    Some(child.wait().ok().and_then(|s| s.code()))
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
    // `None` (no enabled runner claims the root) must never fall through to
    // cargo: the app's entry points refuse first, and this second gate keeps
    // a disabled runner from shelling anything even if a new call site
    // forgets the check. It must also run BEFORE `Started`, which clears the
    // discovered tree — a refusal keeps the panel exactly as it was.
    // Exhaustive matches below, no `_` arm, so a future Runner variant is a
    // compile error here instead of silently cargo.
    let Some(runner) = runner_for(root) else {
        tx.send(TestResponse::Refused);
        return;
    };
    tx.send(TestResponse::Started(Activity::Running));
    let ok = match runner {
        Runner::Pytest => {
            let cmd = pytest_cmd(root, &["-v", "--color=no"]);
            run_streaming(tx, cmd, one(parse_pytest_line))
        }
        Runner::Vitest => {
            let cmd = js_cmd(root, "vitest", &["run", "--reporter=tap-flat"]);
            run_streaming(tx, cmd, one(parse_vitest_tap_line))
        }
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
            let packs = super::codeqltest::test_packs(root, &codeql_markers());
            match super::codeqltest::all_args(root, &packs) {
                Some(args) => run_codeql(tx, root, &args),
                // No test pack: nothing to run, and nothing failed.
                None => Some(true),
            }
        }
    }
    .unwrap_or(false);
    tx.send(TestResponse::Finished { ok: Some(ok) });
}

/// The pack files the bundled CodeQL runner looks for. A run needs the test
/// packs again, not just the verdict that there is one.
fn codeql_markers() -> Vec<String> {
    vec![String::from("qlpack.yml"), String::from("codeql-pack.yml")]
}

/// The report every runner is asked to write: LCOV, into `dir`.
fn coverage_report(dir: &Path) -> PathBuf {
    dir.join("lcov.info")
}

/// Which tests a coverage run covers, when not all of them (#263): `name`
/// as run-at-cursor resolved it, `exact` when it is a full test name
/// rather than a substring filter, `suite` when it is a suite from the
/// Testing tree, selected as a plain run of that suite selects it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageScope {
    pub name: String,
    pub exact: bool,
    pub suite: bool,
}

/// The filter arguments that narrow a coverage run to `scope`: the same
/// selection a plain run of that test makes, less what [`coverage_args`]
/// already says (vitest's `run` and reporter, jest's `--json`).
fn coverage_scope_args(runner: Runner, scope: &CoverageScope) -> Vec<String> {
    let name = scope.name.as_str();
    let drop = |args: Vec<String>, skip: &[&str]| {
        args.into_iter()
            .filter(|a| !skip.contains(&a.as_str()) && !a.starts_with("--reporter="))
            .collect::<Vec<_>>()
    };
    match runner {
        Runner::Pytest => {
            if scope.exact || scope.suite || name.contains(".py") {
                vec![name.to_string()]
            } else {
                vec![String::from("-k"), name.to_string()]
            }
        }
        Runner::Vitest => drop(
            if scope.exact {
                vitest_one_args(name)
            } else {
                vitest_filter_args(name, scope.suite)
            },
            &["run"],
        ),
        Runner::Jest => drop(
            if scope.exact {
                jest_one_args(name)
            } else {
                jest_filter_args(name, scope.suite)
            },
            &["--json"],
        ),
        // `cargo llvm-cov [TESTNAME] [-- <libtest args>]`, as `cargo test`,
        // a suite anchored as its plain run anchors it.
        Runner::Cargo => {
            let name = if scope.suite {
                super::suite_pattern(name)
            } else {
                name.to_string()
            };
            let mut a = vec![name];
            if scope.exact {
                a.push(String::from("--"));
                a.push(String::from("--exact"));
            }
            a
        }
        Runner::Go => {
            if scope.exact {
                super::gotest::one_args(name)
            } else {
                super::gotest::filter_args(name, scope.suite)
            }
        }
        // Refused before any argv is built (see run_coverage).
        Runner::Codeql => Vec::new(),
    }
}

/// The run-everything argv with coverage on, writing LCOV into `dir`. Each
/// keeps the output format the normal run parses, so results stream into
/// the tree as usual.
fn coverage_args(runner: Runner, dir: &Path) -> Vec<String> {
    let dir_s = dir.display().to_string();
    let report = coverage_report(dir).display().to_string();
    let v = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    match runner {
        Runner::Pytest => {
            let mut a = v(&["-v", "--color=no", "--cov=.", "--cov-branch"]);
            a.push(format!("--cov-report=lcov:{report}"));
            a
        }
        Runner::Vitest => {
            let mut a = v(&[
                "run",
                "--reporter=tap-flat",
                "--coverage.enabled",
                "--coverage.reporter=lcov",
            ]);
            a.push(format!("--coverage.reportsDirectory={dir_s}"));
            a
        }
        Runner::Jest => {
            let mut a = v(&["--json", "--coverage", "--coverageReporters=lcov"]);
            a.push(format!("--coverageDirectory={dir_s}"));
            a
        }
        Runner::Cargo => {
            let mut a = v(&[
                "llvm-cov",
                "--no-fail-fast",
                "--color=never",
                "--lcov",
                "--output-path",
            ]);
            a.push(report);
            a
        }
        // A Go cover profile, turned into `report` once the run ends.
        Runner::Go => vec![format!("-coverprofile={}", go_cover_profile(dir).display())],
        Runner::Codeql => Vec::new(),
    }
}

/// Where a Go coverage run writes its native profile.
fn go_cover_profile(dir: &Path) -> PathBuf {
    dir.join("cover.out")
}

/// What installs a runner's coverage tool, picked from the project's own
/// package manager. `None` for jest, whose coverage is built in.
fn coverage_install(runner: Runner, root: &Path) -> Option<(&'static str, String)> {
    let has = |f: &str| root.join(f).exists();
    match runner {
        Runner::Cargo => Some((
            "cargo-llvm-cov",
            String::from("cargo install cargo-llvm-cov"),
        )),
        Runner::Pytest => Some((
            "pytest-cov",
            if has("uv.lock") {
                String::from("uv add --dev pytest-cov")
            } else if has(".venv") {
                String::from(".venv/bin/python -m pip install pytest-cov")
            } else {
                String::from("python3 -m pip install pytest-cov")
            },
        )),
        Runner::Vitest => Some((
            "@vitest/coverage-v8",
            if has("pnpm-lock.yaml") {
                String::from("pnpm add -D @vitest/coverage-v8")
            } else if has("yarn.lock") {
                String::from("yarn add -D @vitest/coverage-v8")
            } else {
                String::from("npm install -D @vitest/coverage-v8")
            },
        )),
        // Coverage is built into `go test`; CodeQL has none to install.
        Runner::Jest | Runner::Go | Runner::Codeql => None,
    }
}

/// Whether the runner's coverage tool can run here.
fn coverage_tool_present(runner: Runner, root: &Path) -> bool {
    match runner {
        Runner::Cargo => cargo_cmd(root, &["llvm-cov", "--version"])
            .status()
            .is_ok_and(|s| s.success()),
        Runner::Pytest => pytest_cmd(root, &["--help"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("--cov")),
        // Node resolves a package from the nearest `node_modules` up the
        // tree, so a monorepo's hoisted install at the repository root counts.
        Runner::Vitest => root.ancestors().any(|dir| {
            ["coverage-v8", "coverage-istanbul"]
                .iter()
                .any(|p| dir.join("node_modules/@vitest").join(p).is_dir())
        }),
        Runner::Jest | Runner::Go | Runner::Codeql => true,
    }
}

/// Run everything with coverage (#263). A missing tool is reported before
/// anything runs, with the command that installs it; the tree is left as
/// it was.
fn run_coverage(root: &Path, tx: &EpochTx, scope: Option<&CoverageScope>) {
    let Some(runner) = runner_for(root) else {
        tx.send(TestResponse::Refused);
        return;
    };
    // A query test has no source lines of its own to cover.
    if runner == Runner::Codeql {
        tx.send(TestResponse::Coverage(Err(CoverageError::Unsupported {
            runner: "CodeQL tests",
        })));
        tx.send(TestResponse::Finished { ok: None });
        return;
    }
    if !coverage_tool_present(runner, root)
        && let Some((tool, install)) = coverage_install(runner, root)
    {
        {
            tx.send(TestResponse::Coverage(Err(CoverageError::Missing {
                tool,
                install,
            })));
            tx.send(TestResponse::Finished { ok: None });
            return;
        }
    }
    let dir = std::env::temp_dir().join(format!("croft-coverage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    // A scoped run, like `run_one`, leaves the rest of the tree as it is:
    // the app has already marked what it runs.
    if scope.is_none() {
        tx.send(TestResponse::Started(Activity::Running));
    }
    let mut args = coverage_args(runner, &dir);
    match scope {
        Some(scope) => args.extend(coverage_scope_args(runner, scope)),
        // The other runners run everything by default; `go test` only the
        // package in the current directory.
        None if runner == Runner::Go => args.push(String::from("./...")),
        None => {}
    }

    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let ok = match runner {
        Runner::Pytest => run_streaming(tx, pytest_cmd(root, &args), one(parse_pytest_line)),
        Runner::Vitest => run_streaming(
            tx,
            js_cmd(root, "vitest", &args),
            one(parse_vitest_tap_line),
        ),
        Runner::Jest => run_streaming(tx, js_cmd(root, "jest", &args), |line| {
            parse_jest_json(root, line)
        }),
        Runner::Cargo => run_streaming(tx, cargo_cmd(root, &args), one(parse_test_line)),
        Runner::Go => run_go(tx, root, &args),
        Runner::Codeql => None,
    }
    .unwrap_or(false);
    let lcov = if runner == Runner::Go {
        std::fs::read_to_string(go_cover_profile(&dir)).map(|profile| {
            let module = super::gotest::module_path(root);
            super::gotest::cover_profile_to_lcov(&profile, module.as_deref())
        })
    } else {
        std::fs::read_to_string(coverage_report(&dir))
    };
    let report = lcov
        .map(|text| super::coverage::Coverage::from_lcov(&text, root))
        .map_err(|_| CoverageError::NoReport);
    tx.send(TestResponse::Coverage(report));
    tx.send(TestResponse::Finished { ok: Some(ok) });
}

/// Run a single test by exact name: `cargo test <name> --color=never -- --exact`
/// (`--exact` stops the name being treated as a substring filter), or for
/// pytest the node ID itself, which is already exact. No `Started` is sent: the
/// app marks just this case Running and keeps the rest of the tree, so a
/// single-test run doesn't wipe the discovered list.
fn run_one(root: &Path, tx: &EpochTx, name: &str) {
    // See run_all: `None` refuses instead of falling through to cargo, and
    // `Refused` (not a bare Finished) rolls back this case's Running mark.
    let Some(runner) = runner_for(root) else {
        tx.send(TestResponse::Refused);
        return;
    };
    let ok = match runner {
        Runner::Pytest => {
            let cmd = pytest_cmd(root, &["-v", "--color=no", name]);
            run_streaming(tx, cmd, one(parse_pytest_line))
        }
        Runner::Vitest => {
            let cmd = js_cmd(root, "vitest", &vitest_one_args(name));
            run_streaming(tx, cmd, one(parse_vitest_tap_line))
        }
        Runner::Jest => {
            let cmd = js_cmd(root, "jest", &jest_one_args(name));
            run_streaming(tx, cmd, |line| parse_jest_json(root, line))
        }
        Runner::Cargo => {
            let cmd = cargo_cmd(root, &["test", name, "--color=never", "--", "--exact"]);
            run_streaming(tx, cmd, one(parse_test_line))
        }
        Runner::Go => run_go(tx, root, &super::gotest::one_args(name)),
        Runner::Codeql => run_codeql(tx, root, &super::codeqltest::select_args(name)),
    }
    .unwrap_or(false);
    tx.send(TestResponse::Finished { ok: Some(ok) });
}

/// Run every test matching a name filter. `suite` marks a header click: the
/// cargo arm then anchors the pattern with [`super::suite_pattern`], since a
/// bare substring like `parse` would also sweep `parse_utils::b`. pytest
/// splits the two shapes: a suite is a node-ID prefix (`tests/test_x.py`,
/// `tests/test_x.py::TestGroup`) passed positionally, a bare function name
/// (run-at-cursor) goes through `-k`, pytest's substring matcher. vitest and
/// jest anchor a suite's describe chain (see [`suite_title_anchor`]), since
/// their `-t` is an unanchored regex over the full name. Like [`run_one`],
/// the app has already marked the affected cases and shown the busy state,
/// so no `Started` is sent.
fn run_filter(root: &Path, tx: &EpochTx, pattern: &str, suite: bool) {
    // See run_all: `None` refuses instead of falling through to cargo, and
    // `Refused` (not a bare Finished) rolls back the filtered Running marks.
    let Some(runner) = runner_for(root) else {
        tx.send(TestResponse::Refused);
        return;
    };
    let ok = match runner {
        Runner::Pytest => {
            // A suite is always a node-ID prefix; the `.py` sniff keeps a
            // node-ID handed to a plain filter run positional too.
            let cmd = if suite || pattern.contains(".py") {
                pytest_cmd(root, &["-v", "--color=no", pattern])
            } else {
                pytest_cmd(root, &["-v", "--color=no", "-k", pattern])
            };
            run_streaming(tx, cmd, one(parse_pytest_line))
        }
        Runner::Vitest => {
            let cmd = js_cmd(root, "vitest", &vitest_filter_args(pattern, suite));
            run_streaming(tx, cmd, one(parse_vitest_tap_line))
        }
        Runner::Jest => {
            let cmd = js_cmd(root, "jest", &jest_filter_args(pattern, suite));
            run_streaming(tx, cmd, |line| parse_jest_json(root, line))
        }
        Runner::Cargo => {
            let anchored;
            let pattern = if suite {
                anchored = super::suite_pattern(pattern);
                &anchored
            } else {
                pattern
            };
            let cmd = cargo_cmd(root, &["test", pattern, "--no-fail-fast", "--color=never"]);
            run_streaming(tx, cmd, one(parse_test_line))
        }
        Runner::Go => run_go(tx, root, &super::gotest::filter_args(pattern, suite)),
        // A suite is its folder; any other filter is taken as the path (or
        // id) of what to run.
        Runner::Codeql => run_codeql(tx, root, &super::codeqltest::select_args(pattern)),
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
fn discover(root: &Path, tx: &EpochTx) {
    // See run_all: refuse before `Started` so the panel keeps its tree.
    let Some(runner) = runner_for(root) else {
        tx.send(TestResponse::Refused);
        return;
    };
    tx.send(TestResponse::Started(Activity::Discovering));
    let not_run = |name: String| TestCase {
        name,
        status: TestStatus::NotRun,
    };
    let ok = match runner {
        Runner::Pytest => {
            let cmd = pytest_cmd(root, &["--collect-only", "-q", "--color=no"]);
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
            let module = super::gotest::module_path(root);
            let cmd = go_cmd(root, &["-list", ".", "./..."]);
            run_streaming_shown(tx, cmd, super::gotest::shown, |line| {
                super::gotest::parse_list_event(module.as_deref(), line)
                    .map(not_run)
                    .into_iter()
                    .collect()
            })
        }
        // Tests are files; listing them needs no CLI, and cannot fail.
        Runner::Codeql => {
            let packs = super::codeqltest::test_packs(root, &codeql_markers());
            for id in super::codeqltest::discover(root, &packs) {
                tx.send(TestResponse::Case(not_run(id)));
            }
            Some(true)
        }
    }
    // A lister that never spawned (pytest not installed) failed too.
    .unwrap_or(false);
    tx.send(TestResponse::Finished { ok: Some(ok) });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runner disabled between the app's entry-point check and the worker
    /// picking the queued request up must refuse WITHOUT wiping panel state:
    /// `Started(Running)` clears the discovered tree, and a bare
    /// `Finished{ok: None}` after `start_single`/`start_filter` leaves those
    /// cases stranded as Running forever (T-Rex reproduced both).
    #[test]
    fn refusing_a_run_with_no_runner_never_wipes_or_strands_the_panel() {
        let tmp = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let etx = EpochTx {
            tx: &tx,
            epoch: 0,
            codeql: Path::new("codeql"),
        };
        run_all(tmp.path(), &etx);
        run_one(tmp.path(), &etx, "a::b");
        run_filter(tmp.path(), &etx, "a", false);
        run_filter(tmp.path(), &etx, "a", true);
        discover(tmp.path(), &etx);
        let msgs: Vec<TestResponse> = rx.try_iter().map(|(_, r)| r).collect();
        assert!(
            !msgs.iter().any(|m| matches!(m, TestResponse::Started(_))),
            "a refused run must not send Started: it wipes the discovered tree"
        );
        assert_eq!(
            msgs.iter()
                .filter(|m| matches!(m, TestResponse::Refused))
                .count(),
            5,
            "each refused request must answer with Refused, not a bare Finished"
        );
        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, TestResponse::Finished { .. })),
            "a bare Finished after start_single/start_filter strands cases Running"
        );
    }

    /// A stand-in `codeql` in `dir` that logs its arguments and prints
    /// `out` (`@ROOT@` replaced by `root`), exiting nonzero when it reports
    /// a failure, as `codeql test run` does.
    #[cfg(unix)]
    fn fake_codeql(dir: &Path, root: &Path, out: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let code = i32::from(out.contains("FAILED"));
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\ncat <<'EOF'\n{out}EOF\nexit {code}\n",
            log = dir.join("calls.log").display(),
            out = out.replace("@ROOT@", &root.display().to_string()),
        );
        let bin = dir.join("codeql");
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    /// CodeQL tests (#578) end to end against a fake CLI: discovery lists
    /// each test file without running anything, a run of the test pack
    /// reports the pass and the diff failure, one test runs by its file,
    /// a suite by its folder, and coverage is refused as unavailable.
    #[cfg(unix)]
    #[test]
    fn codeql_tests_discover_run_and_refuse_coverage_with_a_fake_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let write = |rel: &str, text: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        write(
            "test/qlpack.yml",
            "name: acme/tests\nextractor: javascript\ntests: .\n",
        );
        write("test/Find/Find.qlref", "Find.ql\n");
        write("test/Find/Find.expected", "");
        write("test/Bad/Bad.ql", "select 1");
        write("test/Bad/Bad.expected", "");
        let bin = tempfile::tempdir().unwrap();
        let codeql = fake_codeql(
            bin.path(),
            root,
            "Executing 2 tests in 2 directories.\n\
             [1/2 comp 1.1s eval 20ms] PASSED @ROOT@/test/Find/Find.qlref\n\
             [2/2 comp 1.0s eval 18ms] FAILED(RESULT) @ROOT@/test/Bad/Bad.ql\n\
             --- expected\n+++ actual\n@@ -0,0 +1 @@\n+| 1 |\n\
             1 tests passed; 1 tests failed:\n  FAILED: @ROOT@/test/Bad/Bad.ql\n",
        );
        assert_eq!(runner_for(root), Some(Runner::Codeql));
        let (tx, rx) = std::sync::mpsc::channel();
        let etx = EpochTx {
            tx: &tx,
            epoch: 0,
            codeql: &codeql,
        };
        let drain = |rx: &Receiver<(u64, TestResponse)>| {
            let mut cases = Vec::new();
            let mut finished = None;
            for (_, r) in rx.try_iter() {
                match r {
                    TestResponse::Case(c) => cases.push((c.name, c.status)),
                    TestResponse::Finished { ok } => finished = Some(ok),
                    _ => {}
                }
            }
            (cases, finished)
        };
        let log = || std::fs::read_to_string(bin.path().join("calls.log")).unwrap_or_default();

        discover(root, &etx);
        let (cases, finished) = drain(&rx);
        assert_eq!(
            cases,
            vec![
                (String::from("test/Bad::Bad.ql"), TestStatus::NotRun),
                (String::from("test/Find::Find.qlref"), TestStatus::NotRun),
            ]
        );
        assert_eq!(finished, Some(Some(true)), "listing files cannot fail");
        assert_eq!(log(), "", "discovery never shells the CLI");

        run_all(root, &etx);
        let (cases, finished) = drain(&rx);
        assert_eq!(
            cases,
            vec![
                (String::from("test/Find::Find.qlref"), TestStatus::Passed),
                (String::from("test/Bad::Bad.ql"), TestStatus::Failed),
            ]
        );
        assert_eq!(finished, Some(Some(false)), "a failing test fails the run");
        assert_eq!(log(), "test run test\n", "run all runs the test pack");

        run_one(root, &etx, "test/Find::Find.qlref");
        run_filter(root, &etx, "test/Bad", true);
        let _ = drain(&rx);
        assert!(
            log().ends_with("test run test/Find/Find.qlref\ntest run test/Bad\n"),
            "{}",
            log()
        );

        run_coverage(root, &etx, None);
        let msgs: Vec<TestResponse> = rx.try_iter().map(|(_, r)| r).collect();
        assert!(matches!(
            msgs.as_slice(),
            [
                TestResponse::Coverage(Err(CoverageError::Unsupported { .. })),
                TestResponse::Finished { ok: None }
            ]
        ));
    }

    /// End to end against a real `go` (#264): discovery lists each
    /// package's tests, a run reports pass/fail/skip and subtests, one test
    /// runs alone, and coverage paints the package's source. Needs Go, so it
    /// is ignored by default; run it with `--ignored`.
    #[test]
    #[ignore]
    fn go_test_discovers_runs_and_covers_a_real_module() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("go.mod"), "module example.com/m\n\ngo 1.21\n").unwrap();
        std::fs::create_dir(root.join("foo")).unwrap();
        std::fs::write(
            root.join("foo/foo.go"),
            "package foo\n\nfunc Add(a, b int) int {\n\tif a > 100 {\n\t\treturn 0\n\t}\n\treturn a + b\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("foo/foo_test.go"),
            "package foo\n\nimport \"testing\"\n\n\
             func TestAdd(t *testing.T) {\n\tt.Run(\"small one\", func(t *testing.T) {\n\t\tif Add(1, 2) != 3 {\n\t\t\tt.Fatal(\"bad\")\n\t\t}\n\t})\n}\n\n\
             func TestBad(t *testing.T) { t.Fatal(\"nope\") }\n\n\
             func TestSkip(t *testing.T) { t.Skip(\"later\") }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("root_test.go"),
            "package m\n\nimport \"testing\"\n\nfunc TestRoot(t *testing.T) {}\n",
        )
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let etx = EpochTx {
            tx: &tx,
            epoch: 0,
            codeql: Path::new("codeql"),
        };
        let cases = |rx: &Receiver<(u64, TestResponse)>| {
            let mut v: Vec<(String, TestStatus)> = rx
                .try_iter()
                .filter_map(|(_, r)| match r {
                    TestResponse::Case(c) => Some((c.name, c.status)),
                    _ => None,
                })
                .collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        };
        use TestStatus::*;
        let s = String::from;

        discover(root, &etx);
        assert_eq!(
            cases(&rx),
            [
                (s("./foo::TestAdd"), NotRun),
                (s("./foo::TestBad"), NotRun),
                (s("./foo::TestSkip"), NotRun),
                (s(".::TestRoot"), NotRun),
            ]
        );
        run_all(root, &etx);
        assert_eq!(
            cases(&rx),
            [
                (s("./foo::TestAdd"), Passed),
                (s("./foo::TestAdd/small_one"), Passed),
                (s("./foo::TestBad"), Failed),
                (s("./foo::TestSkip"), Skipped),
                (s(".::TestRoot"), Passed),
            ]
        );
        run_one(root, &etx, "./foo::TestAdd/small_one");
        assert_eq!(
            cases(&rx),
            [
                (s("./foo::TestAdd"), Passed),
                (s("./foo::TestAdd/small_one"), Passed),
            ]
        );
        run_filter(root, &etx, "./foo", true);
        assert_eq!(cases(&rx).len(), 4, "the whole package");

        run_coverage(root, &etx, None);
        let report = rx
            .try_iter()
            .find_map(|(_, r)| match r {
                TestResponse::Coverage(c) => Some(c),
                _ => None,
            })
            .expect("a coverage report")
            .expect("go writes a profile");
        let file = report
            .files
            .get(&root.join("foo/foo.go"))
            .expect("foo.go is covered");
        assert_eq!(file.hits.get(&7), Some(&1), "return a + b ran");
        assert_eq!(file.hits.get(&5), Some(&0), "return 0 never ran");
    }

    #[test]
    fn vitest_titles_are_regex_escaped_like_jests() {
        // vitest's `-t/--testNamePattern` is jest-compatible: a REGEX over
        // the full name. Passing a title raw turns `adds (1 + 1)` into a
        // capture group (0 tests match) and an unbalanced `[` into an
        // invalid-pattern error. The jest paths already escape; vitest must
        // treat the same flag the same way.
        let args = vitest_one_args("tests/math.test.js::math::adds (1 + 1)");
        assert_eq!(
            args,
            vec![
                "run",
                "tests/math.test.js",
                "-t",
                r"^math(?: | > |::)adds \(1 \+ 1\)$",
                "--reporter=tap-flat"
            ]
        );
        let args = vitest_filter_args("parses [ tokens", false);
        assert_eq!(
            args,
            vec!["run", "-t", r"parses \[ tokens", "--reporter=tap-flat"]
        );
        let args = vitest_filter_args("tests/a.test.js::group (x)", false);
        assert_eq!(
            args,
            vec![
                "run",
                "tests/a.test.js",
                "-t",
                r"group \(x\)",
                "--reporter=tap-flat"
            ]
        );
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

    #[test]
    fn cargo_json_artifact_lines_yield_the_unit_test_binary() {
        // Shapes captured from a real `cargo test --no-run --message-format=json`
        // run: one compiler-artifact line per target, `executable` set only on
        // test binaries. A src/lib.rs + src/main.rs crate emits BOTH a lib and
        // a bin harness; last-wins used to hand a lib test to the bin harness,
        // which filters everything out and exits before a breakpoint binds.
        // Candidates rank lib first (unit tests overwhelmingly live there),
        // then bin, then integration, preserving cargo order within a rank.
        let lines = [
            r#"{"reason":"compiler-artifact","target":{"kind":["test"],"name":"cli","src_path":"/p/tests/cli.rs"},"profile":{"test":true},"executable":"/p/target/debug/deps/cli-2a2d806b06669aa7"}"#,
            r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"croft"},"profile":{"test":true},"executable":"/p/target/debug/deps/croft-lib00"}"#,
            r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"croft"},"profile":{"test":true},"executable":"/p/target/debug/deps/croft-bin00"}"#,
            r#"{"reason":"build-finished","success":true}"#,
        ];
        let joined = lines.join("\n");
        let got = test_binary_candidates(&joined);
        assert_eq!(
            got.iter().map(|c| c.path.clone()).collect::<Vec<_>>(),
            vec![
                PathBuf::from("/p/target/debug/deps/croft-lib00"),
                PathBuf::from("/p/target/debug/deps/croft-bin00"),
                PathBuf::from("/p/target/debug/deps/cli-2a2d806b06669aa7"),
            ],
            "lib outranks bin outranks integration; nothing is dropped"
        );
        // Target identity survives the ranking: the integration harness knows
        // its target name so a source file under tests/ can select it.
        assert_eq!(got[2].target, "cli");
        assert_eq!(got[2].src_path, PathBuf::from("/p/tests/cli.rs"));
        assert!(got[2].integration);
        assert!(!got[0].integration && !got[1].integration);
        // Only an integration target: it is still returned.
        assert_eq!(
            test_binary_candidates(lines[0])
                .iter()
                .map(|c| c.path.clone())
                .collect::<Vec<_>>(),
            vec![PathBuf::from("/p/target/debug/deps/cli-2a2d806b06669aa7")]
        );
        assert!(test_binary_candidates(lines[3]).is_empty());
        assert!(test_binary_candidates("").is_empty());
    }

    /// cargo's JSON does not say which target defines a test; with several
    /// harnesses each is asked to `--list` the name (fast, runs nothing) and
    /// the first that knows it wins.
    #[test]
    fn probe_picks_the_harness_that_actually_lists_the_test() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let p = tmp.path().join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let empty = script("bin_harness", r#"echo "0 tests, 0 benchmarks""#);
        let has = script("lib_harness", r#"echo "module::lib_side_test: test""#);
        assert_eq!(
            binary_containing_test(&[empty.clone(), has.clone()], "lib_side_test"),
            Some(has),
            "the harness listing the test wins even when probed second"
        );
        assert_eq!(
            binary_containing_test(&[empty], "lib_side_test"),
            None,
            "no harness knows the test -> no pick"
        );
        // libtest's filter arg is a SUBSTRING match, so a harness whose only
        // hit is a longer name (`lib_side_test_extra`) still prints a
        // `…: test` line; the probe must compare the listed name's final
        // segment exactly, not just spot any listing.
        let superstring = script(
            "super_harness",
            r#"echo "module::lib_side_test_extra: test""#,
        );
        assert_eq!(
            binary_containing_test(&[superstring], "lib_side_test"),
            None,
            "a superstring name is not the test"
        );
    }

    #[test]
    fn a_failed_workspace_build_is_an_error_not_a_partial_binary() {
        // cargo can emit one member's test executable before another member
        // fails to compile. Debugging must surface the build error, never
        // launch the partial artifact as if the build succeeded.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"good\", \"bad\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        for (pkg, body) in [
            ("good", "#[test]\nfn probed_test() {}\n"),
            ("bad", "fn broken() { missing_symbol }\n"),
        ] {
            let dir = tmp.path().join(pkg);
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::write(
                dir.join("Cargo.toml"),
                format!("[package]\nname = \"{pkg}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n"),
            )
            .unwrap();
            std::fs::write(dir.join("src").join("lib.rs"), body).unwrap();
        }
        let err = build_test_binary(tmp.path(), "probed_test", None)
            .expect_err("a failed build must not yield a binary");
        assert!(
            err.to_string().contains("cargo"),
            "the error names the failed build, got: {err}"
        );
    }

    #[test]
    fn the_source_file_picks_the_harness_for_a_duplicate_test_name() {
        // A lib test and an integration test sharing a bare fn name: the
        // lib-first ranking used to run the lib's test when the gesture was
        // in tests/integ.rs. The gesture's file owns exactly one harness.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::create_dir_all(tmp.path().join("tests")).unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"dup\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("src").join("lib.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn same_name() {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("tests").join("integ.rs"),
            "#[test]\nfn same_name() {}\n",
        )
        .unwrap();
        let stem_of = |p: &PathBuf| p.file_name().unwrap().to_string_lossy().to_string();
        let from_integration = build_test_binary(
            tmp.path(),
            "same_name",
            Some(&tmp.path().join("tests").join("integ.rs")),
        )
        .unwrap();
        assert!(
            stem_of(&from_integration).starts_with("integ"),
            "a tests/ file selects its integration harness, got {from_integration:?}"
        );
        let from_lib = build_test_binary(
            tmp.path(),
            "same_name",
            Some(&tmp.path().join("src").join("lib.rs")),
        )
        .unwrap();
        assert!(
            stem_of(&from_lib).starts_with("dup"),
            "a src/ file selects the lib harness, got {from_lib:?}"
        );
    }

    #[test]
    fn a_renamed_integration_target_still_owns_its_source_file() {
        // Explicit `[[test]]` entries can point a target at any path:
        // tests/custom_source.rs building target renamed_harness. Guessing
        // the target from the file stem found nothing, fell back to every
        // harness, and the lib-first probe handed the gesture to the unit
        // harness — whose exact filter then ran zero tests. cargo's JSON
        // knows each target's src_path; the gesture's file matches it
        // exactly, whatever the target is called.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::create_dir_all(tmp.path().join("tests")).unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            concat!(
                "[package]\nname = \"renamed\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
                "[[test]]\nname = \"renamed_harness\"\npath = \"tests/custom_source.rs\"\n",
            ),
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("src").join("lib.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn same_name() {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("tests").join("custom_source.rs"),
            "#[test]\nfn same_name() {}\n",
        )
        .unwrap();
        let picked = build_test_binary(
            tmp.path(),
            "same_name",
            Some(&tmp.path().join("tests").join("custom_source.rs")),
        )
        .unwrap();
        assert!(
            picked
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("renamed_harness"),
            "the [[test]] target owning the source wins, got {picked:?}"
        );
    }

    #[test]
    fn jest_args_mirror_the_vitest_builders() {
        // The jest argv used to be assembled inline at each call site; the
        // vitest escaping bug came exactly from that kind of divergence.
        assert_eq!(
            jest_one_args("tests/math.test.js::math::adds (1 + 1)"),
            vec![
                "--runTestsByPath",
                "tests/math.test.js",
                "-t",
                r"^math(?: | > |::)adds \(1 \+ 1\)$",
                "--json"
            ]
        );
        assert_eq!(
            jest_filter_args("parses [ tokens", false),
            vec!["-t", r"parses \[ tokens", "--json"]
        );
        assert_eq!(
            jest_filter_args("tests/a.test.js::group (x)", false),
            vec![
                "--runTestsByPath",
                "tests/a.test.js",
                "-t",
                r"group \(x\)",
                "--json"
            ]
        );
    }

    /// #1506: Run on one jest or vitest test names it by its whole describe
    /// chain, anchored at both ends, so `works` no longer also runs `works
    /// with negatives` or a `works` under another describe.
    #[test]
    fn one_js_test_is_named_by_its_whole_chain_anchored() {
        let id = "src/math.test.js::add::works";
        let jest = jest_one_args(id);
        assert_eq!(
            jest,
            vec![
                "--runTestsByPath",
                "src/math.test.js",
                "-t",
                "^add(?: | > |::)works$",
                "--json"
            ]
        );
        let vitest = vitest_one_args(id);
        assert_eq!(vitest[3], "^add(?: | > |::)works$", "{vitest:?}");
        let filter = regex::Regex::new(&jest[3]).unwrap();
        assert!(filter.is_match("add works"));
        assert!(filter.is_match("add > works"), "vitest 5's separator");
        for other in ["add works with negatives", "sub add works", "add  works"] {
            assert!(!filter.is_match(other), "{other} must not run");
        }
        // A test outside any describe is its title alone.
        assert_eq!(
            exact_title_anchor("src/a.test.js::works").as_deref(),
            Some("^works$")
        );
        // Regex syntax in a title is still literal.
        let odd =
            regex::Regex::new(&exact_title_anchor("a.test.js::x.y::[1] $ok").unwrap()).unwrap();
        assert!(odd.is_match("x.y [1] $ok") && !odd.is_match("xzy [1] $ok"));
    }

    /// #1506: jest reads a bare file argument as a path regex, so
    /// `src/math.test.js` also ran `legacy/src/math.test.js`. Every run
    /// that names a file names it with `--runTestsByPath`.
    #[test]
    fn every_jest_run_names_its_file_as_an_exact_path() {
        assert_eq!(
            jest_filter_args("src/math.test.js", true),
            vec!["--runTestsByPath", "src/math.test.js", "--json"]
        );
        assert_eq!(
            jest_filter_args("src/math.test.js::add", true),
            vec![
                "--runTestsByPath",
                "src/math.test.js",
                "-t",
                "^add(?: | > )",
                "--json"
            ]
        );
        let coverage = coverage_scope_args(
            Runner::Jest,
            &CoverageScope {
                name: String::from("src/math.test.js::add::works"),
                exact: true,
                suite: false,
            },
        );
        assert_eq!(
            coverage,
            vec![
                "--runTestsByPath",
                "src/math.test.js",
                "-t",
                "^add(?: | > |::)works$"
            ]
        );
    }

    /// #1506, negative: run-at-cursor's bare title names no file, so it
    /// gets neither `--runTestsByPath` nor an anchor.
    #[test]
    fn a_bare_title_run_stays_an_unanchored_search() {
        assert_eq!(
            jest_filter_args("works", false),
            vec!["-t", "works", "--json"]
        );
        assert_eq!(
            vitest_filter_args("works", false),
            vec!["run", "-t", "works", "--reporter=tap-flat"]
        );
        assert_eq!(exact_title_anchor("src/math.test.js"), None);
    }

    /// A suite click's `-t` must not be a bare substring: describe `auth`
    /// would sweep `auth-helper handles retries` in the same file while the
    /// panel marks only the suite's own cases as Running. vitest and jest
    /// both match `-t` against the space-joined describe chain + title
    /// (vitest `getTaskFullName`, jest `getTestID`), so the anchor is
    /// `^segments… ` with the closing join space.
    #[test]
    fn a_js_suite_click_anchors_the_name_filter_to_the_describe_chain() {
        assert_eq!(
            vitest_filter_args("tests/a.test.js::auth", true),
            vec![
                "run",
                "tests/a.test.js",
                "-t",
                "^auth(?: | > )",
                "--reporter=tap-flat"
            ]
        );
        assert_eq!(
            jest_filter_args("tests/a.test.js::group (x)::inner", true),
            vec![
                "--runTestsByPath",
                "tests/a.test.js",
                "-t",
                r"^group \(x\)(?: | > |::)inner(?: | > )",
                "--json"
            ]
        );
        // A suite that is the whole file needs no `-t`: the positional file
        // argument already scopes the run exactly.
        assert_eq!(
            vitest_filter_args("tests/a.test.js", true),
            vec!["run", "tests/a.test.js", "--reporter=tap-flat"]
        );
    }

    #[test]
    fn coverage_runs_keep_each_runners_output_and_write_lcov_into_the_dir() {
        let dir = Path::new("/t/cov");
        let py = coverage_args(Runner::Pytest, dir);
        assert!(
            py.contains(&"--cov-report=lcov:/t/cov/lcov.info".to_string()),
            "{py:?}"
        );
        assert!(py.contains(&"--cov-branch".to_string()) && py.contains(&"-v".to_string()));
        let vi = coverage_args(Runner::Vitest, dir);
        assert!(vi.contains(&"--reporter=tap-flat".to_string()));
        assert!(
            vi.contains(&"--coverage.reportsDirectory=/t/cov".to_string()),
            "{vi:?}"
        );
        let je = coverage_args(Runner::Jest, dir);
        assert!(je.contains(&"--json".to_string()));
        assert!(je.contains(&"--coverageDirectory=/t/cov".to_string()));
        let ca = coverage_args(Runner::Cargo, dir);
        assert_eq!(ca[0], "llvm-cov");
        assert_eq!(ca.last().unwrap(), "/t/cov/lcov.info");
        assert_eq!(coverage_report(dir), Path::new("/t/cov/lcov.info"));
    }

    #[test]
    fn a_scoped_coverage_run_narrows_to_the_test_as_a_plain_run_would() {
        let exact = |n: &str| CoverageScope {
            name: n.into(),
            exact: true,
            suite: false,
        };
        let loose = |n: &str| CoverageScope {
            name: n.into(),
            exact: false,
            suite: false,
        };
        let suite = |n: &str| CoverageScope {
            name: n.into(),
            exact: false,
            suite: true,
        };
        // A suite selects what its plain run selects (#263): cargo anchored
        // at `suite::`, pytest by node-ID prefix, JS by file.
        assert_eq!(
            coverage_scope_args(Runner::Cargo, &suite("parse")),
            vec!["parse::"]
        );
        assert_eq!(
            coverage_scope_args(Runner::Pytest, &suite("test_x")),
            vec!["test_x"]
        );
        assert_eq!(
            coverage_scope_args(Runner::Vitest, &suite("tests/a.test.js")),
            vitest_filter_args("tests/a.test.js", true)
                .into_iter()
                .filter(|a| a != "run" && !a.starts_with("--reporter="))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            coverage_scope_args(Runner::Cargo, &exact("parse::a")),
            vec!["parse::a", "--", "--exact"]
        );
        assert_eq!(
            coverage_scope_args(Runner::Cargo, &loose("parse")),
            vec!["parse"]
        );
        assert_eq!(
            coverage_scope_args(Runner::Pytest, &exact("tests/test_x.py::test_a")),
            vec!["tests/test_x.py::test_a"]
        );
        assert_eq!(
            coverage_scope_args(Runner::Pytest, &loose("test_a")),
            vec!["-k", "test_a"]
        );
        // The coverage args already say `run`, the reporter and `--json`:
        // the scope adds only the selection, so none appears twice.
        let vi = coverage_scope_args(Runner::Vitest, &exact("tests/a.test.js::adds"));
        assert!(
            !vi.iter()
                .any(|a| a == "run" || a.starts_with("--reporter=")),
            "{vi:?}"
        );
        assert!(vi.contains(&String::from("tests/a.test.js")) && vi.contains(&String::from("-t")));
        let je = coverage_scope_args(Runner::Jest, &exact("tests/a.test.js::adds"));
        assert!(!je.contains(&String::from("--json")), "{je:?}");
        assert!(je.contains(&String::from("tests/a.test.js")));
    }

    #[test]
    fn the_install_offer_follows_the_projects_package_manager() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            coverage_install(Runner::Jest, d.path()),
            None,
            "jest has coverage built in"
        );
        assert_eq!(
            coverage_install(Runner::Pytest, d.path()).unwrap().1,
            "python3 -m pip install pytest-cov"
        );
        std::fs::write(d.path().join("uv.lock"), "").unwrap();
        assert_eq!(
            coverage_install(Runner::Pytest, d.path()).unwrap().1,
            "uv add --dev pytest-cov"
        );
        assert_eq!(
            coverage_install(Runner::Vitest, d.path()).unwrap().1,
            "npm install -D @vitest/coverage-v8"
        );
        std::fs::write(d.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(
            coverage_install(Runner::Vitest, d.path()).unwrap().1,
            "pnpm add -D @vitest/coverage-v8"
        );
        assert_eq!(
            coverage_install(Runner::Cargo, d.path()).unwrap().0,
            "cargo-llvm-cov"
        );
    }

    /// #263: a monorepo hoists `@vitest/coverage-v8` to the repository's
    /// `node_modules`, where Node resolves it from a package below.
    #[test]
    fn vitest_coverage_is_found_in_a_hoisted_node_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("packages/app");
        std::fs::create_dir_all(&pkg).unwrap();
        assert!(!coverage_tool_present(Runner::Vitest, &pkg));
        std::fs::create_dir_all(tmp.path().join("node_modules/@vitest/coverage-v8")).unwrap();
        assert!(coverage_tool_present(Runner::Vitest, &pkg));
    }

    /// A stand-in project venv under `root/venv`: `bin/pytest` marks pytest
    /// installed, and `bin/python` is a script that saves its arguments
    /// beside itself (`bin/python.args`) and then runs `body`.
    #[cfg(unix)]
    fn fake_venv(root: &Path, venv: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join(venv).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let python = format!("echo \"$*\" > \"$0.args\"\n{body}");
        for (name, text) in [("python", python.as_str()), ("pytest", "exit 0")] {
            let p = bin.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{text}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// #845: `pytest --collect-only` exits 2 on a collection error, having
    /// listed nothing. Discovery used to drop that status and finish exactly
    /// as an empty project does, so the view offered a run that would fail
    /// the same way. The status now goes back with `Finished`, a listing
    /// that works included.
    #[cfg(unix)]
    #[test]
    fn a_listing_that_errors_is_a_failed_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("pyproject.toml"), "[project]\nname = \"g\"\n").unwrap();
        assert_eq!(runner_for(root), Some(Runner::Pytest));
        let (tx, rx) = std::sync::mpsc::channel();
        let etx = EpochTx {
            tx: &tx,
            epoch: 0,
            codeql: Path::new("codeql"),
        };
        let discovered = || {
            discover(root, &etx);
            let mut cases = Vec::new();
            let mut finished = None;
            for (_, r) in rx.try_iter() {
                match r {
                    TestResponse::Case(c) => cases.push(c.name),
                    TestResponse::Finished { ok } => finished = Some(ok),
                    _ => {}
                }
            }
            (cases, finished)
        };

        fake_venv(root, ".venv", "echo tests/test_g.py::test_total\nexit 0");
        assert_eq!(
            discovered(),
            (
                vec![String::from("tests/test_g.py::test_total")],
                Some(Some(true))
            )
        );
        fake_venv(
            root,
            ".venv",
            "echo \"E   ModuleNotFoundError: No module named 'groceries'\" >&2\nexit 2",
        );
        assert_eq!(
            discovered(),
            (Vec::new(), Some(Some(false))),
            "a collection error is a failed discovery, not an empty project"
        );
        // pytest's exit 5 ("no tests were collected") is an empty project.
        fake_venv(root, ".venv", "echo 'no tests ran in 0.01s'\nexit 5");
        assert_eq!(discovered(), (Vec::new(), Some(Some(true))));
    }

    /// #845: a project venv with pytest in it runs pytest as a module of the
    /// venv's own python, which puts the project root on `sys.path` (the
    /// bare `pytest` script does not, and a flat project's tests then fail
    /// to import its modules). Discovery and runs launch it the same way.
    #[cfg(unix)]
    #[test]
    fn pytest_runs_as_a_module_of_the_project_venvs_python() {
        for venv in [".venv", "venv"] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            fake_venv(root, venv, "exit 0");
            let python = root.join(venv).join("bin").join("python");
            assert_eq!(
                pytest_launch(root),
                (python.clone(), &["-m", "pytest"][..]),
                "{venv}"
            );
            let args = |cmd: Command| {
                cmd.get_args()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            };
            let collect = pytest_cmd(root, &["--collect-only", "-q", "--color=no"]);
            assert_eq!(collect.get_program(), python.as_os_str());
            assert_eq!(
                args(collect),
                ["-m", "pytest", "--collect-only", "-q", "--color=no"]
            );
            assert_eq!(
                args(pytest_cmd(root, &["-v", "--color=no", "t.py::a"])),
                ["-m", "pytest", "-v", "--color=no", "t.py::a"]
            );
        }
        // A venv without pytest in it is not where pytest lives.
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join(".venv").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("python"), "").unwrap();
        assert_ne!(pytest_launch(tmp.path()).0, bin.join("python"));
    }

    /// #845 guard: only exit 0 and pytest's 5 ("no tests were collected")
    /// read as a listing that worked. A lister that exits 1, or dies to a
    /// signal with no exit code at all, is a failed discovery, never an
    /// empty project, even when it printed a test id before dying.
    #[cfg(unix)]
    #[test]
    fn a_listing_that_exits_one_or_dies_to_a_signal_is_a_failed_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("pyproject.toml"), "[project]\nname = \"g\"\n").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let etx = EpochTx {
            tx: &tx,
            epoch: 0,
            codeql: Path::new("codeql"),
        };
        let finished = || {
            discover(root, &etx);
            rx.try_iter().find_map(|(_, r)| match r {
                TestResponse::Finished { ok } => Some(ok),
                _ => None,
            })
        };
        fake_venv(root, ".venv", "exit 1");
        assert_eq!(finished(), Some(Some(false)), "exit 1");
        fake_venv(
            root,
            ".venv",
            "echo tests/test_g.py::test_total\nkill -9 $$",
        );
        assert_eq!(finished(), Some(Some(false)), "killed by a signal");
    }

    /// A `bin` dir to stand for `PATH`: a `pytest` script, and a `python3`
    /// that appends each command line it gets to `python3.log` beside
    /// itself and answers `-c "import pytest"` with `probe_exit`.
    #[cfg(unix)]
    fn fake_path_bin(dir: &Path, probe_exit: i32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let python =
            format!("echo \"$*\" >> \"$0.log\"\n[ \"$1\" = -c ] && exit {probe_exit}\nexit 0");
        for (name, text) in [("python3", python.as_str()), ("pytest", "exit 0")] {
            let p = bin.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{text}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        bin
    }

    /// #845: with no project venv, a `python3` that can import pytest runs
    /// it as `python3 -m pytest`, ahead of a `pytest` script on PATH that
    /// may belong to another interpreter (a uv tool's, which cannot import
    /// the project's dependencies). The probe runs once per root.
    #[cfg(unix)]
    #[test]
    fn without_a_venv_a_python3_that_has_pytest_runs_it_as_a_module() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        let bin = fake_path_bin(tmp.path(), 0);
        let path = bin.clone().into_os_string();
        let want = (bin.join("python3"), &["-m", "pytest"][..]);
        assert_eq!(pytest_launch_on(&root, Some(&path)), want);
        assert_eq!(pytest_launch_on(&root, Some(&path)), want);
        let log = std::fs::read_to_string(bin.join("python3.log")).unwrap_or_default();
        assert_eq!(log, "-c import pytest\n", "probed once");
    }

    /// #845 guard: a `python3` that cannot import pytest is not used for
    /// it; the `pytest` script on PATH runs as before.
    #[cfg(unix)]
    #[test]
    fn without_a_venv_a_python3_lacking_pytest_leaves_the_pytest_script() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        let bin = fake_path_bin(tmp.path(), 1);
        let path = bin.clone().into_os_string();
        assert_eq!(
            pytest_launch_on(&root, Some(&path)),
            (bin.join("pytest"), &[][..])
        );
    }

    /// A stand-in project venv with no `pytest` script: `bin/python` logs
    /// each command line to `bin/python.log` and answers `-c "import
    /// pytest"` with `probe_exit`.
    #[cfg(unix)]
    fn scriptless_venv(root: &Path, probe_exit: i32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join(".venv").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let python = bin.join("python");
        std::fs::write(
            &python,
            format!(
                "#!/bin/sh\necho \"$*\" >> \"$0.log\"\n[ \"$1\" = -c ] && exit {probe_exit}\nexit 0\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        python
    }

    /// #845 review: `python -m pytest` needs the module, not the console
    /// script, so a venv whose python imports pytest is where pytest lives
    /// even with no `bin/pytest` beside it.
    #[cfg(unix)]
    #[test]
    fn a_venv_whose_python_imports_pytest_runs_it_without_the_script() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        let python = scriptless_venv(&root, 0);
        let bin = fake_path_bin(tmp.path(), 0);
        let path = bin.clone().into_os_string();
        assert_eq!(
            pytest_launch_on(&root, Some(&path)),
            (python, &["-m", "pytest"][..])
        );
        assert!(!bin.join("python3.log").exists(), "python3 was not probed");
    }

    /// #845 review guard: the probe is the fallback, not the rule. A venv
    /// with the script is used without asking its python, and a venv whose
    /// python cannot import pytest (and has no script) is passed over.
    #[cfg(unix)]
    #[test]
    fn a_venv_is_probed_only_without_the_script_and_skipped_when_it_lacks_pytest() {
        let tmp = tempfile::tempdir().unwrap();
        let with_script = tmp.path().join("a");
        let python = scriptless_venv(&with_script, 0);
        let script = python.with_file_name("pytest");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        assert_eq!(pytest_launch_on(&with_script, None).0, python);
        assert!(
            !python.with_file_name("python.log").exists(),
            "a venv with the script is not probed"
        );

        let lacking = tmp.path().join("b");
        let python = scriptless_venv(&lacking, 1);
        let bin = fake_path_bin(tmp.path(), 0);
        let path = bin.clone().into_os_string();
        assert_eq!(
            pytest_launch_on(&lacking, Some(&path)),
            (bin.join("python3"), &["-m", "pytest"][..]),
            "a venv without pytest is not where it lives"
        );
        assert_eq!(
            std::fs::read_to_string(python.with_file_name("python.log")).unwrap(),
            "-c import pytest\n",
            "its python was asked once"
        );
    }

    /// #845 guard: a project venv with pytest still wins over a `python3`
    /// that has pytest too, and that `python3` is not even asked.
    #[cfg(unix)]
    #[test]
    fn a_venv_with_pytest_wins_without_probing_python3() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        fake_venv(&root, ".venv", "exit 0");
        let bin = fake_path_bin(tmp.path(), 0);
        let path = bin.clone().into_os_string();
        assert_eq!(
            pytest_launch_on(&root, Some(&path)),
            (root.join(".venv/bin/python"), &["-m", "pytest"][..])
        );
        assert!(!bin.join("python3.log").exists(), "python3 was not probed");
    }

    /// Discover `root` on this thread: the names listed, and what
    /// `Finished` said.
    fn discovery_of(root: &Path) -> (Vec<String>, Option<Option<bool>>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let etx = EpochTx {
            tx: &tx,
            epoch: 0,
            codeql: Path::new("codeql"),
        };
        discover(root, &etx);
        let mut cases = Vec::new();
        let mut finished = None;
        for (_, r) in rx.try_iter() {
            match r {
                TestResponse::Case(c) => cases.push(c.name),
                TestResponse::Finished { ok } => finished = Some(ok),
                _ => {}
            }
        }
        (cases, finished)
    }

    /// A JS project whose `runner` (`vitest` or `jest`) is the stand-in
    /// script `body` in its `node_modules/.bin`.
    #[cfg(unix)]
    fn fake_js_runner(root: &Path, runner: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            root.join("package.json"),
            format!("{{\"devDependencies\":{{\"{runner}\":\"*\"}}}}"),
        )
        .unwrap();
        let p = bin.join(runner);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// #845: a JS project with no test files lists as an empty project, not
    /// a failed discovery. vitest 2 and later exit 0 from `vitest list`
    /// then, but vitest 1 has no `list` command: it reads the word as a
    /// file filter, says "No test files found" and exits 1 unless told
    /// `--passWithNoTests`. The stand-in behaves as vitest 1.6 does. jest's
    /// `--listTests` exits 0 with nothing to list (29 and 30 alike).
    #[cfg(unix)]
    #[test]
    fn a_js_project_with_no_test_files_lists_as_empty_not_failed() {
        let tmp = tempfile::tempdir().unwrap();
        fake_js_runner(
            tmp.path(),
            "vitest",
            "case \" $* \" in *\" --passWithNoTests \"*) exit 0 ;; esac\n\
             echo 'No test files found, exiting with code 1' >&2\nexit 1",
        );
        assert_eq!(runner_for(tmp.path()), Some(Runner::Vitest));
        assert_eq!(discovery_of(tmp.path()), (Vec::new(), Some(Some(true))));

        let tmp = tempfile::tempdir().unwrap();
        fake_js_runner(tmp.path(), "jest", "exit 0");
        assert_eq!(runner_for(tmp.path()), Some(Runner::Jest));
        assert_eq!(discovery_of(tmp.path()), (Vec::new(), Some(Some(true))));
    }

    /// #845 guard: `--passWithNoTests` only forgives an empty project. A
    /// vitest listing that errors (a test file that fails to transform)
    /// still exits 1 with the flag, and stays a failed discovery.
    #[cfg(unix)]
    #[test]
    fn a_vitest_listing_that_errors_is_still_a_failed_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        fake_js_runner(
            tmp.path(),
            "vitest",
            "echo 'Error: Transform failed with 1 error' >&2\nexit 1",
        );
        assert_eq!(discovery_of(tmp.path()), (Vec::new(), Some(Some(false))));
    }

    /// A title holding `::` (`connects to ::1`) is not split into a chain
    /// that matches nothing: the anchored name still selects it, under
    /// jest's and vitest 5's separators alike.
    #[test]
    fn a_js_title_with_a_double_colon_still_runs_exactly() {
        let id = "src/net.test.js::server::connects to ::1";
        let anchor = vitest_one_args(id)[3].clone();
        let filter = regex::Regex::new(&anchor).unwrap();
        for full in ["server connects to ::1", "server > connects to ::1"] {
            assert!(filter.is_match(full), "{full} must run: {anchor}");
        }
        for other in ["server connects to ::12", "server connects to"] {
            assert!(!filter.is_match(other), "{other} must not run");
        }
    }
}
