//! Coverage runs: the LCOV each runner writes, narrowing a run to a scope, the tool that
//! measures it and installing it, and why a run produced no report.

use super::*;

/// The report every runner is asked to write: LCOV, into `dir`.
pub(super) fn coverage_report(dir: &Path) -> PathBuf {
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
pub(super) fn coverage_scope_args(runner: Runner, scope: &CoverageScope) -> Vec<String> {
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
                crate::testing::suite_pattern(name)
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
                crate::testing::gotest::one_args(name)
            } else {
                crate::testing::gotest::filter_args(name, scope.suite)
            }
        }
        // Refused before any argv is built (see run_coverage).
        Runner::Codeql => Vec::new(),
    }
}

/// The run-everything argv with coverage on, writing LCOV into `dir`. Each
/// keeps the output format the normal run parses, so results stream into
/// the tree as usual.
pub(super) fn coverage_args(runner: Runner, dir: &Path) -> Vec<String> {
    let dir_s = dir.display().to_string();
    let report = coverage_report(dir).display().to_string();
    let v = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    match runner {
        Runner::Pytest => {
            let mut a = v(&[
                PYTEST_RUN_VERBOSITY,
                "--color=no",
                "--cov=.",
                "--cov-branch",
            ]);
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
pub(super) fn go_cover_profile(dir: &Path) -> PathBuf {
    dir.join("cover.out")
}

/// What installs a runner's coverage tool, picked from the project's own
/// package manager. `None` for jest, whose coverage is built in.
pub(super) fn coverage_install(runner: Runner, root: &Path) -> Option<(&'static str, String)> {
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
pub(super) fn coverage_tool_present(runner: Runner, root: &Path) -> bool {
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
pub(super) fn run_coverage(root: &Path, tx: &EpochTx, scope: Option<&CoverageScope>) {
    // A scoped run goes to the runner that owns its test, as a plain run
    // of it does (#1408): a pytest case in a maturin crate is pytest-cov's.
    let runner = match scope {
        Some(scope) => tx.choose(&runners_for(tx.extensions, root), &scope.name, None),
        None => runner_for(tx.extensions, root),
    };
    let Some(runner) = runner else {
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
        Runner::Vitest => {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            run_vitest(tx, root, &args)
        }
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
            let modules = crate::testing::gotest::Modules::of(root);
            crate::testing::gotest::cover_profile_to_lcov(&profile, &modules)
        })
    } else {
        std::fs::read_to_string(coverage_report(&dir))
    };
    let report = lcov
        .map(|text| crate::testing::coverage::Coverage::from_lcov(&text, root))
        .map_err(|_| CoverageError::NoReport);
    tx.send(TestResponse::Coverage(report));
    tx.send(TestResponse::Finished { ok: Some(ok) });
}
