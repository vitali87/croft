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
/// 0-based, ready for [`crate::app`]'s go-to-definition.
pub fn find_test_source(root: &Path, full_name: &str) -> Option<(PathBuf, u32)> {
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
    fn returns_none_when_the_fn_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn something_else() {}\n").unwrap();
        assert!(find_test_source(tmp.path(), "m::nope").is_none());
    }
}
