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

/// Editors whose `scheme://file/<path>` deep links open here. A terminal
/// hyperlink can carry any URI while displaying unrelated text, so the
/// `file/` shape alone is not the gate: `https://file//etc/hosts` is a web
/// URL whose host happens to be `file`, not an editor link, and must fall
/// through to the caller's web-only rule.
const EDITOR_LINK_SCHEMES: [&str; 5] = ["vscode", "vscode-insiders", "cursor", "windsurf", "zed"];

/// An editor deep-link URI carried by a terminal hyperlink:
/// `scheme://file/<abs path>[:line[:col]]` (VS Code's shape, printed
/// identically by Cursor, Windsurf, and Zed — VS Code's canonical form
/// doubles the slash, `vscode://file//Users/…`, and the single-slash
/// variant appears in the wild too) or a plain `file://<abs path>`.
/// Croft is an editor, so these open here rather than launching the
/// scheme's own app. The path is percent-decoded; line/column stay
/// 1-based, defaulting to line 1 when the URI carries none.
pub fn editor_file_uri(url: &str) -> Option<FileRef> {
    let (scheme, rest) = url.trim().split_once("://")?;
    if scheme.eq_ignore_ascii_case("file") {
        // file://[host]/abs/path — the scheme has no line-suffix convention.
        let path = decode_path(&rest[rest.find('/')?..])?;
        return Some(FileRef {
            path,
            line: 1,
            column: None,
        });
    }
    if !EDITOR_LINK_SCHEMES
        .iter()
        .any(|s| scheme.eq_ignore_ascii_case(s))
    {
        return None;
    }
    let tail = rest.strip_prefix("file/")?;
    let (raw_path, line, column) = split_line_suffix(tail);
    let mut path = decode_path(raw_path)?;
    if !path.starts_with('/') {
        path.insert(0, '/');
    }
    Some(FileRef { path, line, column })
}

/// A two-file deep link from a report's group cell:
/// `diff://open?left=<enc path[:line[:col]]>&right=<enc …>` (cgr duplicates
/// emits this — single-file schemes like `vscode://file/` cannot carry a
/// pair). Values are fully percent-encoded; both sides must decode to
/// absolute paths.
pub fn diff_uri(url: &str) -> Option<(FileRef, FileRef)> {
    let rest = url.trim().strip_prefix("diff://")?;
    let query = rest.split_once('?').map(|(_, q)| q).unwrap_or(rest);
    let mut left = None;
    let mut right = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        let decoded = decode_path(value)?;
        let (path, line, column) = split_line_suffix(&decoded);
        if !path.starts_with('/') {
            return None;
        }
        let fr = FileRef {
            path: path.to_string(),
            line,
            column,
        };
        match key {
            "left" => left = Some(fr),
            "right" => right = Some(fr),
            _ => return None,
        }
    }
    Some((left?, right?))
}

/// Split a trailing `:line[:col]` off an editor URI path. Digit runs are
/// bounded like PATH_LINE_RE's, so an overlong run reads as path text —
/// and positions are 1-based, so a `:0` suffix reads as path text too.
fn split_line_suffix(s: &str) -> (&str, u32, Option<u32>) {
    let Some((head, last)) = numeric_suffix(s) else {
        return (s, 1, None);
    };
    match numeric_suffix(head) {
        Some((rest, line)) => (rest, line, Some(last)),
        None => (head, last, None),
    }
}

fn numeric_suffix(s: &str) -> Option<(&str, u32)> {
    let (head, tail) = s.rsplit_once(':')?;
    if tail.is_empty() || tail.len() > 7 || !tail.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((head, tail.parse().ok().filter(|&n| n > 0)?))
}

