//! The test runner's own tests, moved out of worker.rs (#2062).

use super::*;

/// A user extensions folder with nothing added: the bundled runners only.
const NO_EXTENSIONS: &str = "";

/// A user extensions folder with CodeQL added, whose runner comes with
/// it (#1842).
fn codeql_added() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("codeql")).unwrap();
    std::fs::write(
        dir.path().join("codeql/extension.toml"),
        crate::mcp::registry_index::CODEQL_MANIFEST,
    )
    .unwrap();
    dir
}

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
        extensions: Path::new(NO_EXTENSIONS),
        control: &RunControl::default(),
        request: 1,
        owners: None,
        tag: None,
    };
    run_all(tmp.path(), &etx);
    run_one(tmp.path(), &etx, "a::b");
    run_filter(tmp.path(), &etx, "a", false);
    run_filter(tmp.path(), &etx, "a", true);
    discover(tmp.path(), &etx, false);
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

/// Running and not a zombie: a killed runner's background child is
/// reparented and may wait to be reaped.
#[cfg(unix)]
fn running(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the pid exists.
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
        !stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
    })
}

#[cfg(unix)]
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// #1534: a run whose test hangs (here a `codeql` that never returns,
/// with a child of its own) is cancelled: the whole process group dies,
/// the panel goes idle with the run marked cancelled, and the next run
/// goes ahead and reports its own verdict.
#[cfg(unix)]
#[test]
fn cancelling_a_hung_run_kills_its_processes_and_frees_the_worker() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("test/Find")).unwrap();
    std::fs::write(
        root.join("test/qlpack.yml"),
        "name: acme/tests\nextractor: javascript\ntests: .\n",
    )
    .unwrap();
    std::fs::write(root.join("test/Find/Find.qlref"), "Find.ql\n").unwrap();
    let bin = tempfile::tempdir().unwrap();
    let hang = bin.path().join("hang");
    std::fs::write(
        &hang,
        format!(
            "#!/bin/sh\necho $$ > '{d}/runner.pid'\nsleep 1000 &\necho $! > '{d}/child.pid'\nwait\n",
            d = bin.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hang, std::fs::Permissions::from_mode(0o755)).unwrap();
    let pid = |name: &str| -> Option<i32> {
        std::fs::read_to_string(bin.path().join(name))
            .ok()?
            .trim()
            .parse()
            .ok()
    };

    let mut worker = TestWorker::spawn(root.to_path_buf());
    let mut panel = TestingPanel::new();
    let added = codeql_added();
    worker.set_extensions_dir(added.path().to_path_buf());
    worker.set_codeql_program(hang);
    worker.run_all();
    wait_until("the hung run starts", || {
        worker.drain(&mut panel);
        panel.is_busy() && pid("child.pid").is_some()
    });
    let (runner, child) = (pid("runner.pid").unwrap(), pid("child.pid").unwrap());
    worker.cancel();
    wait_until("the run ends", || {
        worker.drain(&mut panel);
        !panel.is_busy()
    });
    assert!(panel.take_cancelled(), "the end reads as cancelled");
    wait_until("the runner dies", || !running(runner));
    wait_until("the runner's child dies", || !running(child));

    // The next run is not cancelled: it reports its own verdict.
    let codeql = fake_codeql(
        bin.path(),
        root,
        "[1/1 comp 1.1s eval 20ms] PASSED @ROOT@/test/Find/Find.qlref\n",
    );
    worker.set_codeql_program(codeql);
    worker.run_all();
    panel.take_finished();
    let mut verdict = None;
    wait_until("the second run ends", || {
        worker.drain(&mut panel);
        verdict = panel.take_finished();
        verdict.is_some()
    });
    assert!(!panel.take_cancelled());
    assert_eq!(verdict, Some(Some(true)));
}

/// #2592: the test runner's thread crashing in the middle of a run (here
/// while its `codeql` hangs, with a child of its own) ends the run as
/// stopped instead of leaving it running for good, kills what it was
/// running, and a fresh thread with the same root and settings runs the
/// next run, which a cancel sent before the crash does not cover.
#[cfg(unix)]
#[test]
fn a_run_whose_runner_crashed_ends_and_the_next_one_runs() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("test/Find")).unwrap();
    std::fs::write(
        root.join("test/qlpack.yml"),
        "name: acme/tests\nextractor: javascript\ntests: .\n",
    )
    .unwrap();
    std::fs::write(root.join("test/Find/Find.qlref"), "Find.ql\n").unwrap();
    let bin = tempfile::tempdir().unwrap();
    let hang = bin.path().join("hang");
    std::fs::write(
        &hang,
        format!(
            "#!/bin/sh\necho $$ > '{d}/runner.pid'\nsleep 1000 &\necho $! > '{d}/child.pid'\nwait\n",
            d = bin.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hang, std::fs::Permissions::from_mode(0o755)).unwrap();
    let dir = bin.path().to_path_buf();
    let pid = move |name: &str| -> Option<i32> {
        std::fs::read_to_string(dir.join(name))
            .ok()?
            .trim()
            .parse()
            .ok()
    };
    let pid_in_thread = pid.clone();

    // The first thread starts the hanging runner for the run, hands its
    // error output to a reader holding a sender, as `run_streaming_exit`
    // does, and crashes once it is going; every later one is the real loop.
    let crashed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut worker =
        TestWorker::spawn_with(root.to_path_buf(), move |root, rx, tx, control, owners| {
            if crashed.swap(true, Ordering::SeqCst) {
                return worker_loop(root, rx, tx, control, owners);
            }
            while let Ok(sent) = rx.recv() {
                if matches!(sent.request, TestRequest::RunAll) {
                    let _ = tx.send((sent.epoch, TestResponse::Started(Activity::Running)));
                    let mut cmd = Command::new(&hang);
                    cmd.stderr(Stdio::piped());
                    // Left running, as a real crash leaves it: the
                    // fresh thread's drain kills it.
                    #[allow(clippy::zombie_processes)]
                    let mut runner = control.spawn(sent.number, &mut cmd).unwrap().unwrap();
                    let (err, reader_tx) = (runner.stderr.take().unwrap(), tx.clone());
                    std::thread::spawn(move || {
                        for _ in BufReader::new(err).lines() {}
                        drop(reader_tx);
                    });
                    wait_until("the hung run starts", || {
                        pid_in_thread("child.pid").is_some()
                    });
                    panic!("the test runner crashed mid-run (on purpose, in a test)");
                }
            }
        });
    let mut panel = TestingPanel::new();
    let added = codeql_added();
    // A session sends many requests before a crash, and the crash cancels
    // them all: a fresh thread counting its own from one again would read
    // its first ones as cancelled too.
    for _ in 0..8 {
        worker.set_extensions_dir(added.path().to_path_buf());
    }
    let codeql = fake_codeql(
        bin.path(),
        root,
        "[1/1 comp 1.1s eval 20ms] PASSED @ROOT@/test/Find/Find.qlref\n",
    );
    worker.set_codeql_program(codeql);
    worker.run_all();
    wait_until("the hung run starts", || pid("child.pid").is_some());
    let (runner, child) = (pid("runner.pid").unwrap(), pid("child.pid").unwrap());
    // The run's start and the crash can land in one drain, so the stop is
    // what is waited for: it is latched only for a run in flight.
    let mut stopped = false;
    wait_until("the run ends as stopped", || {
        worker.drain(&mut panel);
        stopped |= panel.take_stopped();
        stopped
    });
    assert!(!panel.is_busy());
    assert!(!panel.take_cancelled());
    assert!(!worker.take_stopped(), "one crash starts it again");
    wait_until("the runner dies", || !running(runner));
    wait_until("the runner's child dies", || !running(child));

    // The fresh thread has the root, `codeql` and extensions folder.
    panel.take_finished();
    worker.run_all();
    let mut verdict = None;
    wait_until("the next run ends", || {
        worker.drain(&mut panel);
        verdict = panel.take_finished();
        verdict.is_some()
    });
    assert!(!panel.take_stopped());
    assert_eq!(verdict, Some(Some(true)));
}

