//! `codeql test run` for the Testing view (#578). Everything CodeQL-specific
//! the worker needs: which folders are test packs, the tests under them, the
//! `codeql test run` arguments, and a parser for its text output.
//!
//! A CodeQL unit test is a `.ql` or `.qlref` file with a `<stem>.expected`
//! beside it, inside a test pack (a `qlpack.yml` / `codeql-pack.yml` that
//! declares `tests:` or `extractor:`). Its id is the test's folder relative
//! to the workspace root, `::`, and its file name (`test/Foo::Foo.qlref`),
//! so the folder is the suite in the tree; a test at the root itself has no
//! folder. The folder and the file are also exactly what `codeql test run`
//! takes to run a directory or a single test.
//!
//! The CLI prints one line per finished test (`[2/3 comp 1.2s eval 300ms]
//! FAILED(RESULT) /abs/Bar.ql`), followed for a failure by the unified diff
//! of expected against actual results or by the compiler's `ERROR:` lines.
//! [`RunStream`] turns that into outcomes, each failure carrying its message.

use std::path::{Path, PathBuf};

use super::model::{TestCase, TestStatus};

/// How deep below the workspace root [`test_packs`] looks for a pack file:
/// deep enough for `<lang>/ql/test/qlpack.yml`, shallow enough that runner
/// detection never walks a whole tree.
const PACK_DEPTH: usize = 4;

/// Files [`discover`] looks at before it stops, as the CodeQL side bar's
/// query discovery caps itself.
const DISCOVER_CAP: usize = 50_000;

/// Which of a test pack's keys a pack file's text declares at the top
/// level: (`tests:`, `extractor:`). A line scan, not YAML, like
/// [`crate::codeql_query::pack_name`]; an indented key belongs to a nested
/// map and does not count.
pub fn declares_tests(text: &str) -> (bool, bool) {
    let key = |k: &str| text.lines().any(|l| l.starts_with(k));
    (key("tests:"), key("extractor:"))
}

/// Walk `dir` (honouring ignore files, skipping noise folders and the
/// `*.testproj` databases `codeql test run` leaves behind), visiting files
/// up to `max_depth` below it.
fn walk(dir: &Path, max_depth: Option<usize>) -> impl Iterator<Item = PathBuf> {
    ignore::WalkBuilder::new(dir)
        .git_ignore(true)
        .require_git(false)
        .hidden(false)
        .max_depth(max_depth)
        .filter_entry(|e| {
            e.depth() == 0
                || !e.file_type().is_some_and(|t| t.is_dir())
                || !(crate::widgets::file_finder::is_noise_dir(e.file_name())
                    || e.file_name().to_string_lossy().ends_with(".testproj")
                    || e.file_name() == ".git")
        })
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .take(DISCOVER_CAP)
        .map(|e| e.into_path())
}

/// Whether `path` is a CodeQL test: a `.ql` or `.qlref` file with a
/// `<stem>.expected` beside it.
fn is_test(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("ql" | "qlref")
    ) && path.with_extension("expected").is_file()
}

/// The test packs at or below `root` (at most [`PACK_DEPTH`] deep), as
/// folders, sorted. `markers` names the pack files to look for. A pack that
/// declares `tests:` is one outright; one that only names an `extractor:`
/// counts when it holds at least one test, since a query pack can name its
/// language that way too.
pub fn test_packs(root: &Path, markers: &[String]) -> Vec<PathBuf> {
    let mut packs: Vec<PathBuf> = walk(root, Some(PACK_DEPTH))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| markers.iter().any(|m| m == n))
        })
        .filter_map(|file| {
            let dir = file.parent()?.to_path_buf();
            let text = std::fs::read_to_string(&file).ok()?;
            match declares_tests(&text) {
                (true, _) => Some(dir),
                (false, true) if walk(&dir, None).any(|p| is_test(&p)) => Some(dir),
                _ => None,
            }
        })
        .collect();
    packs.sort();
    packs.dedup();
    packs
}

