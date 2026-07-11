//! Parser for `cargo test` (libtest) human output. libtest prints one line per
//! test, `test <path> ... <outcome>`, plus a `test result: ...` summary and
//! `Compiling`/`Running` chrome we ignore. We parse the per-test lines into
//! [`TestCase`]s and leave the suite grouping to the panel.

use super::model::{TestCase, TestStatus};

/// Parse a single line of libtest output into a [`TestCase`], or `None` when the
/// line is not a per-test result (compile chrome, the summary line, a blank).
pub fn parse_test_line(line: &str) -> Option<TestCase> {
    // `rsplit_once` keeps the summary line ("test result: ok. ...") out: it has
    // no " ... " infix. Test paths never contain " ... ", so the split is safe.
    let rest = line.strip_prefix("test ")?;
    let (name, outcome) = rest.rsplit_once(" ... ")?;
    let status = match outcome.trim() {
        "ok" => TestStatus::Passed,
        "FAILED" => TestStatus::Failed,
        // libtest prints "ignored" or "ignored, <reason>" for #[ignore] tests.
        s if s.starts_with("ignored") => TestStatus::Skipped,
        _ => return None, // benches ("bench: ..."), "measured", etc.
    };
    Some(TestCase {
        name: name.to_string(),
        status,
    })
}

/// Parse a single line of `cargo test -- --list` output into a discovered test
/// name, or `None` for non-test lines. libtest `--list` prints `<path>: test`
/// for each test and `<path>: benchmark` for benches (skipped), plus a trailing
/// `N tests, M benchmarks` tally (no `: test` suffix, so excluded).
pub fn parse_list_line(line: &str) -> Option<String> {
    line.strip_suffix(": test").map(str::to_string)
}

/// The `pytest -v` per-test outcomes, each searched as ` <WORD>` so the
/// short-summary lines (`FAILED <id> - ...`, outcome first, no leading space)
/// never match. ERROR covers setup/teardown failures; XFAIL is an expected
/// failure (skip-coloured, like VS Code); XPASS an unexpected pass.
const PYTEST_OUTCOMES: [(&str, TestStatus); 6] = [
    (" PASSED", TestStatus::Passed),
    (" FAILED", TestStatus::Failed),
    (" ERROR", TestStatus::Failed),
    (" SKIPPED", TestStatus::Skipped),
    (" XFAIL", TestStatus::Skipped),
    (" XPASS", TestStatus::Passed),
];

/// Parse a single line of `pytest -v` output into a [`TestCase`], or `None`
/// when the line is not a per-test result (session chrome, tracebacks, the
/// short-summary lines). A result line is `<node-id> <OUTCOME>` optionally
/// followed by a reason and the `[ NN%]` progress: the node ID must contain
/// `::` and no whitespace, which excludes prose that happens to name a test.
pub fn parse_pytest_line(line: &str) -> Option<TestCase> {
    for (word, status) in PYTEST_OUTCOMES {
        if let Some((name, rest)) = line.split_once(word)
            && name.contains("::")
            && !name.contains(char::is_whitespace)
            && (rest.is_empty() || rest.starts_with(' '))
        {
            return Some(TestCase {
                name: name.to_string(),
                status,
            });
        }
    }
    None
}