/// #2592: which runner listed each test outlives a thread that crashed, so
/// a test listed by pytest in a folder Cargo also claims still goes to
/// pytest; a new root forgets it, as its tests are another project's.
#[cfg(unix)]
#[test]
fn a_restart_keeps_which_runner_listed_each_test() {
    let tmp = tempfile::tempdir().unwrap();
    let none = tempfile::tempdir().unwrap();
    let crashed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut worker = TestWorker::spawn_with(
        tmp.path().to_path_buf(),
        move |root, rx, tx, control, owners| {
            if crashed.swap(true, Ordering::SeqCst) {
                return worker_loop(root, rx, tx, control, owners);
            }
            let _ = rx.recv();
            owners.record(String::from("tests/test_x.py::test_a"), Runner::Pytest);
            panic!("the test runner crashed (on purpose, in a test)");
        },
    );
    worker.set_extensions_dir(none.path().to_path_buf());
    let mut panel = TestingPanel::new();
    let quiet_discovery_ends = |worker: &mut TestWorker, panel: &mut TestingPanel| {
        worker.discover_quietly();
        wait_until("the quiet discovery to end", || {
            worker.drain(panel);
            !worker.discover_queued()
        });
    };
    // The first thread records the owner and crashes; the fresh one has
    // its root sent again before it reaches the next discovery.
    quiet_discovery_ends(&mut worker, &mut panel);
    quiet_discovery_ends(&mut worker, &mut panel);
    assert_eq!(
        worker.owners.get("tests/test_x.py::test_a"),
        Some(Runner::Pytest),
        "the fresh thread still knows which runner listed the test"
    );
    worker.set_root(tmp.path().to_path_buf());
    quiet_discovery_ends(&mut worker, &mut panel);
    assert_eq!(worker.owners.get("tests/test_x.py::test_a"), None);
}

/// #2592: a test runner that crashes again soon after its restart stops,
/// says so once, and the next run starts it again.
#[cfg(unix)]
#[test]
fn a_test_runner_that_crashes_again_stops_until_the_next_run() {
    let tmp = tempfile::tempdir().unwrap();
    let none = tempfile::tempdir().unwrap();
    let mut worker = TestWorker::crashing(tmp.path().to_path_buf(), 2);
    worker.set_extensions_dir(none.path().to_path_buf());
    let mut panel = TestingPanel::new();
    panel.start_single("m::a");
    worker.run_one(String::from("m::a"));
    wait_until("the runner to stop", || {
        worker.drain(&mut panel);
        worker.take_stopped()
    });
    assert!(!panel.is_busy(), "the run it was on ended");
    assert!(panel.take_stopped());
    assert!(
        panel.cases_for_test().is_empty(),
        "the case the run added is gone again"
    );
    worker.drain(&mut panel);
    assert!(!worker.take_stopped(), "it says so once");

    panel.start_single("m::a");
    worker.run_one(String::from("m::a"));
    wait_until("the next run's answer", || {
        worker.drain(&mut panel);
        panel.take_refusal()
    });
    assert!(!panel.is_busy());
}

/// #1534 negative: a cancel covers the requests sent before it, never a
/// later one, and refuses to start their further children.
#[cfg(unix)]
#[test]
fn a_cancel_covers_only_the_requests_sent_before_it() {
    let control = RunControl::default();
    control.cancel(2);
    assert!(control.is_cancelled(1));
    assert!(control.is_cancelled(2));
    assert!(!control.is_cancelled(3));
    let mut cmd = Command::new("true");
    assert!(
        control.spawn(2, &mut cmd).is_none(),
        "a cancelled run starts nothing"
    );
    let mut child = control.spawn(3, &mut cmd).unwrap().unwrap();
    child.wait().unwrap();
    control.finished();
    assert!(control.running.lock().unwrap().is_none());
}

/// CI on `main` failed `a_single_vitest_project_still_streams_tap_flat`
/// with no tests run: another test forked while its stand-in vitest was
/// still open for writing, so exec said "Text file busy". A runner busy
/// for a moment, held open here, still starts.
#[cfg(unix)]
#[test]
fn a_runner_busy_for_a_moment_after_writing_still_starts() {
    let tmp = tempfile::tempdir().unwrap();
    let (bin, f) = busy_runner(tmp.path());
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(f);
    });
    let control = RunControl::default();
    let mut cmd = Command::new(&bin);
    let mut child = control
        .spawn(1, &mut cmd)
        .expect("not cancelled")
        .expect("the busy runner starts once its writer lets go");
    assert!(child.wait().unwrap().success());
    writer.join().unwrap();
}