/// The id of the test at `path` (see the module docs). A path outside
/// `root` keeps its whole folder as the suite.
pub fn test_id(root: &Path, path: &Path) -> String {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = path.parent().unwrap_or(Path::new(""));
    let rel = dir
        .strip_prefix(root)
        .ok()
        .map(Path::to_path_buf)
        .or_else(|| {
            // The CLI prints resolved paths (/private/var/... on macOS)
            // while the root may be the symlinked spelling.
            let canon = std::fs::canonicalize(root).ok()?;
            dir.strip_prefix(canon).ok().map(Path::to_path_buf)
        })
        .unwrap_or_else(|| dir.to_path_buf());
    let rel = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    if rel.is_empty() {
        file
    } else {
        format!("{rel}::{file}")
    }
}

/// The file or folder an id (or a suite) names, relative to the root:
/// `dir::Foo.ql` is `dir/Foo.ql`, a bare suite `dir` is the folder.
pub fn id_path(id: &str) -> String {
    match id.rsplit_once("::") {
        Some((dir, file)) => format!("{dir}/{file}"),
        None => id.to_string(),
    }
}

/// Every CodeQL test inside the test packs `packs` (see [`test_packs`]),
/// as ids relative to `root`, sorted. Only what a full run covers is
/// listed, so the Testing view never shows a test "Run All" skips.
pub fn discover(root: &Path, packs: &[PathBuf]) -> Vec<String> {
    let mut ids: Vec<String> = packs
        .iter()
        .flat_map(|pack| walk(pack, None))
        .filter(|p| is_test(p))
        .map(|p| test_id(root, &p))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// `codeql` arguments that run every test: each test pack, relative to
/// `root`. `None` when there is no test pack under `root`, since then there
/// is nothing a full run should touch.
pub fn all_args(root: &Path, packs: &[PathBuf]) -> Option<Vec<String>> {
    let rel: Vec<String> = packs
        .iter()
        .filter_map(|p| p.strip_prefix(root).ok())
        .map(|p| {
            let s = p.display().to_string();
            if s.is_empty() { String::from(".") } else { s }
        })
        .collect();
    if rel.is_empty() {
        return None;
    }
    let mut args = vec![String::from("test"), String::from("run")];
    args.extend(rel);
    Some(args)
}

/// `codeql` arguments that run one test, a suite's folder, or whatever
/// path a filter names.
pub fn select_args(id: &str) -> Vec<String> {
    vec![String::from("test"), String::from("run"), id_path(id)]
}

/// One test's result: its id, pass or fail, the failing stage the CLI
/// named (`RESULT`, `COMPILATION`, ...), and for a failure the diff or
/// errors printed about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub id: String,
    pub status: TestStatus,
    pub stage: Option<String>,
    pub message: Vec<String>,
}

impl Outcome {
    pub fn case(&self) -> TestCase {
        TestCase {
            name: self.id.clone(),
            status: self.status,
        }
    }

    /// A one-line summary of a failure for the OUTPUT channel, or `None`
    /// for a pass.
    pub fn failure_summary(&self) -> Option<String> {
        if self.status != TestStatus::Failed {
            return None;
        }
        let stage = self.stage.as_deref().unwrap_or("FAILED");
        // The first line that says something: past the diff's own headers.
        let detail = self.message.iter().map(|l| l.trim()).find(|l| {
            !l.is_empty()
                && !l.starts_with("--- ")
                && !l.starts_with("+++ ")
                && !l.starts_with("@@")
        });
        Some(match detail {
            Some(d) => format!("{} failed ({stage}): {d}", self.id),
            None => format!("{} failed ({stage})", self.id),
        })
    }
}

