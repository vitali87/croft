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
    /// The queued request found no enabled runner claiming the root (it was
    /// disabled between the app's entry-point check and the worker draining
    /// the queue). Distinct from `Finished` so the panel can roll back the
    /// Running marks the `start_*` call painted instead of stranding them,
    /// and the app can say why nothing ran.
    Refused,
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
                TestResponse::Refused => panel.on_refused(),
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

/// Escape a test title for jest's `-t`, which is a REGEX matched against the
/// full name; titles routinely contain `(`, `?`, `$`.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
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
/// to the title. vitest's `-t/--testNamePattern` is jest-compatible — a
/// REGEX over the full name — so the title is escaped like jest's.
fn vitest_one_args(name: &str) -> Vec<String> {
    let (file, title) = js_id_parts(name);
    let mut args = vec![String::from("run"), file.to_string()];
    if let Some(t) = title {
        args.push(String::from("-t"));
        args.push(regex_escape(t));
    }
    args.push(String::from("--reporter=tap-flat"));
    args
}

/// vitest argv for a filter run: a suite click passes a node-ID prefix
/// (`file` or `file::describe`), run-at-cursor a bare title. The file scopes
/// the run when present; a describe segment (or the bare title) narrows via
/// `-t`.
fn vitest_filter_args(pattern: &str) -> Vec<String> {
    let mut args = vec![String::from("run")];
    let (file, title) = js_id_parts(pattern);
    if is_js_file(pattern) {
        args.push(file.to_string());
        if let Some(t) = title {
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

/// jest argv for one exact test, mirroring [`vitest_one_args`]: the file
/// scopes the run and `-t` (a regex over the full name) narrows to the
/// escaped title.
fn jest_one_args(name: &str) -> Vec<String> {
    let (file, title) = js_id_parts(name);
    let mut args = vec![file.to_string()];
    if let Some(t) = title {
        args.push(String::from("-t"));
        args.push(regex_escape(t));
    }
    args.push(String::from("--json"));
    args
}

/// jest argv for a filter run, mirroring [`vitest_filter_args`]: a node-ID
/// prefix scopes by file (plus `-t` for a describe segment), a bare title
/// goes through `-t` alone.
fn jest_filter_args(pattern: &str) -> Vec<String> {
    let mut args = Vec::new();
    if is_js_file(pattern) {
        let (file, title) = js_id_parts(pattern);
        args.push(file.to_string());
        if let Some(t) = title {
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
    mut cmd: Command,
    parse: impl Fn(&str) -> Vec<TestCase>,
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
            for case in parse(&line) {
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
    // See run_all: `None` refuses instead of falling through to cargo, and
    // `Refused` (not a bare Finished) rolls back the filtered Running marks.
    let Some(runner) = runner_for(root) else {
        tx.send(TestResponse::Refused);
        return;
    };
    let ok = match runner {
        Runner::Pytest => {
            let cmd = if pattern.contains(".py") {
                pytest_cmd(root, &["-v", "--color=no", pattern])
            } else {
                pytest_cmd(root, &["-v", "--color=no", "-k", pattern])
            };
            run_streaming(tx, cmd, one(parse_pytest_line))
        }
        Runner::Vitest => {
            let cmd = js_cmd(root, "vitest", &vitest_filter_args(pattern));
            run_streaming(tx, cmd, one(parse_vitest_tap_line))
        }
        Runner::Jest => {
            let cmd = js_cmd(root, "jest", &jest_filter_args(pattern));
            run_streaming(tx, cmd, |line| parse_jest_json(root, line))
        }
        Runner::Cargo => {
            let cmd = cargo_cmd(root, &["test", pattern, "--no-fail-fast", "--color=never"]);
            run_streaming(tx, cmd, one(parse_test_line))
        }
    }
    .unwrap_or(false);
    tx.send(TestResponse::Finished { ok: Some(ok) });
}

/// List tests without running them (`cargo test -- --list`, or pytest's
/// `--collect-only -q`), streaming each as a `NotRun` case. The cargo path
/// still compiles the test binary, hence the Discovering state.
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
    match runner {
        Runner::Pytest => {
            let cmd = pytest_cmd(root, &["--collect-only", "-q", "--color=no"]);
            run_streaming(tx, cmd, |line| {
                parse_pytest_collect_line(line)
                    .map(not_run)
                    .into_iter()
                    .collect()
            });
        }
        Runner::Vitest => {
            let cmd = js_cmd(root, "vitest", &["list"]);
            run_streaming(tx, cmd, |line| {
                parse_vitest_list_line(line)
                    .map(not_run)
                    .into_iter()
                    .collect()
            });
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
            });
        }
        Runner::Cargo => {
            let cmd = cargo_cmd(root, &["test", "--color=never", "--", "--list"]);
            run_streaming(tx, cmd, |line| {
                parse_list_line(line).map(not_run).into_iter().collect()
            });
        }
    }
    tx.send(TestResponse::Finished { ok: None });
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
        let etx = EpochTx { tx: &tx, epoch: 0 };
        run_all(tmp.path(), &etx);
        run_one(tmp.path(), &etx, "a::b");
        run_filter(tmp.path(), &etx, "a");
        discover(tmp.path(), &etx);
        let msgs: Vec<TestResponse> = rx.try_iter().map(|(_, r)| r).collect();
        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, TestResponse::Started(_))),
            "a refused run must not send Started: it wipes the discovered tree"
        );
        assert_eq!(
            msgs.iter()
                .filter(|m| matches!(m, TestResponse::Refused))
                .count(),
            4,
            "each refused request must answer with Refused, not a bare Finished"
        );
        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, TestResponse::Finished { .. })),
            "a bare Finished after start_single/start_filter strands cases Running"
        );
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
                r"adds \(1 \+ 1\)",
                "--reporter=tap-flat"
            ]
        );
        let args = vitest_filter_args("parses [ tokens");
        assert_eq!(
            args,
            vec!["run", "-t", r"parses \[ tokens", "--reporter=tap-flat"]
        );
        let args = vitest_filter_args("tests/a.test.js::group (x)");
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
            vec!["tests/math.test.js", "-t", r"adds \(1 \+ 1\)", "--json"]
        );
        assert_eq!(
            jest_filter_args("parses [ tokens"),
            vec!["-t", r"parses \[ tokens", "--json"]
        );
        assert_eq!(
            jest_filter_args("tests/a.test.js::group (x)"),
            vec!["tests/a.test.js", "-t", r"group \(x\)", "--json"]
        );
    }
}