/// A stand-in runner in `dir` and the handle that keeps it open for
/// writing, so exec says "Text file busy" until the handle is dropped.
#[cfg(unix)]
fn busy_runner(dir: &Path) -> (PathBuf, std::fs::File) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    let bin = dir.join("runner");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(&bin)
        .unwrap();
    f.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
    f.flush().unwrap();
    (bin, f)
}

/// A cancel while a busy runner is being waited out lands at once
/// rather than after the wait, and the run starts nothing.
#[cfg(unix)]
#[test]
fn a_cancel_stops_the_wait_for_a_busy_runner() {
    let tmp = tempfile::tempdir().unwrap();
    let (bin, writer) = busy_runner(tmp.path());
    let control = RunControl::default();
    let spawner = {
        let control = control.clone();
        std::thread::spawn(move || control.spawn(1, &mut Command::new(&bin)))
    };
    std::thread::sleep(std::time::Duration::from_millis(100));
    control.cancel(1);
    let spawned = spawner.join().unwrap();
    assert!(spawned.is_none(), "the cancel ends the wait: {spawned:?}");
    drop(writer);
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
    let added = codeql_added();
    assert_eq!(
        runner_for(Path::new(NO_EXTENSIONS), root),
        None,
        "not built in"
    );
    assert_eq!(runner_for(added.path(), root), Some(Runner::Codeql));
    let (tx, rx) = std::sync::mpsc::channel();
    let etx = EpochTx {
        tx: &tx,
        epoch: 0,
        codeql: &codeql,
        extensions: added.path(),
        control: &RunControl::default(),
        request: 1,
        owners: None,
        tag: None,
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

    discover(root, &etx, false);
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
        extensions: Path::new(NO_EXTENSIONS),
        control: &RunControl::default(),
        request: 1,
        owners: None,
        tag: None,
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

    discover(root, &etx, false);
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

/// #1841: the Testing view's own discover leaves finding the runner to
/// the worker, so a root no runner claims must end it without the
/// refusal a run gesture reports, and without a Started that would
/// wipe the panel.
#[test]
fn a_quiet_discovery_with_no_runner_ends_silently() {
    let tmp = tempfile::tempdir().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let etx = EpochTx {
        tx: &tx,
        epoch: 0,
        codeql: Path::new("codeql"),
        extensions: Path::new(NO_EXTENSIONS),
        control: &RunControl::default(),
        request: 1,
        owners: None,
        tag: None,
    };
    discover(tmp.path(), &etx, true);
    let msgs: Vec<TestResponse> = rx.try_iter().map(|(_, r)| r).collect();
    assert!(
        matches!(msgs.as_slice(), [TestResponse::QuietDiscovery]),
        "a quiet discovery with no runner says it was reached and nothing else"
    );

    let (mut w, tx) = TestWorker::for_test();
    let mut panel = TestingPanel::new();
    w.discover_quietly();
    assert!(w.discover_queued());
    tx.send((0, TestResponse::QuietDiscovery)).unwrap();
    assert!(
        !w.drain(&mut panel),
        "the marker changes nothing on screen, so nothing needs redrawing"
    );
    assert!(!w.discover_queued(), "the marker ends the queued discovery");
    assert!(!panel.take_refusal(), "a quiet discovery never says why");
    assert!(!panel.is_busy());
}

/// #1841: only the quiet discovery's own marker clears the queued flag:
/// a regular discovery queued ahead of it answering first must not, or
/// a reopen of the Testing view would queue another. A re-root clears
/// it too, since its drain drops the old root's answers.
#[test]
fn only_the_quiet_discovery_itself_clears_the_queued_flag() {
    let (mut w, tx) = TestWorker::for_test();
    let mut panel = TestingPanel::new();
    w.discover();
    w.discover_quietly();
    tx.send((0, TestResponse::Started(Activity::Discovering)))
        .unwrap();
    tx.send((0, TestResponse::Finished { ok: Some(true) }))
        .unwrap();
    tx.send((0, TestResponse::Refused)).unwrap();
    assert!(w.drain(&mut panel));
    assert!(
        w.discover_queued(),
        "the earlier discovery's answers are not the quiet one starting"
    );
    tx.send((0, TestResponse::QuietDiscovery)).unwrap();
    tx.send((0, TestResponse::Started(Activity::Discovering)))
        .unwrap();
    assert!(w.drain(&mut panel));
    assert!(!w.discover_queued());
    assert!(panel.is_busy(), "the panel now shows the discovery");

    let (mut w, _tx) = TestWorker::for_test();
    w.discover_quietly();
    w.set_root(PathBuf::from("/new"));
    assert!(
        !w.discover_queued(),
        "the old root's discovery can never report under the new epoch"
    );
}

/// A root that is a package with `members` beside it (#1508).
fn package_and_workspace() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\nmembers = [\"util\"]\n",
    )
    .unwrap();
    tmp
}

fn args_of(cmd: &Command) -> Vec<String> {
    cmd.get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// #1508: at the root of a workspace that is also a package, every test
/// and coverage run covers the members too: listing, Run All, one test,
/// a filter, the debug build and llvm-cov.
#[test]
fn cargo_runs_at_a_workspace_root_cover_every_member() {
    let tmp = package_and_workspace();
    let root = tmp.path();
    for (args, want) in [
        (
            &["test", "--color=never", "--", "--list"][..],
            &["test", "--workspace", "--color=never", "--", "--list"][..],
        ),
        (
            &["test", "--no-fail-fast", "--color=never"][..],
            &["test", "--workspace", "--no-fail-fast", "--color=never"][..],
        ),
        (
            &[
                "test",
                "tests::util_works",
                "--color=never",
                "--",
                "--exact",
            ][..],
            &[
                "test",
                "--workspace",
                "tests::util_works",
                "--color=never",
                "--",
                "--exact",
            ][..],
        ),
        (
            &["test", "--no-run", "--message-format=json"][..],
            &["test", "--workspace", "--no-run", "--message-format=json"][..],
        ),
        (
            &["llvm-cov", "--no-fail-fast", "--lcov"][..],
            &["llvm-cov", "--workspace", "--no-fail-fast", "--lcov"][..],
        ),
    ] {
        assert_eq!(args_of(&cargo_cmd(root, args)), want);
    }
    // A virtual manifest takes the flag too; it changes nothing there.
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"a\", \"b\"]\n",
    )
    .unwrap();
    assert_eq!(workspace_args(root, &["test"]), ["test", "--workspace"]);
}

/// #1508, negative: a single package, a probe for llvm-cov and a
/// manifest croft cannot read run as they always did.
#[test]
fn cargo_runs_without_a_workspace_table_are_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n# [workspace] in a comment\n",
    )
    .unwrap();
    assert_eq!(
        workspace_args(root, &["test", "--no-fail-fast"]),
        ["test", "--no-fail-fast"]
    );
    let ws = package_and_workspace();
    assert_eq!(
        workspace_args(ws.path(), &["llvm-cov", "--version"]),
        ["llvm-cov", "--version"]
    );
    std::fs::write(root.join("Cargo.toml"), "[workspace\nbroken").unwrap();
    assert_eq!(workspace_args(root, &["test"]), ["test"]);
    let none = tempfile::tempdir().unwrap();
    assert_eq!(workspace_args(none.path(), &["test"]), ["test"]);
}