/// A per-test result line: `[n/m ...] VERDICT path`, as the status, the
/// stage in `FAILED(<stage>)`, and the path. Anything else is `None`.
fn result_line(line: &str) -> Option<(TestStatus, Option<String>, &str)> {
    let rest = line.trim_start().strip_prefix('[')?;
    let (counter, rest) = rest.split_once(']')?;
    let (done, total) = counter.split_once('/')?;
    let total = total.split_whitespace().next()?;
    if done.parse::<u32>().is_err() || total.parse::<u32>().is_err() {
        return None;
    }
    let (verdict, path) = rest.trim_start().split_once(char::is_whitespace)?;
    let path = path.trim();
    if !(path.ends_with(".ql") || path.ends_with(".qlref")) {
        return None;
    }
    let (status, stage) = if verdict == "PASSED" {
        (TestStatus::Passed, None)
    } else if let Some(rest) = verdict.strip_prefix("FAILED") {
        let stage = rest
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .map(str::to_string);
        (TestStatus::Failed, stage)
    } else if verdict.starts_with("SKIPPED") {
        (TestStatus::Skipped, None)
    } else {
        return None;
    };
    Some((status, stage, path))
}

/// Lines that open a new phase of the run (or its closing summary): what
/// follows them no longer belongs to the previous test.
fn is_boundary(line: &str) -> bool {
    let t = line.trim();
    [
        "Executing ",
        "Extracting test database",
        "Compiling queries",
        "All ",
        "FAILED: ",
    ]
    .iter()
    .any(|p| t.starts_with(p))
        || (t.contains(" tests passed;") && t.contains(" tests failed"))
}

/// Whether a line is one of the CLI's own diagnostics.
fn is_diagnostic(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("ERROR:") || t.starts_with("WARNING:") || t.starts_with("Error:")
}

/// Whether `line` names a query file other than `file`.
fn names_other_query(line: &str, file: &str) -> bool {
    (line.contains(".ql") || line.contains(".qlref")) && !line.contains(file)
}

/// Streams `codeql test run` text output into [`Outcome`]s. An outcome is
/// complete when the next result line, a phase line or the end of output
/// arrives, since its diff follows the line that names it. Compiler errors
/// printed before a `FAILED(COMPILATION)` line are held and attached to it.
pub struct RunStream {
    root: PathBuf,
    open: Option<Outcome>,
    held: Vec<String>,
}

impl RunStream {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            open: None,
            held: Vec::new(),
        }
    }

    /// Feed one output line; returns the outcome it completed, if any.
    pub fn feed(&mut self, line: &str) -> Option<Outcome> {
        if let Some((status, stage, path)) = result_line(line) {
            let done = self.open.take();
            let path = Path::new(path);
            let abs = if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.root.join(path)
            };
            let held = std::mem::take(&mut self.held);
            self.open = Some(Outcome {
                id: test_id(&self.root, &abs),
                status,
                stage,
                message: if status == TestStatus::Failed {
                    held
                } else {
                    Vec::new()
                },
            });
            return done;
        }
        if is_boundary(line) {
            self.held.clear();
            return self.open.take();
        }
        match &mut self.open {
            Some(o) if o.status == TestStatus::Failed => {
                let file = o.id.rsplit("::").next().unwrap_or(&o.id).to_string();
                if is_diagnostic(line) && names_other_query(line, &file) {
                    // An error about the NEXT test, printed before its line.
                    self.held.push(line.to_string());
                } else if !line.trim().is_empty() {
                    o.message.push(line.to_string());
                }
            }
            _ if is_diagnostic(line) => self.held.push(line.to_string()),
            _ => {}
        }
        None
    }

    /// The outcome still open when the output ended.
    pub fn finish(&mut self) -> Option<Outcome> {
        self.open.take()
    }
}

