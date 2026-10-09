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
    // `#[should_panic]` tests print `test boom - should panic ... ok`, but
    // `--list` names them `boom`: without the suffix stripped the result
    // never lands on the discovered row, and a phantom row appears instead.
    let name = name.strip_suffix(" - should panic").unwrap_or(name);
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
/// Under pytest-xdist the line is `[gwN] [ NN%] <OUTCOME> <node-id>` instead
/// (#1503); see [`parse_xdist_line`].
pub fn parse_pytest_line(line: &str) -> Option<TestCase> {
    if let Some(case) = parse_xdist_line(line) {
        return Some(case);
    }
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

/// A pytest-xdist `-v` result (#1503): pytest's terminal reporter writes a
/// worker's report as `[gwN]`, the `[ NN%]` progress unless
/// `console_output_style = classic` turned it off, the outcome, then the node
/// ID. Anything after the node ID (a skip reason) is ignored, and a node ID
/// is still required to hold `::`, so xdist's own `[gwN] …` chrome is not a
/// result.
fn parse_xdist_line(line: &str) -> Option<TestCase> {
    let (worker, rest) = line.strip_prefix("[gw")?.split_once("] ")?;
    if worker.is_empty() || !worker.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let rest = match rest.strip_prefix('[') {
        Some(progress) => progress.split_once("] ")?.1,
        None => rest,
    };
    let (mut word, mut rest) = rest.split_once(' ')?;
    // `console_output_style = times` puts the test's duration before the
    // outcome (`[gw0] 1.234ms PASSED …`).
    if word.starts_with(|c: char| c.is_ascii_digit()) {
        (word, rest) = rest.split_once(' ')?;
    }
    let status = PYTEST_OUTCOMES
        .iter()
        .find(|(w, _)| w.trim_start() == word)?
        .1;
    let name = xdist_node_id(rest.trim_start())?;
    name.contains("::").then(|| TestCase {
        name: name.to_string(),
        status,
    })
}

/// The node ID at the start of `rest`: up to the first whitespace, unless a
/// parametrize ID (`test_a[hello world]`) is still open there, in which case
/// it runs to that bracket's close.
fn xdist_node_id(rest: &str) -> Option<&str> {
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let head = &rest[..end];
    let end = match head.find('[') {
        Some(open) if !head[open..].contains(']') => open + rest[open..].find(']')? + 1,
        _ => end,
    };
    Some(&rest[..end]).filter(|n| !n.is_empty())
}

/// Parse a single line of `pytest --collect-only -q` output into a discovered
/// node ID, or `None` for the blank line and the `N tests collected in Xs`
/// tally (both contain whitespace or lack the `::` a node ID always has).
pub fn parse_pytest_collect_line(line: &str) -> Option<String> {
    (line.contains("::") && !line.contains(char::is_whitespace)).then(|| line.to_string())
}

/// The extensions a JS/TS test file can carry. A vitest ID's first ` > `
/// segment is always the test file, so requiring one of these excludes chrome
/// that happens to contain the separator.
const JS_TEST_EXTS: [&str; 6] = [".js", ".ts", ".jsx", ".tsx", ".mjs", ".cjs"];

pub(crate) fn is_js_test_file(segment: &str) -> bool {
    JS_TEST_EXTS.iter().any(|e| segment.ends_with(e))
}

/// Join a vitest `file > describe... > test` ID into croft's `::`-separated
/// node ID, so [`super::model::TestCase::suite_and_leaf`] and the panel tree
/// work unchanged across cargo / pytest / JS.
fn vitest_id_to_name(id: &str) -> String {
    id.split(" > ").collect::<Vec<_>>().join("::")
}

/// Parse a single line of `vitest list` output (`file > describe... > test`,
/// unindented, one per test) into a discovered node ID, or `None` for chrome.
pub fn parse_vitest_list_line(line: &str) -> Option<String> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let (first, _) = line.split_once(" > ")?;
    is_js_test_file(first).then(|| vitest_id_to_name(line))
}

/// Parse a single line of `vitest run --reporter=tap-flat` output into a
/// [`TestCase`], or `None` for TAP chrome (`TAP version 13`, the `1..N` plan)
/// and the indented YAML diagnostic block under a failure. tap-flat prints
/// `ok N - <id> # time=…` / `not ok N - <id> # time=…` / `ok N - <id> # SKIP`
/// with the full `file > describe... > test` ID on every line, so the parse
/// is stateless like cargo's and pytest's.
pub fn parse_vitest_tap_line(line: &str) -> Option<TestCase> {
    let (ok, rest) = if let Some(r) = line.strip_prefix("ok ") {
        (true, r)
    } else {
        (false, line.strip_prefix("not ok ")?)
    };
    let (_num, rest) = rest.split_once(" - ")?;
    let (id, directive) = match rest.rsplit_once(" # ") {
        Some((id, d)) => (id, Some(d)),
        None => (rest, None),
    };
    let (first, _) = id.split_once(" > ")?;
    if !is_js_test_file(first) {
        return None;
    }
    let status = if directive.is_some_and(|d| d.trim_start().starts_with("SKIP")) {
        TestStatus::Skipped
    } else if ok {
        TestStatus::Passed
    } else {
        TestStatus::Failed
    };
    Some(TestCase {
        name: vitest_id_to_name(id),
        status,
    })
}

