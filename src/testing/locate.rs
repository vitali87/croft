//! Locate a test's source location by name. libtest's `--list` gives no file or
//! line (the JSON format that does is nightly-only), so we find the `fn` in the
//! source tree instead: walk the workspace (honouring `.gitignore`, so `target`
//! is skipped), grep for `fn <leaf>`, and rank candidates by how well the file
//! path matches the test's module path. Good enough to jump the editor there.

use std::path::{Path, PathBuf};

use grep_regex::RegexMatcher;
use grep_searcher::{BinaryDetection, MmapChoice, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::WalkBuilder;

const RUST_EXT: &str = "rs";

/// Records the first matching line in a file and stops scanning it.
struct FirstLine(Option<u64>);

impl Sink for FirstLine {
    type Error = std::io::Error;
    fn matched(&mut self, _s: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, std::io::Error> {
        self.0 = mat.line_number();
        Ok(false) // one hit is enough
    }
}

/// The source location `(path, line)` of the test named `full_name` (e.g.
/// `widgets::testing::tests::foo`), or `None` if no `fn` is found. `line` is
/// 0-based, ready for [`crate::app`]'s go-to-definition. pytest node IDs
/// (`tests/test_x.py::test_y`) carry their file, so those skip the walk and
/// grep just that file for the `def`.
pub fn find_test_source(root: &Path, full_name: &str) -> Option<(PathBuf, u32)> {
    if full_name.contains(".py::") {
        return pytest_source(root, full_name);
    }
    let mut segments: Vec<&str> = full_name.split("::").collect();
    let leaf = segments.pop()?;
    // Module segments minus the conventional `tests` wrapper: these are what a
    // file path is scored against (e.g. `widgets::testing` -> src/widgets/testing.rs).
    let module: Vec<&str> = segments.into_iter().filter(|s| *s != "tests").collect();

    // `fn <leaf>` followed by `(`, `<` (generics) or whitespace. leaf is a Rust
    // identifier, so nothing to escape.
    let matcher = RegexMatcher::new(&format!(r"\bfn\s+{leaf}\s*[(<\s]")).ok()?;
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .memory_map(MmapChoice::never())
        .build();

    let mut best: Option<(PathBuf, u32, usize)> = None; // (path, line, score)
    for entry in WalkBuilder::new(root).build().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some(RUST_EXT) {
            continue;
        }
        let mut sink = FirstLine(None);
        if searcher.search_path(&matcher, path, &mut sink).is_err() {
            continue;
        }
        let Some(line1) = sink.0 else { continue };
        let score = path_score(path, &module);
        let line0 = line1.saturating_sub(1) as u32;
        if best.as_ref().is_none_or(|(_, _, s)| score > *s) {
            best = Some((path.to_path_buf(), line0, score));
        }
    }
    best.map(|(p, l, _)| (p, l))
}

/// Resolve a pytest node ID: the segment before `.py::` is the file (relative
/// to the workspace root), the last segment the test name — its parametrize
/// suffix (`[1]`) stripped, since the source `def` carries no bracket.
fn pytest_source(root: &Path, full_name: &str) -> Option<(PathBuf, u32)> {
    let (file, rest) = full_name.split_once(".py::")?;
    let path = root.join(format!("{file}.py"));
    let leaf = rest.rsplit("::").next()?.split('[').next()?;
    let matcher = RegexMatcher::new(&format!(r"\bdef\s+{leaf}\s*\(")).ok()?;
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .memory_map(MmapChoice::never())
        .build();
    let mut sink = FirstLine(None);
    searcher.search_path(&matcher, &path, &mut sink).ok()?;
    let line1 = sink.0?;
    Some((path, line1.saturating_sub(1) as u32))
}

/// The name of the nearest `fn` at or above `cursor_row` — the test the caret
/// sits in, for run-at-cursor. Scans upward and returns the first `fn <ident>`
/// it finds. `None` if the caret is above every function.
pub fn enclosing_fn_name(lines: &[String], cursor_row: usize) -> Option<String> {
    let start = cursor_row.min(lines.len().saturating_sub(1));
    for line in lines[..=start].iter().rev() {
        if let Some(name) = fn_name_in(line) {
            return Some(name);
        }
    }
    None
}

/// The name of the TEST function whose definition starts on `lines[idx]`, or
/// `None` when the line defines no function or the function is not a test.
/// Backs the editor's gutter play glyph. Python follows pytest's collection
/// convention (`def test_*` in a `test_*.py` / `*_test.py` file); Rust
/// requires a `test` attribute (`#[test]`, `#[tokio::test]`, `#[rstest]`,
/// ...) in the contiguous attribute block directly above the `fn`. `path` is
/// the buffer's file: an unsaved buffer gets no beads (nothing could run
/// them), and the extension picks the language rules.
pub fn test_fn_on_line(path: Option<&Path>, lines: &[String], idx: usize) -> Option<String> {
    let file = path?.file_name()?.to_str()?;
    let line = lines.get(idx)?;
    if file.ends_with(".py") {
        let name = name_after(line, "def ")?;
        // pytest's default collection reads test_*.py / *_test.py only: a
        // `def test_connection()` in production code gets no bead.
        let collectable = file.starts_with("test_") || file.ends_with("_test.py");
        return (collectable && name.starts_with("test")).then_some(name);
    }
    if !file.ends_with(".rs") {
        return None;
    }
    let name = name_after(line, "fn ")?;
    for above in lines[..idx].iter().rev() {
        let t = above.trim_start();
        if !t.starts_with("#[") {
            break;
        }
        if attr_marks_test(t) {
            return Some(name);
        }
    }
    None
}

/// Whether an attribute line marks a test fn: the attribute path's LAST
/// segment IS `test` (`#[test]`, `#[tokio::test]`, `#[googletest::test]`)
/// or `rstest` — never a mere suffix. `#[contest]`/`#[mytest]` are ordinary
/// attributes cargo does not collect, and a bead on them runs zero tests.
/// `#[cfg(test)]` and `#[cfg_attr(test, ...)]` are configuration (path
/// segment `cfg`/`cfg_attr`), and treating them as test markers put a play
/// bead on every `#[cfg(test)] fn helper()` in a codebase.
fn attr_marks_test(attr_line: &str) -> bool {
    let inner = attr_line.trim_start_matches("#[");
    let name_end = inner
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .unwrap_or(inner.len());
    inner[..name_end]
        .rsplit("::")
        .next()
        .is_some_and(|seg| seg == "test" || seg == "rstest")
}

/// Extract the identifier after a function keyword (`fn ` for Rust, `def ` for
/// Python) in a single line, or `None`. Deliberately simple: it does not skip
/// keywords inside comments or strings, which is a rare enough case for a
/// run-at-cursor convenience that it isn't worth a parser.
fn fn_name_in(line: &str) -> Option<String> {
    ["fn ", "def "].iter().find_map(|kw| name_after(line, kw))
}

fn name_after(line: &str, keyword: &str) -> Option<String> {
    let idx = line.find(keyword)?;
    let name: String = line[idx + keyword.len()..]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    // The keyword must sit on a word boundary (start of line or after
    // whitespace), and the name non-empty — guards against `define_fn foo`.
    let before = &line[..idx];
    let boundary = before.is_empty() || before.ends_with(|c: char| c.is_whitespace());
    (boundary && !name.is_empty()).then_some(name)
}

/// How many module segments appear as components of `path` — the more, the more
/// likely this file defines the test (disambiguates same-named fns).
fn path_score(path: &Path, module: &[&str]) -> usize {
    let components: Vec<&str> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    module
        .iter()
        .filter(|seg| {
            // A module segment matches a path component, or the file stem (the
            // last module segment is usually the file name, e.g. `testing.rs`).
            components
                .iter()
                .any(|c| c == *seg || c.strip_suffix(".rs") == Some(seg))
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_fn_and_prefers_the_module_matching_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // A decoy in an unrelated file, and the real one under the module path.
        std::fs::create_dir_all(root.join("src/other")).unwrap();
        std::fs::create_dir_all(root.join("src/widgets")).unwrap();
        std::fs::write(
            root.join("src/other/misc.rs"),
            "fn helper() {}\nfn target_test() { /* decoy */ }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/widgets/testing.rs"),
            "mod tests {\n    fn setup() {}\n    fn target_test() { assert!(true); }\n}\n",
        )
        .unwrap();

        let (path, line) = find_test_source(root, "widgets::testing::tests::target_test").unwrap();
        assert!(
            path.ends_with("src/widgets/testing.rs"),
            "the module-matching file wins over the decoy, got {path:?}"
        );
        assert_eq!(line, 2, "0-based line of `fn target_test` in testing.rs");
    }

    #[test]
    fn enclosing_fn_scans_upward_to_the_nearest_fn() {
        let lines: Vec<String> = [
            "mod tests {",
            "    fn first() {",
            "        assert!(true);", // cursor here -> first
            "    }",
            "    fn second() {",
            "        assert!(false);", // cursor here -> second
            "    }",
            "}",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        assert_eq!(enclosing_fn_name(&lines, 2).as_deref(), Some("first"));
        assert_eq!(enclosing_fn_name(&lines, 5).as_deref(), Some("second"));
        // Above every fn: nothing to run.
        assert_eq!(enclosing_fn_name(&lines, 0), None);
        // A word merely ending in "fn" is not a keyword boundary.
        let noise = vec![String::from("let define_fn_thing = 3;")];
        assert_eq!(enclosing_fn_name(&noise, 0), None);
    }

    #[test]
    fn pytest_node_ids_resolve_to_the_def_in_their_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(
            root.join("tests/test_sample.py"),
            "import pytest\n\ndef test_passes():\n    assert True\n\nclass TestGroup:\n    def test_method(self):\n        assert True\n",
        )
        .unwrap();

        let (path, line) = find_test_source(root, "tests/test_sample.py::test_passes").unwrap();
        assert!(path.ends_with("tests/test_sample.py"), "got {path:?}");
        assert_eq!(line, 2, "0-based line of `def test_passes`");
        let (_, line) =
            find_test_source(root, "tests/test_sample.py::TestGroup::test_method").unwrap();
        assert_eq!(line, 6, "class methods resolve through the last segment");
        // Parametrized IDs strip their bracket suffix before the lookup.
        let (_, line) = find_test_source(root, "tests/test_sample.py::test_passes[1]").unwrap();
        assert_eq!(line, 2);
        // A node ID pointing at a missing file finds nothing.
        assert!(find_test_source(root, "tests/gone.py::test_x").is_none());
    }

    #[test]
    fn enclosing_fn_understands_python_defs() {
        let lines: Vec<String> = [
            "import pytest",
            "def test_first():",
            "    assert True", // cursor here -> test_first
            "class TestGroup:",
            "    def test_method(self):",
            "        assert True", // cursor here -> test_method
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(enclosing_fn_name(&lines, 2).as_deref(), Some("test_first"));
        assert_eq!(enclosing_fn_name(&lines, 5).as_deref(), Some("test_method"));
        assert_eq!(enclosing_fn_name(&lines, 0), None);
    }

    #[test]
    fn returns_none_when_the_fn_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn something_else() {}\n").unwrap();
        assert!(find_test_source(tmp.path(), "m::nope").is_none());
    }

    #[test]
    fn test_fn_on_line_detects_rust_test_attributes() {
        let lines: Vec<String> = [
            "fn helper() {}",
            "#[test]",
            "fn plain_case() {}",
            "#[rstest]",
            "#[tokio::test(flavor = \"multi_thread\")]",
            "async fn async_case() {}",
            "#[derive(Debug)]",
            "fn derived_not_a_test() {}",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let rs = Some(Path::new("a.rs"));
        assert_eq!(
            test_fn_on_line(rs, &lines, 0),
            None,
            "no attribute, no test"
        );
        assert_eq!(
            test_fn_on_line(rs, &lines, 2).as_deref(),
            Some("plain_case")
        );
        assert_eq!(
            test_fn_on_line(rs, &lines, 5).as_deref(),
            Some("async_case"),
            "tokio::test in a stacked attribute block must count"
        );
        assert_eq!(
            test_fn_on_line(rs, &lines, 7),
            None,
            "a non-test attribute above a fn must not count"
        );
        assert_eq!(
            test_fn_on_line(rs, &lines, 1),
            None,
            "attribute lines define no fn"
        );
    }

    #[test]
    fn cfg_test_is_configuration_not_a_test_attribute() {
        // croft's own source has dozens of `#[cfg(test)] fn helper()` sites
        // (clipboard.rs, port_detect.rs, overlay.rs...). A bead on those
        // runs `cargo test <helper_name>`, matches nothing, and wipes the
        // Testing tree with an empty run.
        let lines: Vec<String> = [
            "#[cfg(test)]",
            "pub fn clear_native() {}",
            "#[cfg(not(test))]",
            "fn scan() {}",
            "#[cfg_attr(test, derive(Clone))]",
            "fn configured() {}",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let rs = Some(Path::new("a.rs"));
        assert_eq!(
            test_fn_on_line(rs, &lines, 1),
            None,
            "#[cfg(test)] gates compilation; it does not mark a test"
        );
        assert_eq!(test_fn_on_line(rs, &lines, 3), None);
        assert_eq!(
            test_fn_on_line(rs, &lines, 5),
            None,
            "cfg_attr is configuration"
        );
    }

    #[test]
    fn suffix_named_attributes_are_not_test_markers() {
        // `#[contest]` / `#[mytest]` are ordinary proc-macro attributes
        // cargo never collects; a play bead on them runs zero tests. Only
        // the exact `test` segment (bare or path-qualified) and `rstest`
        // mark a test.
        let lines: Vec<String> = [
            "#[contest]",
            "fn fake_case() {}",
            "#[mytest]",
            "fn other_fake_case() {}",
            "#[googletest::test]",
            "fn qualified_case() {}",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let rs = Some(Path::new("a.rs"));
        assert_eq!(
            test_fn_on_line(rs, &lines, 1),
            None,
            "#[contest] merely ends in `test`"
        );
        assert_eq!(
            test_fn_on_line(rs, &lines, 3),
            None,
            "#[mytest] merely ends in `test`"
        );
        assert_eq!(
            test_fn_on_line(rs, &lines, 5).as_deref(),
            Some("qualified_case"),
            "a path-qualified exact `test` segment still counts"
        );
    }

    #[test]
    fn test_fn_on_line_detects_pytest_defs_by_name() {
        let lines: Vec<String> = [
            "def test_first():",
            "def helper():",
            "    async def test_inner(self):",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let tf = Some(Path::new("test_app.py"));
        assert_eq!(
            test_fn_on_line(tf, &lines, 0).as_deref(),
            Some("test_first")
        );
        assert_eq!(
            test_fn_on_line(tf, &lines, 1),
            None,
            "pytest only collects test_*"
        );
        assert_eq!(
            test_fn_on_line(tf, &lines, 2).as_deref(),
            Some("test_inner")
        );
        assert_eq!(
            test_fn_on_line(Some(Path::new("app_test.py")), &lines, 0).as_deref(),
            Some("test_first"),
            "the *_test.py collection form counts too"
        );
        assert_eq!(
            test_fn_on_line(Some(Path::new("app.py")), &lines, 0),
            None,
            "a def test_* in production code (non-collectable file) gets no bead"
        );
        assert_eq!(
            test_fn_on_line(None, &lines, 0),
            None,
            "an unsaved buffer has nothing a runner could target"
        );
    }
}