/// Parse a single line of `pytest --collect-only -q` output into a discovered
/// node ID, or `None` for the blank line and the `N tests collected in Xs`
/// tally (both contain whitespace or lack the `::` a node ID always has).
pub fn parse_pytest_collect_line(line: &str) -> Option<String> {
    (line.contains("::") && !line.contains(char::is_whitespace)).then(|| line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pass_fail_ignored_and_skips_chrome() {
        assert_eq!(
            parse_test_line("test mymod::works ... ok").unwrap().status,
            TestStatus::Passed
        );
        assert_eq!(
            parse_test_line("test mymod::broken ... FAILED")
                .unwrap()
                .status,
            TestStatus::Failed
        );
        assert_eq!(
            parse_test_line("test slow ... ignored").unwrap().status,
            TestStatus::Skipped
        );
        assert_eq!(
            parse_test_line("test slow ... ignored, needs network")
                .unwrap()
                .status,
            TestStatus::Skipped
        );
        assert_eq!(
            parse_test_line("test a::b::c ... ok").unwrap().name,
            "a::b::c"
        );
        // Chrome and the summary line are not test cases.
        assert!(parse_test_line("test result: ok. 1 passed; 0 failed; 0 ignored;").is_none());
        assert!(parse_test_line("   Compiling croft v0.1.0").is_none());
        assert!(parse_test_line("").is_none());
        assert!(parse_test_line("test benchy ... bench: 12 ns/iter").is_none());
    }

    #[test]
    fn cargo_progress_surfaces_status_verbs_and_ignores_diagnostics() {
        use crate::testing::worker::cargo_progress;
        assert_eq!(
            cargo_progress("   Compiling ratatui v0.29.0").as_deref(),
            Some("Compiling ratatui v0.29.0")
        );
        assert_eq!(
            cargo_progress("    Finished `test` profile").as_deref(),
            Some("Finished `test` profile")
        );
        // Not a status line: a diagnostic, a blank, the list output.
        assert!(cargo_progress("error[E0433]: failed to resolve").is_none());
        assert!(cargo_progress("").is_none());
        assert!(cargo_progress("mymod::works: test").is_none());
    }

    #[test]
    fn pytest_verbose_lines_parse_to_cases_and_skip_chrome() {
        // Captured from a real `pytest -v --color=no` run (pytest 9.0.2).
        let c = parse_pytest_line(
            "tests/test_sample.py::test_passes PASSED                                 [ 14%]",
        )
        .unwrap();
        assert_eq!(c.name, "tests/test_sample.py::test_passes");
        assert_eq!(c.status, TestStatus::Passed);
        assert_eq!(
            parse_pytest_line(
                "tests/test_sample.py::test_fails FAILED                                  [ 28%]"
            )
            .unwrap()
            .status,
            TestStatus::Failed
        );
        assert_eq!(
            parse_pytest_line(
                "tests/test_sample.py::test_skipped SKIPPED (not now)                     [ 42%]"
            )
            .unwrap()
            .status,
            TestStatus::Skipped
        );
        assert_eq!(
            parse_pytest_line(
                "tests/test_sample.py::test_param[1] PASSED                               [ 57%]"
            )
            .unwrap()
            .name,
            "tests/test_sample.py::test_param[1]"
        );
        assert_eq!(
            parse_pytest_line(
                "tests/test_sample.py::TestGroup::test_method PASSED                      [ 85%]"
            )
            .unwrap()
            .name,
            "tests/test_sample.py::TestGroup::test_method"
        );
        assert_eq!(
            parse_pytest_line(
                "tests/test_sample.py::test_xfail XFAIL                                   [100%]"
            )
            .unwrap()
            .status,
            TestStatus::Skipped
        );
        assert_eq!(
            parse_pytest_line("tests/test_sample.py::test_setup ERROR")
                .unwrap()
                .status,
            TestStatus::Failed
        );
        assert_eq!(
            parse_pytest_line("tests/test_sample.py::test_unexpected XPASS")
                .unwrap()
                .status,
            TestStatus::Passed
        );
        // Chrome, section rules, tracebacks and the short summary (which leads
        // with the outcome, no space before it) are not result lines.
        assert!(
            parse_pytest_line(
                "============================= test session starts =============================="
            )
            .is_none()
        );
        assert!(
            parse_pytest_line(
                "==================================== ERRORS ===================================="
            )
            .is_none()
        );
        assert!(
            parse_pytest_line("FAILED tests/test_sample.py::test_fails - assert False").is_none()
        );
        assert!(parse_pytest_line("tests/test_sample.py:7: AssertionError").is_none());
        assert!(parse_pytest_line("collecting ... collected 7 items").is_none());
        assert!(parse_pytest_line("").is_none());
    }

    #[test]
    fn pytest_collect_lines_take_node_ids_and_skip_the_tally() {
        // Captured from a real `pytest --collect-only -q` run: bare node IDs,
        // a blank line, then a "N tests collected in Xs" tally.
        assert_eq!(
            parse_pytest_collect_line("tests/test_sample.py::test_passes").as_deref(),
            Some("tests/test_sample.py::test_passes")
        );
        assert_eq!(
            parse_pytest_collect_line("tests/test_sample.py::TestGroup::test_method").as_deref(),
            Some("tests/test_sample.py::TestGroup::test_method")
        );
        assert_eq!(
            parse_pytest_collect_line("tests/test_sample.py::test_param[1]").as_deref(),
            Some("tests/test_sample.py::test_param[1]")
        );
        assert!(parse_pytest_collect_line("7 tests collected in 0.00s").is_none());
        assert!(parse_pytest_collect_line("no tests ran in 0.01s").is_none());
        assert!(parse_pytest_collect_line("").is_none());
    }

    #[test]
    fn list_parser_takes_test_lines_and_skips_benches_and_tally() {
        assert_eq!(
            parse_list_line("mymod::works: test").as_deref(),
            Some("mymod::works")
        );
        assert_eq!(parse_list_line("a::b::c: test").as_deref(), Some("a::b::c"));
        assert!(parse_list_line("benchy: benchmark").is_none());
        assert!(parse_list_line("2 tests, 1 benchmark").is_none());
        assert!(parse_list_line("").is_none());
    }
}
