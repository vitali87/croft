//! File references (`path:line[:col]`) detected in terminal text.
//!
//! Powers Cmd/Ctrl+click on compiler / test / grep output in any terminal
//! pane: `src/merge.rs:127:19`, `./x.py:12`, `/abs/f.c:10`, `~/f.rs:3`, and
//! Python's `File "x.py", line 12` traceback form all resolve to a jump
//! target. Because croft owns the PTY, this works for anything printed by
//! any command, with no per-tool "problem matcher" configuration.
//!
//! Structural sibling of `port_detect::url_at`: given one row of terminal
//! text and the clicked character column, return the reference under the
//! pointer. Path existence is the caller's problem (the app checks the
//! resolved path before opening, which also filters false positives like
//! `host.com:443`).

use std::sync::LazyLock;

use regex::Regex;

/// A `path:line[:col]` reference. `line` / `column` are 1-based, as printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRef {
    pub path: String,
    pub line: u32,
    pub column: Option<u32>,
}

/// `path:line[:col]` where the path token contains a `/` or a `.` so bare
/// numbers (`12:30`) and shell timestamps never match. Path characters
/// mirror what compilers and grep print; quotes / brackets / whitespace
/// terminate the token.
static PATH_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
        (?P<path> [~]? [A-Za-z0-9_@+\-./]* [/.] [A-Za-z0-9_@+\-./]* )
        : (?P<line> \d{1,7} )
        (?: : (?P<col> \d{1,7} ) )?
    ",
    )
    .expect("path:line regex")
});

/// Python traceback form: `File "src/x.py", line 12`.
static PY_TRACEBACK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"File "(?P<path>[^"]+)", line (?P<line>\d{1,7})"#).expect("traceback regex")
});

/// The file reference covering character index `col` in `text`, if any.
pub fn file_ref_at(text: &str, col: usize) -> Option<FileRef> {
    // Traceback form first: its span covers `File "…"` through the line
    // number, and a quoted path may contain characters the generic token
    // regex refuses (e.g. spaces).
    for c in PY_TRACEBACK_RE.captures_iter(text) {
        let m = c.get(0).expect("whole match");
        let start = text[..m.start()].chars().count();
        let end = start + m.as_str().chars().count();
        if col >= start && col < end {
            return Some(FileRef {
                path: c["path"].to_string(),
                line: c["line"].parse().ok()?,
                column: None,
            });
        }
    }
    for c in PATH_LINE_RE.captures_iter(text) {
        let m = c.get(0).expect("whole match");
        let start = text[..m.start()].chars().count();
        let end = start + m.as_str().chars().count();
        if col >= start && col < end {
            return Some(FileRef {
                path: c["path"].to_string(),
                line: c["line"].parse().ok()?,
                column: c.name("col").and_then(|v| v.as_str().parse().ok()),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, col: usize) -> Option<FileRef> {
        file_ref_at(text, col)
    }

    #[test]
    fn finds_a_rustc_style_reference_under_the_cursor() {
        let text = "  --> src/merge.rs:127:19";
        let r = at(text, 8).unwrap();
        assert_eq!(r.path, "src/merge.rs");
        assert_eq!(r.line, 127);
        assert_eq!(r.column, Some(19));
        // Clicking the line number is still inside the reference.
        assert!(at(text, 20).is_some());
        // Clicking the arrow is not.
        assert!(at(text, 3).is_none());
    }

    #[test]
    fn column_is_optional() {
        let r = at("g.py:12: error", 2).unwrap();
        assert_eq!((r.path.as_str(), r.line, r.column), ("g.py", 12, None));
    }

    #[test]
    fn absolute_dotted_and_home_paths_match() {
        assert_eq!(at("/a/b/f.c:10", 4).unwrap().path, "/a/b/f.c");
        assert_eq!(at("./rel/f.ts:3", 5).unwrap().path, "./rel/f.ts");
        assert_eq!(at("~/notes/todo.md:1", 5).unwrap().path, "~/notes/todo.md");
    }

    #[test]
    fn bare_numbers_and_timestamps_never_match() {
        assert!(at("12:30", 1).is_none());
        assert!(at("at 12:30:45 today", 5).is_none());
    }

    #[test]
    fn python_traceback_form_matches_anywhere_inside_it() {
        let text = r#"  File "src/app.py", line 42, in main"#;
        let r = at(text, 10).unwrap();
        assert_eq!(
            (r.path.as_str(), r.line, r.column),
            ("src/app.py", 42, None)
        );
        // The `line 42` words are part of the reference too.
        assert!(at(text, 24).is_some());
    }

    #[test]
    fn grep_output_with_trailing_text_keeps_only_the_location() {
        let r = at("src/x.rs:12:some matched text", 3).unwrap();
        assert_eq!((r.path.as_str(), r.line, r.column), ("src/x.rs", 12, None));
    }
}
