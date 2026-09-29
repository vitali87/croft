//! `go test` for the Testing view (#264). Everything Go-specific the worker
//! needs: the module path from `go.mod`, the test IDs (`./pkg::TestName`,
//! a subtest keeping Go's own `TestName/sub` leaf), the `go test -json`
//! event parsers, the `-run` selectors, and turning a Go cover profile into
//! the LCOV the coverage view reads.
//!
//! A package is named by its directory relative to the module root (`.` for
//! the root package, `./internal/db` below it), which is both what the tree
//! shows and what `go test` takes as a package argument. A package outside
//! the module (no `go.mod`, or a `go.work` sibling) keeps its import path,
//! which `go test` also accepts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::model::{TestCase, TestStatus};
use super::regex_escape;

/// The `module` path `root/go.mod` declares, if any.
pub fn module_path(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("go.mod")).ok()?;
    text.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("module")?;
        // `module` must be the whole keyword, not `modulefoo`.
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let m = rest.trim().trim_matches('"');
        (!m.is_empty()).then(|| m.to_string())
    })
}

/// The tree's name for the package at import path `pkg`.
pub fn package_id(module: Option<&str>, pkg: &str) -> String {
    match module {
        Some(m) if pkg == m => String::from("."),
        Some(m) => match pkg.strip_prefix(m).and_then(|r| r.strip_prefix('/')) {
            Some(rest) => format!("./{rest}"),
            None => pkg.to_string(),
        },
        None => pkg.to_string(),
    }
}

/// The directory of a package id, when it is one of the module's own.
pub fn package_dir(root: &Path, package: &str) -> Option<PathBuf> {
    match package {
        "." => Some(root.to_path_buf()),
        p => p.strip_prefix("./").map(|rest| root.join(rest)),
    }
}

/// One `go test -json` event: only the fields croft reads.
#[derive(serde::Deserialize)]
struct Event {
    #[serde(rename = "Action")]
    action: String,
    #[serde(rename = "Package", default)]
    package: String,
    #[serde(rename = "Test", default)]
    test: Option<String>,
    #[serde(rename = "Output", default)]
    output: Option<String>,
}

fn event(line: &str) -> Option<Event> {
    serde_json::from_str(line.trim()).ok()
}

/// A finished test from one `go test -json` line: `pass`, `fail` or `skip`
/// for a named test. Package-level results and output are not cases.
pub fn parse_event(module: Option<&str>, line: &str) -> Option<TestCase> {
    let e = event(line)?;
    let status = match e.action.as_str() {
        "pass" => TestStatus::Passed,
        "fail" => TestStatus::Failed,
        "skip" => TestStatus::Skipped,
        _ => return None,
    };
    let test = e.test?;
    Some(TestCase {
        name: format!("{}::{test}", package_id(module, &e.package)),
        status,
    })
}

/// A test `go test -json -list .` names: its output lines that are a bare
/// `Test…`, `Example…` or `Fuzz…` identifier. The `ok  pkg 0.1s` trailer
/// and benchmarks (which a plain run skips) are not.
pub fn parse_list_event(module: Option<&str>, line: &str) -> Option<String> {
    let e = event(line)?;
    if e.action != "output" {
        return None;
    }
    let name = e.output?.trim_end().to_string();
    let runnable = ["Test", "Example", "Fuzz"]
        .iter()
        .any(|p| name.starts_with(p));
    let ident = name.chars().all(|c| c.is_alphanumeric() || c == '_');
    (runnable && ident).then(|| format!("{}::{name}", package_id(module, &e.package)))
}

/// What the OUTPUT channel shows for a `go test -json` line: the text the
/// test printed, not the JSON around it. A line that is not an event (a
/// build error on stdout) shows as it is.
pub fn shown(line: &str) -> Option<String> {
    match event(line) {
        Some(e) => e.output.map(|o| o.trim_end_matches('\n').to_string()),
        None => Some(line.to_string()),
    }
}