fn decode_path(raw: &str) -> Option<String> {
    let decoded =
        String::from_utf8(crate::shell_integration::percent_decode(raw.as_bytes())).ok()?;
    (!decoded.is_empty()).then_some(decoded)
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

    #[test]
    fn vscode_deep_link_with_doubled_slash_parses() {
        let r = editor_file_uri("vscode://file//Users/me/repo/a.py:42").unwrap();
        assert_eq!(
            (r.path.as_str(), r.line, r.column),
            ("/Users/me/repo/a.py", 42, None)
        );
    }

    #[test]
    fn single_slash_variant_and_column_parse() {
        let r = editor_file_uri("cursor://file/Users/me/a.rs:12:7").unwrap();
        assert_eq!(
            (r.path.as_str(), r.line, r.column),
            ("/Users/me/a.rs", 12, Some(7))
        );
    }

    #[test]
    fn lineless_deep_link_defaults_to_line_one() {
        let r = editor_file_uri("zed://file//tmp/x.txt").unwrap();
        assert_eq!((r.path.as_str(), r.line, r.column), ("/tmp/x.txt", 1, None));
    }

    #[test]
    fn percent_encoded_spaces_decode() {
        let r = editor_file_uri("vscode://file//My%20Repo/mod.py:7").unwrap();
        assert_eq!((r.path.as_str(), r.line), ("/My Repo/mod.py", 7));
    }

    #[test]
    fn plain_file_uri_parses_without_line() {
        let r = editor_file_uri("file:///etc/hosts").unwrap();
        assert_eq!((r.path.as_str(), r.line, r.column), ("/etc/hosts", 1, None));
        // An authority component is skipped, not read as the path.
        let r = editor_file_uri("file://localhost/etc/hosts").unwrap();
        assert_eq!(r.path, "/etc/hosts");
    }

    #[test]
    fn non_file_uris_are_refused() {
        assert!(editor_file_uri("https://example.com/a.py:3").is_none());
        assert!(editor_file_uri("vscode://settings/keybindings").is_none());
        assert!(editor_file_uri("mailto:a@b.c").is_none());
        assert!(editor_file_uri("vscode://file/").is_none());
    }

    #[test]
    fn foreign_schemes_with_a_file_host_are_refused() {
        // A web URL whose host is literally `file` matches the `file/`
        // shape but is not an editor link; only allowlisted schemes pass.
        assert!(editor_file_uri("https://file//etc/hosts").is_none());
        assert!(editor_file_uri("ftp://file//etc/hosts").is_none());
        assert!(editor_file_uri("notepad://file//etc/hosts").is_none());
    }

    #[test]
    fn zero_line_or_column_reads_as_path_text() {
        // Positions are 1-based: `:0` is not a position, so the suffix
        // stays in the path (which then fails the caller's is_file gate).
        let r = editor_file_uri("vscode://file//tmp/a.rs:0").unwrap();
        assert_eq!(
            (r.path.as_str(), r.line, r.column),
            ("/tmp/a.rs:0", 1, None)
        );
        let r = editor_file_uri("vscode://file//tmp/a.rs:12:0").unwrap();
        assert_eq!(
            (r.path.as_str(), r.line, r.column),
            ("/tmp/a.rs:12:0", 1, None)
        );
    }

    #[test]
    fn diff_uri_parses_both_sides_with_lines() {
        let url = "diff://open?left=%2Frepo%2Fa.py%3A5&right=%2Frepo%2Fb.py%3A8";
        let (l, r) = diff_uri(url).unwrap();
        assert_eq!((l.path.as_str(), l.line), ("/repo/a.py", 5));
        assert_eq!((r.path.as_str(), r.line), ("/repo/b.py", 8));
    }

    #[test]
    fn diff_uri_decodes_spaces_and_defaults_lines() {
        let url = "diff://open?left=%2FMy%20Repo%2Fa.py&right=%2FMy%20Repo%2Fb.py";
        let (l, r) = diff_uri(url).unwrap();
        assert_eq!((l.path.as_str(), l.line), ("/My Repo/a.py", 1));
        assert_eq!(r.path, "/My Repo/b.py");
    }

    #[test]
    fn diff_uri_refuses_partial_relative_or_foreign_forms() {
        // One side missing.
        assert!(diff_uri("diff://open?left=%2Fa.py%3A1").is_none());
        // Relative path.
        assert!(diff_uri("diff://open?left=a.py%3A1&right=%2Fb.py%3A2").is_none());
        // Unknown key.
        assert!(diff_uri("diff://open?left=%2Fa&middle=%2Fm&right=%2Fb").is_none());
        // Different scheme entirely.
        assert!(diff_uri("vscode://file//a.py:1").is_none());
    }
}