/// Every outcome in a complete `codeql test run` output.
#[cfg(test)]
pub fn parse_output(root: &Path, text: &str) -> Vec<Outcome> {
    let mut stream = RunStream::new(root);
    let mut out: Vec<Outcome> = text.lines().filter_map(|l| stream.feed(l)).collect();
    out.extend(stream.finish());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/work/queries";

    /// A run with a pass, a result diff and a compilation failure whose
    /// error is printed before its result line, as the CLI prints them.
    const SAMPLE: &str = "\
Executing 3 tests in 2 directories.
Extracting test database in /work/queries/test/Sql.
Compiling queries in /work/queries/test/Sql.
Executing tests in /work/queries/test/Sql.
[1/3 comp 2.1s eval 412ms] PASSED /work/queries/test/Sql/SqlInjection.qlref
[2/3 comp 1.9s eval 388ms] FAILED(RESULT) /work/queries/test/Sql/Tainted.ql
--- expected
+++ actual
@@ -1,2 +1,3 @@
 | app.js:3:5:3:9 | query | tainted |
+| app.js:9:5:9:9 | other | tainted |
Extracting test database in /work/queries/test/Broken.
Compiling queries in /work/queries/test/Broken.
ERROR: could not resolve type Foo (/work/queries/test/Broken/Broken.ql:4,8-11)
[3/3] FAILED(COMPILATION) /work/queries/test/Broken/Broken.ql
1 tests passed; 2 tests failed:
  FAILED: /work/queries/test/Sql/Tainted.ql
  FAILED: /work/queries/test/Broken/Broken.ql
";

    #[test]
    fn result_lines_give_status_stage_and_path() {
        assert_eq!(
            result_line("[1/3 comp 2.1s eval 412ms] PASSED /a/Foo.qlref"),
            Some((TestStatus::Passed, None, "/a/Foo.qlref"))
        );
        assert_eq!(
            result_line("[12/40] FAILED(RESULT) rel/Bar.ql"),
            Some((
                TestStatus::Failed,
                Some(String::from("RESULT")),
                "rel/Bar.ql"
            ))
        );
        // Summary entries, diff lines and chatter are not results.
        assert_eq!(result_line("  FAILED: /a/Bar.ql"), None);
        assert_eq!(result_line("+| [1/2] PASSED x.ql |"), None);
        assert_eq!(result_line("[a/b] PASSED /a/Foo.ql"), None);
        assert_eq!(result_line("[1/1] PASSED /a/notes.txt"), None);
    }

    #[test]
    fn a_run_yields_passes_diffs_and_compilation_errors() {
        let out = parse_output(Path::new(ROOT), SAMPLE);
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[0].id, "test/Sql::SqlInjection.qlref");
        assert_eq!(out[0].status, TestStatus::Passed);
        assert!(out[0].message.is_empty());

        assert_eq!(out[1].id, "test/Sql::Tainted.ql");
        assert_eq!(out[1].status, TestStatus::Failed);
        assert_eq!(out[1].stage.as_deref(), Some("RESULT"));
        assert_eq!(
            out[1].message,
            vec![
                "--- expected",
                "+++ actual",
                "@@ -1,2 +1,3 @@",
                " | app.js:3:5:3:9 | query | tainted |",
                "+| app.js:9:5:9:9 | other | tainted |",
            ],
            "the diff, and nothing past the next phase line"
        );
        assert_eq!(
            out[1].failure_summary().as_deref(),
            Some("test/Sql::Tainted.ql failed (RESULT): | app.js:3:5:3:9 | query | tainted |")
        );

        assert_eq!(out[2].id, "test/Broken::Broken.ql");
        assert_eq!(out[2].stage.as_deref(), Some("COMPILATION"));
        assert_eq!(
            out[2].message,
            vec!["ERROR: could not resolve type Foo (/work/queries/test/Broken/Broken.ql:4,8-11)"],
            "an error printed before the result line belongs to it"
        );
    }

    #[test]
    fn an_error_after_its_result_line_and_the_last_test_are_kept() {
        let text = "\
[1/2 comp 1s] FAILED(COMPILATION) /work/queries/A.ql
ERROR: could not resolve module Bar (/work/queries/A.ql:1,8-11)
ERROR: could not resolve type Baz (/work/queries/B.ql:2,1-4)
[2/2] FAILED(COMPILATION) /work/queries/B.ql
";
        let out = parse_output(Path::new(ROOT), text);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "A.ql", "a test at the root has no suite");
        assert_eq!(
            out[0].message,
            vec!["ERROR: could not resolve module Bar (/work/queries/A.ql:1,8-11)"]
        );
        assert_eq!(
            out[1].message,
            vec!["ERROR: could not resolve type Baz (/work/queries/B.ql:2,1-4)"],
            "the output ended with B open; finish() still returns it"
        );
        let all_passed =
            "[1/1 comp 1s eval 2ms] PASSED /work/queries/t/A.ql\nAll 1 tests passed.\n";
        let out = parse_output(Path::new(ROOT), all_passed);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].status, TestStatus::Passed);
        assert_eq!(out[0].failure_summary(), None);
    }

    #[test]
    fn ids_map_to_the_paths_codeql_takes() {
        let root = Path::new(ROOT);
        assert_eq!(
            test_id(root, Path::new("/work/queries/a/b/X.qlref")),
            "a/b::X.qlref"
        );
        assert_eq!(id_path("a/b::X.qlref"), "a/b/X.qlref");
        assert_eq!(id_path("a/b"), "a/b", "a suite is its folder");
        assert_eq!(select_args("X.ql"), vec!["test", "run", "X.ql"]);
        assert_eq!(
            all_args(root, &[root.join("test"), root.join("java/ql/test")]).unwrap(),
            vec!["test", "run", "test", "java/ql/test"]
        );
        assert_eq!(
            all_args(root, &[root.to_path_buf()]).unwrap(),
            vec!["test", "run", "."]
        );
        assert_eq!(all_args(root, &[]), None, "no test pack, no full run");
    }

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn markers() -> Vec<String> {
        vec![String::from("qlpack.yml"), String::from("codeql-pack.yml")]
    }

    #[test]
    fn discovery_finds_queries_with_expected_results() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/qlpack.yml", "name: acme/queries\n");
        write(root, "src/Find.ql", "select 1");
        write(
            root,
            "test/qlpack.yml",
            "name: acme/tests\nextractor: javascript\ntests: .\n",
        );
        write(root, "test/Find/Find.qlref", "Find.ql\n");
        write(root, "test/Find/Find.expected", "");
        write(root, "test/Find/test.js", "x()");
        write(root, "test/Inline/Inline.ql", "select 1");
        write(root, "test/Inline/Inline.expected", "");
        // No .expected: not a test. A test database's contents never are.
        write(root, "test/Draft/Draft.ql", "select 1");
        write(root, "test/Find/Find.testproj/x/Y.ql", "");
        write(root, "test/Find/Find.testproj/x/Y.expected", "");
        let packs = test_packs(root, &markers());
        assert_eq!(packs, vec![root.join("test")]);
        assert_eq!(
            discover(root, &packs),
            vec!["test/Find::Find.qlref", "test/Inline::Inline.ql"]
        );
        // A test outside every test pack is not listed: "Run All" runs the
        // packs, so it would never run.
        write(root, "loose/Loose.ql", "select 1");
        write(root, "loose/Loose.expected", "");
        assert_eq!(discover(root, &packs).len(), 2);
        assert!(discover(root, &[]).is_empty());
    }

    #[test]
    fn only_packs_that_hold_tests_are_test_packs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // A query pack naming its extractor, with no tests: not a test pack.
        write(root, "codeql-pack.yml", "name: acme/q\nextractor: go\n");
        write(root, "Q.ql", "select 1");
        assert!(test_packs(root, &markers()).is_empty());
        // A nested `tests:` key is some other map's, not the pack's.
        write(root, "other/qlpack.yml", "name: x\nbuild:\n  tests: yes\n");
        assert!(test_packs(root, &markers()).is_empty());
        // Once the extractor pack holds a test, it is one.
        write(root, "Q.expected", "");
        assert_eq!(test_packs(root, &markers()), vec![root.to_path_buf()]);
        assert_eq!(declares_tests("tests: .\n"), (true, false));
    }
}