/// #1534: a cancel whose run's end is dropped as stale by a re-root
/// does not carry over to the new root's next run.
#[test]
fn a_cancel_dropped_by_a_re_root_does_not_cancel_the_next_run() {
    let (mut w, tx) = TestWorker::for_test();
    let mut panel = TestingPanel::new();
    w.cancel();
    w.set_root(PathBuf::from("/new"));
    tx.send((0, TestResponse::Finished { ok: None })).unwrap();
    tx.send((1, TestResponse::Started(Activity::Running)))
        .unwrap();
    tx.send((1, TestResponse::Finished { ok: Some(true) }))
        .unwrap();
    w.drain(&mut panel);
    assert!(!panel.take_cancelled(), "the new run is not cancelled");
    assert_eq!(panel.take_finished(), Some(Some(true)));
}

/// #1534: a cancelled coverage run's missing report does not replace
/// the cancel with a coverage error.
#[test]
fn a_cancelled_coverage_run_reports_no_coverage_error() {
    let (mut w, tx) = TestWorker::for_test();
    let mut panel = TestingPanel::new();
    tx.send((0, TestResponse::Started(Activity::Running)))
        .unwrap();
    w.drain(&mut panel);
    w.cancel();
    tx.send((0, TestResponse::Coverage(Err(CoverageError::NoReport))))
        .unwrap();
    tx.send((0, TestResponse::Finished { ok: Some(false) }))
        .unwrap();
    w.drain(&mut panel);
    assert!(panel.take_cancelled());
    assert!(panel.take_coverage_error().is_none());
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
    let odd = regex::Regex::new(&exact_title_anchor("a.test.js::x.y::[1] $ok").unwrap()).unwrap();
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
    assert!(
        py.contains(&"--cov-branch".to_string()) && py.contains(&PYTEST_RUN_VERBOSITY.to_string())
    );
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

/// #2078: Cancel during Rerun Failed Tests stops the whole rerun, not
/// only the test running then. `run_many` queued its request without
/// counting it, so the cancel stopped one request short: the running
/// test died and the next name still started. It is counted now, like
/// every other request.
#[cfg(unix)]
#[test]
fn cancelling_a_rerun_of_failed_tests_stops_the_names_after_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("pyproject.toml"), "[project]\nname = \"g\"\n").unwrap();
    fake_venv(
        root,
        ".venv",
        "echo \"$*\" >> \"$0.log\"\ncase \"$*\" in *test_a*) sleep 1000;; esac\nfor a; do last=$a; done\necho \"$last PASSED\"\nexit 0",
    );
    let log = root.join(".venv/bin/python.log");
    let calls = || std::fs::read_to_string(&log).unwrap_or_default();

    let mut worker = TestWorker::spawn(root.to_path_buf());
    let mut panel = TestingPanel::new();
    worker.set_extensions_dir(PathBuf::from(NO_EXTENSIONS));
    let before = worker.requests_sent();
    // As `App::start_many_tests` starts a rerun.
    let names = vec![String::from("t.py::test_a"), String::from("t.py::test_b")];
    panel.start_many(&names);
    worker.run_many(names);
    assert_eq!(worker.requests_sent(), before + 1, "the rerun is counted");
    wait_until("the first name starts", || {
        worker.drain(&mut panel);
        panel.is_busy() && calls().contains("test_a")
    });
    worker.cancel();
    wait_until("the rerun ends", || {
        worker.drain(&mut panel);
        !panel.is_busy()
    });
    assert!(panel.take_cancelled(), "the end reads as cancelled");
    assert!(
        !calls().contains("test_b"),
        "the name after the cancelled one never starts: {}",
        calls()
    );
}

/// #1535: Rerun Failed Tests runs each name exactly, all of them even
/// after one fails, and the run ends once: failed if any failed, passed
/// only when every one did.
#[cfg(unix)]
#[test]
fn run_many_runs_every_name_and_fails_if_any_did() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("pyproject.toml"), "[project]\nname = \"g\"\n").unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let etx = EpochTx {
        tx: &tx,
        epoch: 0,
        codeql: Path::new("codeql"),
        extensions: Path::new(NO_EXTENSIONS),
        control: &RunControl::default(),
        request: 1,
        owners: None,
        tag: None,
    };
    let log = root.join(".venv/bin/python.log");
    let ran = |names: &[&str]| {
        let _ = std::fs::remove_file(&log);
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        run_many(root, &etx, &names);
        let finished: Vec<Option<bool>> = rx
            .try_iter()
            .filter_map(|(_, r)| match r {
                TestResponse::Finished { ok } => Some(ok),
                _ => None,
            })
            .collect();
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        (calls.lines().count(), finished)
    };

    fake_venv(
        root,
        ".venv",
        "echo \"$*\" >> \"$0.log\"\ncase \"$*\" in *test_b*) exit 1;; *test_gone*) exit 0;; esac\nfor a; do last=$a; done\necho \"$last PASSED\"\nexit 0",
    );
    assert_eq!(
        ran(&["t.py::test_a", "t.py::test_b", "t.py::test_c"]),
        (3, vec![Some(false)]),
        "test_c still runs after test_b fails, and the run ends once, failed"
    );
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("t.py::test_a") && calls.contains("t.py::test_c"));
    assert_eq!(
        ran(&["t.py::test_a", "t.py::test_c"]),
        (2, vec![Some(true)]),
        "every one passing is a passed run"
    );
    assert_eq!(
        ran(&["t.py::test_a", "t.py::test_gone"]),
        (2, vec![Some(false)]),
        "a name the runner reported no result for, though it exited 0, fails the run"
    );
}

