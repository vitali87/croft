//! Discovering tests without running them: each runner's lister, streamed.

use super::*;

/// List tests without running them (`cargo test -- --list`, or pytest's
/// `--collect-only -q`), streaming each as a `NotRun` case. The cargo path
/// still compiles the test binary, hence the Discovering state. The lister's
/// exit status goes back with `Finished`, so a listing that errors (pytest's
/// exit 2 on a collection error, a build failure) reads as a failed
/// discovery rather than a project with no tests (#845).
pub(super) fn discover(root: &Path, tx: &EpochTx, quiet: bool) {
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
pub(super) fn discover_with(runner: Runner, root: &Path, tx: &EpochTx) -> bool {
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