/// `-run` for a test name: each `/`-separated level anchored and escaped,
/// the way `go test` matches subtests level by level.
fn run_pattern(test: &str) -> String {
    test.split('/')
        .map(|level| format!("^{}$", regex_escape(level)))
        .collect::<Vec<_>>()
        .join("/")
}

/// The package and `-run` arguments that select `id`: `./pkg::TestName`
/// runs that one test (and its subtests), a bare package id the whole
/// package.
fn select(id: &str) -> Vec<String> {
    match id.split_once("::") {
        Some((pkg, test)) => vec![pkg.to_string(), String::from("-run"), run_pattern(test)],
        None => vec![id.to_string()],
    }
}

/// Arguments after `go test -json` for one exact test.
pub fn one_args(id: &str) -> Vec<String> {
    select(id)
}

/// Arguments after `go test -json` for a filter run. A suite click passes
/// a package or `package::Test` id; run-at-cursor a bare function name,
/// which, like cargo's filter, may match in any package.
pub fn filter_args(pattern: &str, suite: bool) -> Vec<String> {
    if suite || pattern.contains("::") {
        select(pattern)
    } else {
        vec![
            String::from("./..."),
            String::from("-run"),
            regex_escape(pattern),
        ]
    }
}

/// The debuggee's argv for delve's `test` mode: the same selection as
/// [`one_args`], spelled as the test binary's own flag.
pub fn test_binary_args(id: &str) -> Vec<String> {
    match id.split_once("::") {
        Some((_, test)) => vec![String::from("-test.run"), run_pattern(test)],
        None => vec![String::from("-test.run"), regex_escape(id)],
    }
}