/// A project whose pytest `addopts` is `addopts`, with a venv python
/// that answers the way pytest does for the verbosity it ends up at:
/// `addopts` comes first, `-v` / `-q` (and `-qq`) count up and down
/// from there, and `--verbosity=N` sets the level outright. Collection
/// prints node ids only at -1; a run prints `<id> PASSED` lines only
/// from 1 up, and dots below that.
#[cfg(unix)]
fn pytest_project_with_addopts(root: &Path, addopts: &str) {
    std::fs::write(
        root.join("pyproject.toml"),
        format!("[tool.pytest.ini_options]\naddopts = \"{addopts}\"\n"),
    )
    .unwrap();
    let body = format!(
        r#"shift 2
v=0
collect=0
for a in {addopts} "$@"; do
  case "$a" in
    -v) v=$((v+1)) ;;
    -vv) v=$((v+2)) ;;
    -q) v=$((v-1)) ;;
    -qq) v=$((v-2)) ;;
    --verbosity=*) v="${{a#--verbosity=}}" ;;
    --collect-only) collect=1 ;;
  esac
done
if [ "$collect" = 1 ]; then
  if [ "$v" -le -2 ]; then echo "tests/test_x.py: 2"
  elif [ "$v" -eq -1 ]; then echo "tests/test_x.py::test_a"; echo "tests/test_x.py::test_b"
  else echo "<Module tests/test_x.py>"; echo "  <Function test_a>"; echo "  <Function test_b>"
  fi
  echo "2 tests collected in 0.01s"
  exit 0
fi
if [ "$v" -ge 1 ]; then
  echo "tests/test_x.py::test_a PASSED"
  echo "tests/test_x.py::test_b FAILED"
else
  echo "tests/test_x.py .F"
fi
echo "1 failed, 1 passed in 0.01s"
exit 1"#
    );
    fake_venv(root, ".venv", &body);
}

/// What `f` sends: the cases by name and status, and the run's verdict.
fn collect_responses(
    root: &Path,
    f: impl Fn(&Path, &EpochTx),
) -> (Vec<(String, TestStatus)>, Option<Option<bool>>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let etx = EpochTx {
        tx: &tx,
        epoch: 0,
        codeql: Path::new("codeql"),
        extensions: Path::new(NO_EXTENSIONS),
        control: &RunControl::default(),
        request: 1,
        owners: None,
        tag: None,
    };
    f(root, &etx);
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
}

/// The issue's maturin layout (#1408): a crate with one Rust test, and a
/// pytest project beside it whose venv python answers as pytest does.
#[cfg(unix)]
fn maturin_project(root: &Path) {
    pytest_project_with_addopts(root, "");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fastmath\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {}\n}\n",
    )
    .unwrap();
}

/// What `f` sends through a sender that records which runner listed
/// each test, as the worker loop's does.
fn collect_with_owners(
    root: &Path,
    owners: &Owners,
    f: impl Fn(&Path, &EpochTx),
) -> (Vec<(String, TestStatus)>, Option<Option<bool>>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let etx = EpochTx {
        tx: &tx,
        epoch: 0,
        codeql: Path::new("codeql"),
        extensions: Path::new(NO_EXTENSIONS),
        control: &RunControl::default(),
        request: 1,
        owners: Some(owners),
        tag: None,
    };
    f(root, &etx);
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
}

/// #1408: a maturin crate's Testing view lists its Rust test and its
/// pytest tests together, and Run All runs both runners.
#[cfg(unix)]
#[test]
fn a_maturin_crate_lists_and_runs_its_rust_and_python_tests() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    maturin_project(root);
    let owners = Owners::default();
    let (cases, finished) = collect_with_owners(root, &owners, |r, t| discover(r, t, false));
    let names: Vec<&str> = cases.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "tests::adds",
            "tests/test_x.py::test_a",
            "tests/test_x.py::test_b"
        ],
    );
    assert_eq!(finished, Some(Some(true)));
    assert_eq!(owners.get("tests::adds").unwrap(), Runner::Cargo);
    assert_eq!(
        owners.get("tests/test_x.py::test_a").unwrap(),
        Runner::Pytest
    );

    let (cases, finished) = collect_with_owners(root, &owners, run_all);
    assert!(
        cases.contains(&(String::from("tests::adds"), TestStatus::Passed)),
        "{cases:?}"
    );
    assert!(
        cases.contains(&(String::from("tests/test_x.py::test_b"), TestStatus::Failed)),
        "{cases:?}"
    );
    assert_eq!(
        finished,
        Some(Some(false)),
        "pytest's failure fails the run"
    );
}

/// #1408: a run from a `.py` file, or of a test pytest listed, goes to
/// pytest in a maturin crate; it used to run `cargo test` and report
/// that no test ran.
#[cfg(unix)]
#[test]
fn a_python_test_in_a_maturin_crate_runs_under_pytest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    maturin_project(root);
    let py = root.join("tests/test_x.py");
    let owners = Owners::default();
    let (cases, _) = collect_with_owners(root, &owners, |r, t| {
        run_filter_in(r, t, "test_a", false, Some(&py))
    });
    assert!(
        cases.contains(&(String::from("tests/test_x.py::test_a"), TestStatus::Passed)),
        "{cases:?}"
    );
    collect_with_owners(root, &owners, |r, t| discover(r, t, false));
    let (cases, _) = collect_with_owners(root, &owners, |r, t| {
        run_one(r, t, "tests/test_x.py::test_a")
    });
    assert!(
        cases.contains(&(String::from("tests/test_x.py::test_a"), TestStatus::Passed)),
        "{cases:?}"
    );
}