/// Parse a `jest --json` stdout blob into one [`TestCase`] per assertion.
/// jest prints its human output to stderr and exactly one JSON document to
/// stdout at the end of the run: `testResults[].name` is the absolute test
/// file (relativised against `root`), each of its `assertionResults[]`
/// carries `ancestorTitles` (the describe chain), `title`, and a `status`
/// (`pending`/`todo`/`disabled` are the skip family). Anything that is not
/// that document parses to nothing.
pub fn parse_jest_json(root: &std::path::Path, line: &str) -> Vec<TestCase> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return Vec::new();
    };
    let Some(files) = v.get("testResults").and_then(|t| t.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for f in files {
        let Some(file) = f.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let rel = std::path::Path::new(file)
            .strip_prefix(root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| file.to_string());
        let Some(asserts) = f.get("assertionResults").and_then(|a| a.as_array()) else {
            continue;
        };
        for a in asserts {
            let Some(title) = a.get("title").and_then(|t| t.as_str()) else {
                continue;
            };
            let status = match a.get("status").and_then(|s| s.as_str()) {
                Some("passed") => TestStatus::Passed,
                Some("failed") => TestStatus::Failed,
                Some("pending" | "skipped" | "todo" | "disabled") => TestStatus::Skipped,
                _ => continue,
            };
            let mut segs: Vec<&str> = vec![rel.as_str()];
            if let Some(anc) = a.get("ancestorTitles").and_then(|x| x.as_array()) {
                segs.extend(anc.iter().filter_map(|t| t.as_str()));
            }
            segs.push(title);
            out.push(TestCase {
                name: segs.join("::"),
                status,
            });
        }
    }
    out
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
        // `#[should_panic]` results carry a suffix `--list` does not.
        let r = parse_test_line("test a::boom - should panic ... ok").unwrap();
        assert_eq!(r.name, "a::boom");
        assert_eq!(r.status, TestStatus::Passed);
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

    /// #1503: under pytest-xdist (`-n auto` in addopts) `pytest -v` prints
    /// the worker and the outcome BEFORE the node ID, so no result was read
    /// and Run All reported a failed run. Lines as pytest 9 + xdist 3 print
    /// them, progress on and off.
    #[test]
    fn pytest_xdist_verbose_lines_parse_outcome_first() {
        let cases = [
            (
                "[gw0] [ 50%] PASSED tests/test_x.py::test_a ",
                "tests/test_x.py::test_a",
                TestStatus::Passed,
            ),
            (
                "[gw1] [100%] FAILED tests/test_x.py::test_b ",
                "tests/test_x.py::test_b",
                TestStatus::Failed,
            ),
            (
                "[gw3] [ 25%] SKIPPED tests/test_x.py::test_c ",
                "tests/test_x.py::test_c",
                TestStatus::Skipped,
            ),
            (
                "[gw0] [ 75%] ERROR tests/test_x.py::test_d ",
                "tests/test_x.py::test_d",
                TestStatus::Failed,
            ),
            (
                "[gw12] [ 10%] XFAIL tests/test_x.py::TestGroup::test_e ",
                "tests/test_x.py::TestGroup::test_e",
                TestStatus::Skipped,
            ),
            (
                "[gw2] [ 90%] XPASS tests/test_x.py::test_f[1-2] ",
                "tests/test_x.py::test_f[1-2]",
                TestStatus::Passed,
            ),
            // `console_output_style = classic` drops the progress.
            (
                "[gw0] PASSED tests/test_x.py::test_a",
                "tests/test_x.py::test_a",
                TestStatus::Passed,
            ),
            // `console_output_style = times` prints the duration first.
            (
                "[gw0] 1.234ms PASSED tests/test_x.py::test_a ",
                "tests/test_x.py::test_a",
                TestStatus::Passed,
            ),
            // A parametrize ID keeps its whitespace.
            (
                "[gw1] [ 50%] FAILED tests/test_x.py::test_g[hello world] ",
                "tests/test_x.py::test_g[hello world]",
                TestStatus::Failed,
            ),
        ];
        for (line, name, status) in cases {
            let c = parse_pytest_line(line).unwrap_or_else(|| panic!("{line:?}"));
            assert_eq!((c.name.as_str(), c.status), (name, status), "{line:?}");
        }
    }

    /// #1503 negative: xdist's own chrome names workers but is no result.
    #[test]
    fn pytest_xdist_chrome_is_not_a_result() {
        for line in [
            "created: 2/2 workers",
            "2 workers [2 items]",
            "scheduling tests via LoadScheduling",
            "[gw0] linux Python 3.11.2 cwd: /tmp/ptq",
            "[gw0] Python 3.11.2 (main, Mar 13 2023, 12:18:29) [GCC 12.2.0]",
            "[gw1] node down: Not properly terminated",
            "[gw0] [ 50%] PASSED",
            "[gw0] [ 50%] PASSED see tests/test_x.py",
            "[gwX] [ 50%] PASSED tests/test_x.py::test_a",
            "replacing crashed worker gw0",
            "FAILED tests/test_x.py::test_b - assert False",
        ] {
            assert!(parse_pytest_line(line).is_none(), "{line:?}");
        }
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
    fn vitest_list_lines_normalise_to_double_colon_ids() {
        // Captured from a real `vitest list` run (vitest 3.2.7): one
        // `file > describe... > test` line per test, unindented.
        assert_eq!(
            parse_vitest_list_line("tests/math.test.js > math > adds").as_deref(),
            Some("tests/math.test.js::math::adds")
        );
        assert_eq!(
            parse_vitest_list_line("tests/math.test.js > top level works").as_deref(),
            Some("tests/math.test.js::top level works")
        );
        assert_eq!(
            parse_vitest_list_line("src/util.spec.ts > helpers > clamps").as_deref(),
            Some("src/util.spec.ts::helpers::clamps")
        );
        assert!(parse_vitest_list_line("").is_none());
        // Chrome lacks the leading test-file segment.
        assert!(parse_vitest_list_line("RUN v3.2.7 /path/to/proj").is_none());
        assert!(parse_vitest_list_line("filter:  ").is_none());
    }

    #[test]
    fn vitest_tap_flat_lines_parse_to_cases_and_skip_yaml_blocks() {
        // Captured from a real `vitest run --reporter=tap-flat` (vitest 3.2.7).
        let c =
            parse_vitest_tap_line("ok 1 - tests/math.test.js > math > adds # time=0.47ms").unwrap();
        assert_eq!(c.name, "tests/math.test.js::math::adds");
        assert_eq!(c.status, TestStatus::Passed);
        assert_eq!(
            parse_vitest_tap_line(
                "not ok 2 - tests/math.test.js > math > subtracts wrong # time=2.87ms"
            )
            .unwrap()
            .status,
            TestStatus::Failed
        );
        assert_eq!(
            parse_vitest_tap_line("ok 3 - tests/math.test.js > math > skipped case # SKIP")
                .unwrap()
                .status,
            TestStatus::Skipped
        );
        // TAP chrome and the failure's indented YAML diagnostic block.
        assert!(parse_vitest_tap_line("TAP version 13").is_none());
        assert!(parse_vitest_tap_line("1..4").is_none());
        assert!(parse_vitest_tap_line("    ---").is_none());
        assert!(parse_vitest_tap_line("    error:").is_none());
        assert!(
            parse_vitest_tap_line("        message: \"expected 2 to be 1\"").is_none(),
            "YAML lines are indented; results never are"
        );
        assert!(parse_vitest_tap_line("").is_none());
    }

    #[test]
    fn jest_json_blob_parses_every_assertion_with_relative_ids() {
        use std::path::Path;
        // Shape captured from a real `jest --json` run (jest 30): one blob on
        // stdout with testResults[].name (absolute file) and
        // assertionResults[] {title, ancestorTitles, status}.
        let blob = r#"{"numTotalTests":4,"success":false,"testResults":[{"name":"/proj/tests/math.test.js","assertionResults":[{"ancestorTitles":["math"],"fullName":"math adds","status":"passed","title":"adds"},{"ancestorTitles":["math"],"fullName":"math subtracts wrong","status":"failed","title":"subtracts wrong"},{"ancestorTitles":["math"],"fullName":"math skipped case","status":"pending","title":"skipped case"},{"ancestorTitles":[],"fullName":"top level works","status":"passed","title":"top level works"}]}]}"#;
        let cases = parse_jest_json(Path::new("/proj"), blob);
        assert_eq!(cases.len(), 4);
        assert_eq!(cases[0].name, "tests/math.test.js::math::adds");
        assert_eq!(cases[0].status, TestStatus::Passed);
        assert_eq!(cases[1].status, TestStatus::Failed);
        assert_eq!(
            cases[2].status,
            TestStatus::Skipped,
            "jest reports skipped tests as `pending`"
        );
        assert_eq!(cases[3].name, "tests/math.test.js::top level works");
        // Stray non-JSON stdout lines parse to nothing.
        assert!(parse_jest_json(Path::new("/proj"), "").is_empty());
        assert!(
            parse_jest_json(Path::new("/proj"), "Determining test suites to run...").is_empty()
        );
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
