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

/// The node IDs in a `vitest list --json` document (#1504): an array of
/// `{"name", "file"}` where `file` is absolute and `name` is the describe
/// chain and title joined by ` > `. Each ID is rooted at the file relative to
/// `root`, so two projects' `x.test.ts` stay apart, where the text listing's
/// `[a] x.test.ts` named neither file. Chrome before the array is skipped;
/// anything else parses to nothing.
pub fn parse_vitest_list_json(root: &std::path::Path, text: &str) -> Vec<String> {
    let start = if text.starts_with('[') {
        0
    } else {
        match text.find("\n[") {
            Some(i) => i + 1,
            None => return Vec::new(),
        }
    };
    let Some(Ok(serde_json::Value::Array(tests))) =
        serde_json::Deserializer::from_str(&text[start..])
            .into_iter::<serde_json::Value>()
            .next()
    else {
        return Vec::new();
    };
    tests
        .iter()
        .filter_map(|t| {
            let file = t.get("file")?.as_str()?;
            let name = t.get("name")?.as_str()?;
            let mut segs = vec![relative_to(root, file)];
            segs.extend(name.split(" > ").map(str::to_string));
            Some(segs.join("::"))
        })
        .collect()
}

/// `file` relative to `root` when it is under it, else as given.
fn relative_to(root: &std::path::Path, file: &str) -> String {
    std::path::Path::new(file)
        .strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| file.to_string())
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
/// that document parses to nothing. vitest's `json` reporter writes the same
/// jest-compatible document, which is how a vitest projects run is read
/// (#1504).
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
        let rel = relative_to(root, file);
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

    /// #1504: `vitest list --json` for two projects that each hold an
    /// `x.test.ts` with the same tests, as vitest 3 prints it (indented,
    /// after any chrome).
    fn two_project_listing(root: &str) -> String {
        format!(
            "RUN  v3.2.4 {root}\n[\n  {{\n    \"name\": \"parses input\",\n    \"file\": \"{root}/packages/a/x.test.ts\",\n    \"projectName\": \"a\"\n  }},\n  \
             {{\"name\": \"rejects bad input\", \"file\": \"{root}/packages/a/x.test.ts\", \"projectName\": \"a\"}},\n  \
             {{\"name\": \"parses input\", \"file\": \"{root}/packages/b/x.test.ts\", \"projectName\": \"b\"}},\n  \
             {{\"name\": \"auth > rejects bad input\", \"file\": \"{root}/packages/b/x.test.ts\", \"projectName\": \"b\"}}\n]\n"
        )
    }

    #[test]
    fn a_vitest_projects_listing_keeps_each_packages_tests_apart() {
        let root = std::path::Path::new("/w/vp");
        assert_eq!(
            parse_vitest_list_json(root, &two_project_listing("/w/vp")),
            vec![
                "packages/a/x.test.ts::parses input",
                "packages/a/x.test.ts::rejects bad input",
                "packages/b/x.test.ts::parses input",
                "packages/b/x.test.ts::auth::rejects bad input",
            ]
        );
    }

    #[test]
    fn a_vitest_listing_that_is_not_json_lists_nothing() {
        let root = std::path::Path::new("/w/vp");
        assert!(parse_vitest_list_json(root, "").is_empty());
        assert!(parse_vitest_list_json(root, "x.test.ts > parses input\n").is_empty());
        assert!(parse_vitest_list_json(root, "[\n{\"oops\"").is_empty());
        // An entry missing its file or name is dropped, not invented.
        assert_eq!(
            parse_vitest_list_json(
                root,
                r#"[{"name":"a"},{"file":"/w/vp/x.test.ts"},{"name":"b","file":"/w/vp/x.test.ts"}]"#
            ),
            vec!["x.test.ts::b"]
        );
    }

    #[test]
    fn a_vitest_json_report_names_cases_by_their_workspace_file() {
        // #1504: vitest's json reporter, for the two projects above after a
        // run. tap-flat named all four `x.test.ts > ...`, so they collapsed.
        let report = r#"{"numTotalTests":4,"testResults":[
            {"name":"/w/vp/packages/a/x.test.ts","status":"passed","assertionResults":[
                {"ancestorTitles":[],"fullName":"parses input","status":"passed","title":"parses input","failureMessages":[]},
                {"ancestorTitles":[],"fullName":"rejects bad input","status":"passed","title":"rejects bad input","failureMessages":[]}]},
            {"name":"/w/vp/packages/b/x.test.ts","status":"failed","assertionResults":[
                {"ancestorTitles":[],"fullName":"parses input","status":"passed","title":"parses input","failureMessages":[]},
                {"ancestorTitles":["auth"],"fullName":"auth rejects bad input","status":"failed","title":"rejects bad input","failureMessages":["expected 'b' to be 'a'"]}]}]}"#;
        let cases: Vec<(String, TestStatus)> =
            parse_jest_json(std::path::Path::new("/w/vp"), report)
                .into_iter()
                .map(|c| (c.name, c.status))
                .collect();
        assert_eq!(
            cases,
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
                    "packages/b/x.test.ts::auth::rejects bad input".into(),
                    TestStatus::Failed
                ),
            ]
        );
        // The run's IDs are the listing's, so results land on discovered rows.
        let listed =
            parse_vitest_list_json(std::path::Path::new("/w/vp"), &two_project_listing("/w/vp"));
        assert_eq!(
            cases.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
            listed
        );
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