/// #1408: the runner for a run is the file's language, then whichever
/// runner listed the test (or a test in the suite), then the folder's
/// first.
#[test]
fn a_run_goes_to_its_files_runner_then_the_one_that_listed_it() {
    let both = [Runner::Cargo, Runner::Pytest];
    let owners = BTreeMap::from([
        (String::from("tests/test_api.py::test_py"), Runner::Pytest),
        (String::from("tests::adds"), Runner::Cargo),
    ]);
    let py = Path::new("/w/tests/test_api.py");
    let rs = Path::new("/w/src/lib.rs");
    assert_eq!(
        choose_runner(&both, &owners, "test_py", Some(py)),
        Some(Runner::Pytest)
    );
    assert_eq!(
        choose_runner(&both, &owners, "adds", Some(rs)),
        Some(Runner::Cargo)
    );
    assert_eq!(
        choose_runner(&both, &owners, "tests/test_api.py::test_py", None),
        Some(Runner::Pytest)
    );
    assert_eq!(
        choose_runner(&both, &owners, "tests/test_api.py", None),
        Some(Runner::Pytest),
        "a suite goes to the runner of a test under it"
    );
    assert_eq!(runner_for_file(&both, Some(py)), Some(Runner::Pytest));
    let web = [Runner::Pytest, Runner::Jest];
    assert_eq!(
        runner_for_file(&web, Some(Path::new("/w/ui/a.test.tsx"))),
        Some(Runner::Jest)
    );
}

/// #1408: a suite owns only what sits under it at the `::` separator.
/// Rust's `tests` suite beside pytest's `tests/...` nodes (which sort
/// first) is Cargo's, not pytest's.
#[test]
fn a_suite_goes_to_the_runner_of_a_test_under_its_separator() {
    let both = [Runner::Pytest, Runner::Cargo];
    let owners = BTreeMap::from([
        (String::from("tests/test_x.py::test_a"), Runner::Pytest),
        (String::from("tests::adds"), Runner::Cargo),
    ]);
    assert_eq!(
        choose_runner(&both, &owners, "tests", None),
        Some(Runner::Cargo)
    );
}

/// #1408: where Vitest and Jest both claim the folder, a JS file goes to
/// the runner that listed it, not to whichever comes first; with no
/// listing yet it falls back to the extension order.
#[test]
fn a_js_file_goes_to_the_runner_that_listed_its_tests() {
    let both = [Runner::Vitest, Runner::Jest];
    let owners = BTreeMap::from([
        (String::from("ui/a.test.ts::adds"), Runner::Jest),
        (String::from("web/b.test.ts::subs"), Runner::Vitest),
    ]);
    let jest_file = Path::new("/w/ui/a.test.ts");
    let vitest_file = Path::new("/w/web/b.test.ts");
    assert_eq!(
        choose_runner(&both, &owners, "adds", Some(jest_file)),
        Some(Runner::Jest)
    );
    assert_eq!(
        choose_runner(&both, &owners, "subs", Some(vitest_file)),
        Some(Runner::Vitest)
    );
    assert_eq!(
        choose_runner(&both, &owners, "ui/a.test.ts::adds", Some(jest_file)),
        Some(Runner::Jest)
    );
    assert_eq!(
        choose_runner(&both, &BTreeMap::new(), "adds", Some(jest_file)),
        Some(Runner::Vitest)
    );
}

/// #1408, negative: a file in a language the folder has no runner for,
/// a name no runner listed, or a listing by a runner no longer there
/// falls back to the folder's first runner; no runner at all is none.
#[test]
fn a_run_nothing_claims_goes_to_the_folders_first_runner() {
    let both = [Runner::Cargo, Runner::Pytest];
    let stale = BTreeMap::from([(String::from("x"), Runner::Go)]);
    let md = Path::new("/w/README.md");
    let go = Path::new("/w/main_test.go");
    assert_eq!(
        choose_runner(&both, &stale, "y", Some(md)),
        Some(Runner::Cargo)
    );
    assert_eq!(
        choose_runner(&both, &stale, "x", Some(go)),
        Some(Runner::Cargo)
    );
    assert_eq!(runner_for_file(&both, None), Some(Runner::Cargo));
    assert_eq!(runner_for_file(&[], Some(md)), None);
    assert_eq!(choose_runner(&[], &stale, "x", None), None);
}

/// #1502: a project with `-q` in its pytest `addopts` (the pytest docs'
/// own `-ra -q` example) listed no tests: croft's `-q` stacked on it and
/// collection printed per-file counts instead of node ids. Discovery now
/// sets the verbosity outright.
#[cfg(unix)]
#[test]
fn discovery_lists_pytest_tests_under_a_quiet_addopts() {
    for addopts in ["-ra -q", "-qq", "", "-v"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        pytest_project_with_addopts(root, addopts);
        let (cases, finished) = collect_responses(root, |r, t| discover(r, t, false));
        assert_eq!(
            cases,
            vec![
                (String::from("tests/test_x.py::test_a"), TestStatus::NotRun),
                (String::from("tests/test_x.py::test_b"), TestStatus::NotRun),
            ],
            "addopts {addopts:?}"
        );
        assert_eq!(finished, Some(Some(true)), "addopts {addopts:?}");
    }
}