/// Turn a Go cover profile into LCOV `DA:` records, paths relative to the
/// module root. Each block's count applies to every line it spans; where
/// blocks share a line, the line keeps the highest count. Files outside
/// the module are left out: there is nothing in the workspace to paint.
pub fn cover_profile_to_lcov(profile: &str, module: Option<&str>) -> String {
    let mut files: BTreeMap<String, BTreeMap<u32, u64>> = BTreeMap::new();
    for line in profile.lines().skip_while(|l| l.starts_with("mode:")) {
        // `import/path/file.go:3.24,4.13 1 1`
        let Some((file, rest)) = line.rsplit_once(':') else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let (Some(span), Some(_stmts), Some(Ok(count))) = (
            parts.next(),
            parts.next(),
            parts.next().map(str::parse::<u64>),
        ) else {
            continue;
        };
        let Some((start, end)) = span.split_once(',') else {
            continue;
        };
        let line_of = |pos: &str| pos.split('.').next().and_then(|l| l.parse::<u32>().ok());
        let (Some(from), Some(to)) = (line_of(start), line_of(end)) else {
            continue;
        };
        let rel = match module {
            Some(m) => match file.strip_prefix(m).and_then(|r| r.strip_prefix('/')) {
                Some(r) => r.to_string(),
                None => continue,
            },
            None => continue,
        };
        let hits = files.entry(rel).or_default();
        for n in from..=to {
            let h = hits.entry(n).or_insert(0);
            *h = (*h).max(count);
        }
    }
    let mut out = String::new();
    for (file, hits) in files {
        out.push_str(&format!("SF:{file}\n"));
        for (n, h) in hits {
            out.push_str(&format!("DA:{n},{h}\n"));
        }
        out.push_str("end_of_record\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: Option<&str> = Some("example.com/m");

    #[test]
    fn go_json_events_become_cases_named_by_package_directory() {
        let pass = r#"{"Action":"pass","Package":"example.com/m/foo","Test":"TestAdd/small_one","Elapsed":0}"#;
        assert_eq!(
            parse_event(M, pass),
            Some(TestCase {
                name: String::from("./foo::TestAdd/small_one"),
                status: TestStatus::Passed,
            })
        );
        let fail = r#"{"Action":"fail","Package":"example.com/m","Test":"TestRoot"}"#;
        assert_eq!(
            parse_event(M, fail).map(|c| (c.name, c.status)),
            Some((String::from(".::TestRoot"), TestStatus::Failed))
        );
        let skip = r#"{"Action":"skip","Package":"example.com/m/foo","Test":"TestSkip"}"#;
        assert_eq!(parse_event(M, skip).unwrap().status, TestStatus::Skipped);
        // A package's own result, a run marker and output are not cases.
        for line in [
            r#"{"Action":"fail","Package":"example.com/m/foo","Elapsed":0.003}"#,
            r#"{"Action":"run","Package":"example.com/m/foo","Test":"TestAdd"}"#,
            r#"{"Action":"output","Package":"example.com/m/foo","Test":"TestAdd","Output":"--- PASS\n"}"#,
            "# example.com/m/foo [build failed]",
        ] {
            assert_eq!(parse_event(M, line), None, "{line}");
        }
        // Outside the module the import path stays.
        assert_eq!(
            parse_event(
                M,
                r#"{"Action":"pass","Package":"other.org/x","Test":"TestX"}"#
            )
            .unwrap()
            .name,
            "other.org/x::TestX"
        );
    }

    #[test]
    fn listing_keeps_the_runnable_names_and_drops_the_trailer() {
        let out = |s: &str| {
            format!(r#"{{"Action":"output","Package":"example.com/m/foo","Output":"{s}\n"}}"#)
        };
        assert_eq!(
            parse_list_event(M, &out("TestAdd")).as_deref(),
            Some("./foo::TestAdd")
        );
        assert_eq!(
            parse_list_event(M, &out("ExampleAdd")).as_deref(),
            Some("./foo::ExampleAdd")
        );
        assert_eq!(parse_list_event(M, &out("BenchmarkAdd")), None);
        assert_eq!(
            parse_list_event(M, &out("ok  \\texample.com/m/foo\\t0.002s")),
            None
        );
    }

    #[test]
    fn run_arguments_anchor_each_subtest_level() {
        assert_eq!(
            one_args("./foo::TestAdd/small_one"),
            ["./foo", "-run", "^TestAdd$/^small_one$"]
        );
        assert_eq!(filter_args("./foo", true), ["./foo"]);
        assert_eq!(
            filter_args(".::TestRoot", true),
            [".", "-run", "^TestRoot$"]
        );
        assert_eq!(filter_args("TestAdd", false), ["./...", "-run", "TestAdd"]);
        assert_eq!(
            test_binary_args("./foo::TestAdd"),
            ["-test.run", "^TestAdd$"]
        );
    }

    #[test]
    fn the_module_path_and_package_directories() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("go.mod"),
            "// c\nmodule \"example.com/m\"\n\ngo 1.21\n",
        )
        .unwrap();
        assert_eq!(module_path(d.path()).as_deref(), Some("example.com/m"));
        assert_eq!(package_dir(d.path(), "."), Some(d.path().to_path_buf()));
        assert_eq!(package_dir(d.path(), "./a/b"), Some(d.path().join("a/b")));
        assert_eq!(package_dir(d.path(), "other.org/x"), None);
    }

    #[test]
    fn a_cover_profile_becomes_lcov_line_hits() {
        let profile = "mode: set\n\
            example.com/m/foo/foo.go:3.24,4.13 1 1\n\
            example.com/m/foo/foo.go:4.13,6.3 1 0\n\
            example.com/m/foo/foo.go:7.2,7.14 1 1\n\
            other.org/x/x.go:1.1,2.2 1 1\n";
        assert_eq!(
            cover_profile_to_lcov(profile, M),
            "SF:foo/foo.go\nDA:3,1\nDA:4,1\nDA:5,0\nDA:6,0\nDA:7,1\nend_of_record\n"
        );
    }
}