/// #1502: with `-q` in `addopts`, croft's `-v` cancelled out, the run
/// printed dots, no result was parsed and the view said "Run failed".
/// Runs now ask for verbosity 1 outright, whatever `addopts` holds.
#[cfg(unix)]
#[test]
fn a_pytest_run_reports_results_under_a_quiet_addopts() {
    for addopts in ["-ra -q", "-qq", "", "-v"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        pytest_project_with_addopts(root, addopts);
        let expected = vec![
            (String::from("tests/test_x.py::test_a"), TestStatus::Passed),
            (String::from("tests/test_x.py::test_b"), TestStatus::Failed),
        ];
        let (cases, _) = collect_responses(root, run_all);
        assert_eq!(cases, expected, "run all, addopts {addopts:?}");
        let (cases, _) = collect_responses(root, |r, t| run_one(r, t, "tests/test_x.py"));
        assert_eq!(cases, expected, "run one, addopts {addopts:?}");
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
    assert_eq!(
        runner_for(Path::new(NO_EXTENSIONS), root),
        Some(Runner::Pytest)
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let etx = EpochTx {
        tx: &tx,
        epoch: 0,
        codeql: Path::new("codeql"),
        extensions: Path::new(NO_EXTENSIONS),
        control: &RunControl::default(),
        request: 1,
        owners: None,
        tag: None,
    };
    let discovered = || {
        discover(root, &etx, false);
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

/// #1503: with `-n auto` in a project's addopts, pytest-xdist prints each
/// result outcome first. Run All read none of them and, the run exiting 1
/// for its failed test, the view showed "Run failed" over an empty list.
#[cfg(unix)]
#[test]
fn run_all_reads_results_under_pytest_xdist() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("pyproject.toml"), "[project]\nname = \"x\"\n").unwrap();
    fake_venv(
        root,
        ".venv",
        "cat <<'EOF'\n\
             ============================= test session starts ==============================\n\
             created: 2/2 workers\n\
             2 workers [2 items]\n\
             \n\
             scheduling tests via LoadScheduling\n\
             \n\
             [gw0] [ 50%] PASSED tests/test_x.py::test_a \n\
             [gw1] [100%] FAILED tests/test_x.py::test_b \n\
             =========================== short test summary info ============================\n\
             FAILED tests/test_x.py::test_b - assert False\n\
             ========================= 1 failed, 1 passed in 0.61s ==========================\n\
             EOF\nexit 1",
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let etx = EpochTx {
        tx: &tx,
        epoch: 0,
        codeql: Path::new("codeql"),
        extensions: Path::new(NO_EXTENSIONS),
        control: &RunControl::default(),
        request: 1,
        owners: None,
        tag: None,
    };
    run_all(root, &etx);
    let cases: Vec<(String, TestStatus)> = rx
        .try_iter()
        .filter_map(|(_, r)| match r {
            TestResponse::Case(c) => Some((c.name, c.status)),
            _ => None,
        })
        .collect();
    assert_eq!(
        cases,
        vec![
            (String::from("tests/test_x.py::test_a"), TestStatus::Passed),
            (String::from("tests/test_x.py::test_b"), TestStatus::Failed),
        ]
    );
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
        extensions: Path::new(NO_EXTENSIONS),
        control: &RunControl::default(),
        request: 1,
        owners: None,
        tag: None,
    };
    let finished = || {
        discover(root, &etx, false);
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
    let python = format!("echo \"$*\" >> \"$0.log\"\n[ \"$1\" = -c ] && exit {probe_exit}\nexit 0");
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
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let with_script = tmp.path().join("a");
    let python = scriptless_venv(&with_script, 0);
    let script = python.with_file_name("pytest");
    std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        extensions: Path::new(NO_EXTENSIONS),
        control: &RunControl::default(),
        request: 1,
        owners: None,
        tag: None,
    };
    discover(root, &etx, false);
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

/// #1505, end to end with the real `go` when it is installed: a
/// `go.work` root lists every module's tests and Run All runs them.
#[test]
fn a_go_work_workspace_lists_and_runs_every_module() {
    if Command::new(go_binary()).arg("version").output().is_err() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let write = |rel: &str, text: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    write("lib/go.mod", "module example.com/lib\n\ngo 1.21\n");
    write(
        "lib/lib_test.go",
        "package lib\n\nimport \"testing\"\n\nfunc TestAdd(t *testing.T) {}\n",
    );
    write("svc/go.mod", "module example.com/svc\n\ngo 1.21\n");
    write(
        "svc/svc_test.go",
        "package svc\n\nimport \"testing\"\n\nfunc TestSvc(t *testing.T) { t.Fatal(\"svc broken\") }\n",
    );
    write("go.work", "go 1.21\n\nuse (\n\t./lib\n\t./svc\n)\n");
    let (mut listed, finished) = discovery_of(root);
    listed.sort();
    assert_eq!(listed, ["./lib::TestAdd", "./svc::TestSvc"], "{finished:?}");

    let (tx, rx) = std::sync::mpsc::channel();
    let etx = EpochTx {
        tx: &tx,
        epoch: 0,
        codeql: Path::new("codeql"),
        extensions: Path::new(NO_EXTENSIONS),
        control: &RunControl::default(),
        request: 1,
        owners: None,
        tag: None,
    };
    run_all(root, &etx);
    let mut ran: Vec<(String, TestStatus)> = rx
        .try_iter()
        .filter_map(|(_, r)| match r {
            TestResponse::Case(c) => Some((c.name, c.status)),
            _ => None,
        })
        .collect();
    ran.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        ran,
        [
            (String::from("./lib::TestAdd"), TestStatus::Passed),
            (String::from("./svc::TestSvc"), TestStatus::Failed),
        ]
    );
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
    assert_eq!(
        runner_for(Path::new(NO_EXTENSIONS), tmp.path()),
        Some(Runner::Vitest)
    );
    assert_eq!(discovery_of(tmp.path()), (Vec::new(), Some(Some(true))));

    let tmp = tempfile::tempdir().unwrap();
    fake_js_runner(tmp.path(), "jest", "exit 0");
    assert_eq!(
        runner_for(Path::new(NO_EXTENSIONS), tmp.path()),
        Some(Runner::Jest)
    );
    assert_eq!(discovery_of(tmp.path()), (Vec::new(), Some(Some(true))));
}

/// Run everything in `root` on this thread: the cases reported, in order.
fn run_all_of(root: &Path) -> Vec<(String, TestStatus)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let etx = EpochTx {
        tx: &tx,
        epoch: 0,
        codeql: Path::new("codeql"),
        extensions: Path::new(NO_EXTENSIONS),
        control: &RunControl::default(),
        request: 1,
        owners: None,
        tag: None,
    };
    run_all(root, &etx);
    rx.try_iter()
        .filter_map(|(_, r)| match r {
            TestResponse::Case(c) => Some((c.name, c.status)),
            _ => None,
        })
        .collect()
}

/// A stand-in vitest 3 for the #1504 monorepo: packages `a` and `b`
/// each hold an `x.test.ts` with the same two tests, and `b`'s second
/// fails. tap-flat names each test by its file relative to its project,
/// so all four print as `x.test.ts > ...`; `list --json` and the json
/// reporter carry absolute files.
#[cfg(unix)]
fn fake_vitest_monorepo(root: &Path) {
    let r = root.display();
    let list = format!(
        r#"[{{"name":"parses input","file":"{r}/packages/a/x.test.ts","projectName":"a"}},{{"name":"rejects bad input","file":"{r}/packages/a/x.test.ts","projectName":"a"}},{{"name":"parses input","file":"{r}/packages/b/x.test.ts","projectName":"b"}},{{"name":"rejects bad input","file":"{r}/packages/b/x.test.ts","projectName":"b"}}]"#
    );
    let pass = |t: &str| {
        format!(r#"{{"ancestorTitles":[],"status":"passed","title":"{t}","failureMessages":[]}}"#)
    };
    let report = format!(
        r#"{{"testResults":[{{"name":"{r}/packages/a/x.test.ts","assertionResults":[{},{}]}},{{"name":"{r}/packages/b/x.test.ts","assertionResults":[{},{{"ancestorTitles":[],"status":"failed","title":"rejects bad input","failureMessages":["no"]}}]}}]}}"#,
        pass("parses input"),
        pass("rejects bad input"),
        pass("parses input"),
    );
    std::fs::write(root.join("list.json"), list).unwrap();
    std::fs::write(root.join("report.json"), report).unwrap();
    fake_js_runner(
        root,
        "vitest",
        "case \"$1\" in list)\n\
               case \" $* \" in *\" --json \"*) cat list.json ;;\n\
               *) echo '[a] x.test.ts > parses input'; echo '[b] x.test.ts > parses input' ;; esac\n\
               exit 0 ;; esac\n\
             echo 'ok 1 - x.test.ts > parses input # time=1ms'\n\
             echo 'ok 2 - x.test.ts > rejects bad input # time=1ms'\n\
             echo 'ok 3 - x.test.ts > parses input # time=1ms'\n\
             echo 'not ok 4 - x.test.ts > rejects bad input # time=1ms'\n\
             for a in \"$@\"; do case \"$a\" in --outputFile.json=*) cp report.json \"${a#*=}\" ;; esac; done\n\
             exit 1",
    );
}

/// #1504: with vitest `projects`, a run reports four tests in two
/// packages, not two in one merged `x.test.ts`, and on the same IDs the
/// discovery listed.
#[cfg(unix)]
#[test]
fn a_vitest_projects_run_keeps_each_packages_tests_apart() {
    let tmp = tempfile::tempdir().unwrap();
    fake_vitest_monorepo(tmp.path());
    std::fs::write(
        tmp.path().join("vitest.config.ts"),
        "import { defineConfig } from 'vitest/config'\n\
             export default defineConfig({ test: { projects: ['packages/*'] } })\n",
    )
    .unwrap();
    let ran = run_all_of(tmp.path());
    assert_eq!(
        ran,
        vec![
            (
                "packages/a/x.test.ts::parses input".into(),
                TestStatus::Passed
            ),
            (
                "packages/a/x.test.ts::rejects bad input".into(),
                TestStatus::Passed
            ),
            (
                "packages/b/x.test.ts::parses input".into(),
                TestStatus::Passed
            ),
            (
                "packages/b/x.test.ts::rejects bad input".into(),
                TestStatus::Failed
            ),
        ]
    );
    let (listed, finished) = discovery_of(tmp.path());
    assert_eq!(finished, Some(Some(true)));
    assert_eq!(listed, ran.into_iter().map(|c| c.0).collect::<Vec<_>>());
}

/// #1504 negative: a single-project vitest root keeps streaming
/// tap-flat as before, and is never asked for a JSON report.
#[cfg(unix)]
#[test]
fn a_single_vitest_project_still_streams_tap_flat() {
    let tmp = tempfile::tempdir().unwrap();
    fake_vitest_monorepo(tmp.path());
    std::fs::write(
        tmp.path().join("vitest.config.ts"),
        "export default { test: { include: ['projects/**/*.test.ts'] } }\n",
    )
    .unwrap();
    let ran = run_all_of(tmp.path());
    assert_eq!(ran.len(), 4, "{ran:?}");
    assert!(
        ran.iter().all(|c| c.0.starts_with("x.test.ts::")),
        "{ran:?}"
    );
    let (listed, _) = discovery_of(tmp.path());
    assert_eq!(
        listed,
        vec!["[a] x.test.ts::parses input", "[b] x.test.ts::parses input"]
    );
}

#[test]
fn vitest_projects_are_found_in_the_config_or_a_workspace_file() {
    for config in [
        "defineConfig({ test: { projects: ['packages/*'] } })",
        "export default { test: { \"projects\" : [] } }",
        "export default { test: { 'projects': [] } }",
        "defineWorkspace; export default { test: { workspace: ['a'] } }",
    ] {
        assert!(declares_vitest_projects(config), "{config}");
    }
    for config in [
        "export default { test: { include: ['projects/**'] } }",
        "const projects = ['a']",
        "export default { test: { myprojects: 1, workspaceRoot: 'x' } }",
        "",
    ] {
        assert!(!declares_vitest_projects(config), "{config}");
    }
    let tmp = tempfile::tempdir().unwrap();
    assert!(!vitest_has_projects(tmp.path()));
    std::fs::write(
        tmp.path().join("vitest.workspace.ts"),
        "export default []\n",
    )
    .unwrap();
    assert!(vitest_has_projects(tmp.path()));
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("vitest.projects.json"), "[]\n").unwrap();
    assert!(vitest_has_projects(tmp.path()));
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("vite.config.mts"), "test: { projects: [] }").unwrap();
    assert!(vitest_has_projects(tmp.path()));
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

/// #2808: a `python3` or `pytest` on PATH that cannot run is passed over
/// for the next folder's, as a shell passes it over; taken, it failed to
/// start with "permission denied".
#[cfg(unix)]
#[test]
fn a_python3_or_pytest_that_cannot_run_is_passed_over() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let (first, second) = (tmp.path().join("a"), tmp.path().join("b"));
    for name in ["python3", "pytest"] {
        for (dir, mode) in [(&first, 0o644), (&second, 0o755)] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(name), "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(mode))
                .unwrap();
        }
    }
    let path = std::env::join_paths([&first, &second]).unwrap();
    assert_eq!(python3_program(Some(&path)), second.join("python3"));
    assert_eq!(pytest_script(Some(&path)), Some(second.join("pytest")));
}
