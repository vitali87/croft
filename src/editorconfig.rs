//! `.editorconfig` support (VS Code has no built-in EditorConfig; the
//! `editorconfig.editorconfig` extension is the standard fill, and this was
//! a documented gap in `src/vscode_extensions.rs`).
//!
//! Reads the [EditorConfig](https://editorconfig.org) files that apply to a
//! path and resolves the properties croft can honour:
//!
//! | property                   | effect in croft                          |
//! |----------------------------|------------------------------------------|
//! | `indent_style`             | spaces vs tabs for typed indentation     |
//! | `indent_size` / `tab_width`| the indent width                         |
//! | `end_of_line`              | the line ending written on save          |
//! | `trim_trailing_whitespace` | strips trailing spaces on save           |
//! | `insert_final_newline`     | guarantees a trailing newline on save    |
//!
//! Unknown properties are ignored rather than rejected, per the spec: a file
//! written for another editor must not make croft refuse the ones it shares.

#[cfg(test)]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
/// A line-ending choice from `end_of_line`. Kept local rather than reusing
/// the editor's `LineEnding` so this module stays independently testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    Lf,
    Crlf,
}

/// The resolved properties for one file. Every field is `Option` because
/// "unset" and "set to the default" are different: only a property some
/// `.editorconfig` actually named may override croft's own detection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Props {
    /// `Some(true)` for `indent_style = space`, `Some(false)` for `tab`.
    pub use_spaces: Option<bool>,
    /// `indent_size`, or `tab_width` when `indent_size` is absent or `tab`.
    pub indent_width: Option<u32>,
    pub eol: Option<Eol>,
    pub trim_trailing_whitespace: Option<bool>,
    pub insert_final_newline: Option<bool>,
    /// `charset` (#1432): the encoding a file without a BOM opens and saves
    /// in. `utf-8-bom` also asks for the mark on save.
    pub charset: Option<crate::file_encoding::Charset>,
}

impl Props {
    /// Whether any property at all was set, i.e. whether a `.editorconfig`
    /// had anything to say about this file.
    pub fn is_empty(&self) -> bool {
        *self == Props::default()
    }
}

/// `indent_size` as written: a width, or `tab` to defer to `tab_width`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndentSize {
    Width(u32),
    Tab,
}

/// Properties as parsed, before the indent width is derived. `indent_size`
/// and `tab_width` are held apart because their precedence is not "last one
/// read wins": a numeric `indent_size` beats `tab_width` whichever line or
/// file comes later, so the width can only be settled once every file has
/// been folded in.
#[derive(Debug, Clone, Copy, Default)]
struct Raw {
    props: Props,
    indent_size: Option<IndentSize>,
    tab_width: Option<u32>,
}

impl Raw {
    fn resolve(self) -> Props {
        let indent_width = match self.indent_size {
            Some(IndentSize::Width(n)) => Some(n),
            Some(IndentSize::Tab) | None => self.tab_width,
        };
        Props {
            indent_width,
            ..self.props
        }
    }
}

/// The filename the spec fixes.
const FILENAME: &str = ".editorconfig";

/// Resolve the properties that apply to `path` by walking from its directory
/// up to the filesystem root, stopping at the first file declaring
/// `root = true`. Nearer files win, which is why the walk collects
/// outermost-first and applies in that order.
pub fn for_file(path: &Path) -> Props {
    let Some(start) = path.parent() else {
        return Props::default();
    };
    // Collect the chain nearest-first, stopping at a `root = true` file.
    let mut chain: Vec<(PathBuf, String)> = Vec::new();
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(FILENAME);
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            let is_root = parse_is_root(&text);
            chain.push((d.to_path_buf(), text));
            if is_root {
                break;
            }
        }
        dir = d.parent();
    }
    // Apply outermost-first so the nearest file's sections land last and win.
    let mut raw = Raw::default();
    for (dir, text) in chain.iter().rev() {
        apply_file(&mut raw, dir, text, path);
    }
    raw.resolve()
}

/// Whether the preamble (everything before the first `[section]`) sets
/// `root = true`.
fn parse_is_root(text: &str) -> bool {
    for line in text.lines() {
        let line = strip_comment(line).trim();
        if line.starts_with('[') {
            break;
        }
        if let Some((k, v)) = split_pair(line)
            && k.eq_ignore_ascii_case("root")
        {
            return v.eq_ignore_ascii_case("true");
        }
    }
    false
}

/// Fold every matching section of one `.editorconfig` into `props`, in file
/// order, so a later section overrides an earlier one (the spec's rule).
fn apply_file(raw: &mut Raw, config_dir: &Path, text: &str, path: &Path) {
    let Some(rel) = relative_slash_path(config_dir, path) else {
        return;
    };
    let mut matching = false;
    for line in text.lines() {
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            matching = section_matches(inner, &rel);
            continue;
        }
        if !matching {
            continue;
        }
        if let Some((k, v)) = split_pair(line) {
            set_prop(raw, &k.to_ascii_lowercase(), v.trim());
        }
    }
}

fn set_prop(raw: &mut Raw, key: &str, value: &str) {
    let props = &mut raw.props;
    // `unset` explicitly clears a property inherited from an outer file.
    if value.eq_ignore_ascii_case("unset") {
        match key {
            "indent_style" => props.use_spaces = None,
            "indent_size" => raw.indent_size = None,
            "tab_width" => raw.tab_width = None,
            "end_of_line" => props.eol = None,
            "trim_trailing_whitespace" => props.trim_trailing_whitespace = None,
            "insert_final_newline" => props.insert_final_newline = None,
            "charset" => props.charset = None,
            _ => {}
        }
        return;
    }
    match key {
        "indent_style" => {
            if value.eq_ignore_ascii_case("space") {
                props.use_spaces = Some(true);
            } else if value.eq_ignore_ascii_case("tab") {
                props.use_spaces = Some(false);
            }
        }
        // `indent_size = tab` defers to `tab_width`; `Raw::resolve` applies
        // the precedence once every file has been read.
        "indent_size" => {
            if value.eq_ignore_ascii_case("tab") {
                raw.indent_size = Some(IndentSize::Tab);
            } else if let Some(n) = parse_width(value) {
                raw.indent_size = Some(IndentSize::Width(n));
            }
        }
        "tab_width" => {
            if let Some(n) = parse_width(value) {
                raw.tab_width = Some(n);
            }
        }
        "end_of_line" => {
            if value.eq_ignore_ascii_case("lf") {
                props.eol = Some(Eol::Lf);
            } else if value.eq_ignore_ascii_case("crlf") {
                props.eol = Some(Eol::Crlf);
            }
            // `cr` (classic Mac) is deliberately unhandled: croft has no
            // CR-only line ending, and guessing LF would silently rewrite
            // every line of the file on the next save.
        }
        "trim_trailing_whitespace" => props.trim_trailing_whitespace = parse_bool(value),
        "insert_final_newline" => props.insert_final_newline = parse_bool(value),
        // A value naming no encoding is ignored like any unknown value.
        "charset" => {
            if let Some(charset) = crate::file_encoding::parse(value) {
                props.charset = Some(charset);
            }
        }
        _ => {}
    }
}

/// A width croft will honour; anything else is ignored like an unknown value.
fn parse_width(value: &str) -> Option<u32> {
    value.parse::<u32>().ok().filter(|n| (1..=16).contains(n))
}

fn parse_bool(value: &str) -> Option<bool> {
    if value.eq_ignore_ascii_case("true") {
        Some(true)
    } else if value.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

/// `;` and `#` start a comment, but only outside a `[section]` header — a
/// glob may legitimately contain `#`.
fn strip_comment(line: &str) -> &str {
    let trimmed = line.trim_start();
    if trimmed.starts_with('[') {
        return line;
    }
    match line.find([';', '#']) {
        Some(i) => &line[..i],
        None => line,
    }
}

fn split_pair(line: &str) -> Option<(&str, &str)> {
    let (k, v) = line.split_once('=')?;
    let k = k.trim();
    (!k.is_empty()).then_some((k, v.trim()))
}

/// `path` relative to `config_dir`, with `/` separators. `None` when the
/// file is not under the config's directory at all.
fn relative_slash_path(config_dir: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(config_dir).ok()?;
    Some(
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Whether a `[section]` glob matches a path relative to its config file.
///
/// Per the spec, a glob with no `/` matches the filename at any depth, so it
/// is anchored with `**/`; one containing a `/` is relative to the config
/// file's directory. A leading `/` just anchors it there explicitly.
fn section_matches(glob: &str, rel: &str) -> bool {
    let anchored = if let Some(stripped) = glob.strip_prefix('/') {
        stripped.to_string()
    } else if glob.contains('/') {
        glob.to_string()
    } else {
        format!("**/{glob}")
    };
    // An expansion past the cap matches nothing rather than allocating it.
    let Some(patterns) = expand_braces(&anchored) else {
        return false;
    };
    patterns.iter().any(|p| {
        glob_match(
            &p.chars().collect::<Vec<_>>(),
            &rel.chars().collect::<Vec<_>>(),
        )
    })
}

/// The most concrete patterns one section glob may expand to. Brace groups
/// multiply (`{a,b}` thirty times is over a billion), and the file comes from
/// whatever repository was opened, so the product is capped before it is
/// built rather than after.
const MAX_BRACE_EXPANSION: usize = 1024;

/// Expand `{a,b}` alternatives into concrete patterns. Nested braces expand
/// too, since the recursion re-runs over each produced alternative. `None`
/// once the expansion would pass [`MAX_BRACE_EXPANSION`].
fn expand_braces(pattern: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = pattern.chars().collect();
    let Some(open) = chars.iter().position(|&c| c == '{') else {
        return Some(vec![pattern.to_string()]);
    };
    // The matching close brace, skipping any nested pairs.
    let mut depth = 0usize;
    let mut close = None;
    for (i, &c) in chars.iter().enumerate().skip(open) {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else {
        return Some(vec![pattern.to_string()]);
    };
    let prefix: String = chars[..open].iter().collect();
    let suffix: String = chars[close + 1..].iter().collect();
    // Split the body on TOP-LEVEL commas only.
    let body = &chars[open + 1..close];
    let mut parts: Vec<String> = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for &c in body {
        match c {
            '{' => {
                depth += 1;
                cur.push(c);
            }
            '}' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    let mut out = Vec::new();
    for p in parts {
        out.extend(expand_braces(&format!("{prefix}{p}{suffix}"))?);
        if out.len() > MAX_BRACE_EXPANSION {
            return None;
        }
    }
    Some(out)
}

/// A glob matcher with EditorConfig's semantics: `*` stops at `/`, `**`
/// crosses it, `?` is one non-`/` character, and `[abc]` / `[!abc]` are
/// character classes.
///
/// Backtracking is memoised on `(pattern index, text index)`: the pattern
/// comes from whatever repository the user opened a file in, and plain
/// backtracking over a run of stars is exponential, which would freeze
/// `open`. With the memo each pair is decided once, so the cost is bounded
/// by `|p| * |t|` states.
fn glob_match(p: &[char], t: &[char]) -> bool {
    let mut memo = vec![None; (p.len() + 1) * (t.len() + 1)];
    Glob {
        p,
        t,
        memo: &mut memo,
    }
    .at(0, 0)
}

struct Glob<'a> {
    p: &'a [char],
    t: &'a [char],
    memo: &'a mut [Option<bool>],
}

impl Glob<'_> {
    fn at(&mut self, pi: usize, ti: usize) -> bool {
        let slot = pi * (self.t.len() + 1) + ti;
        if let Some(hit) = self.memo[slot] {
            return hit;
        }
        let hit = self.step(pi, ti);
        self.memo[slot] = Some(hit);
        hit
    }

    fn step(&mut self, pi: usize, ti: usize) -> bool {
        let (p, t) = (self.p, self.t);
        if pi == p.len() {
            return ti == t.len();
        }
        match p[pi] {
            '*' if p.get(pi + 1) == Some(&'*') => {
                let rest = pi + 2;
                // `**/` must also match zero directories, so `**/x` matches `x`.
                if p.get(rest) == Some(&'/') && self.at(rest + 1, ti) {
                    return true;
                }
                (ti..=t.len()).any(|i| self.at(rest, i))
            }
            '*' => {
                let limit = t[ti..]
                    .iter()
                    .position(|&c| c == '/')
                    .map_or(t.len(), |i| ti + i);
                (ti..=limit).any(|i| self.at(pi + 1, i))
            }
            '?' => ti < t.len() && t[ti] != '/' && self.at(pi + 1, ti + 1),
            '[' => self.class(pi, ti),
            c => ti < t.len() && t[ti] == c && self.at(pi + 1, ti + 1),
        }
    }

    fn class(&mut self, pi: usize, ti: usize) -> bool {
        let (p, t) = (self.p, self.t);
        let negated = p.get(pi + 1) == Some(&'!');
        let start = if negated { pi + 2 } else { pi + 1 };
        let Some(end) = p
            .iter()
            .skip(start)
            .position(|&c| c == ']')
            .map(|i| i + start)
        else {
            // Unterminated `[` is a literal one.
            return ti < t.len() && t[ti] == '[' && self.at(pi + 1, ti + 1);
        };
        if ti == t.len() || t[ti] == '/' {
            return false;
        }
        let c = t[ti];
        let set = &p[start..end];
        let mut hit = false;
        let mut i = 0;
        while i < set.len() {
            if i + 2 < set.len() && set[i + 1] == '-' {
                if c >= set[i] && c <= set[i + 2] {
                    hit = true;
                }
                i += 3;
            } else {
                if c == set[i] {
                    hit = true;
                }
                i += 1;
            }
        }
        if hit == negated {
            return false;
        }
        self.at(end + 1, ti + 1)
    }
}

/// Test seam: resolve against an in-memory set of `(dir, contents)` instead
/// of the filesystem, so the glob and precedence rules can be tested without
/// writing `.editorconfig` files into temp trees.
#[cfg(test)]
fn for_file_with(files: &HashMap<PathBuf, String>, path: &Path) -> Props {
    let Some(start) = path.parent() else {
        return Props::default();
    };
    let mut chain: Vec<(PathBuf, String)> = Vec::new();
    let mut dir = Some(start);
    while let Some(d) = dir {
        if let Some(text) = files.get(&d.join(FILENAME)) {
            let is_root = parse_is_root(text);
            chain.push((d.to_path_buf(), text.clone()));
            if is_root {
                break;
            }
        }
        dir = d.parent();
    }
    let mut raw = Raw::default();
    for (dir, text) in chain.iter().rev() {
        apply_file(&mut raw, dir, text, path);
    }
    raw.resolve()
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    fn files(entries: &[(&str, &str)]) -> HashMap<PathBuf, String> {
        entries
            .iter()
            .map(|(p, c)| (PathBuf::from(p), (*c).to_string()))
            .collect()
    }

    #[test]
    fn indent_style_and_size_are_read() {
        let f = files(&[(
            "/w/.editorconfig",
            "root = true\n\n[*]\nindent_style = space\nindent_size = 2\n",
        )]);
        let p = for_file_with(&f, Path::new("/w/src/a.rs"));
        assert_eq!(p.use_spaces, Some(true));
        assert_eq!(p.indent_width, Some(2));
    }

    #[test]
    fn a_later_matching_section_overrides_an_earlier_one() {
        let f = files(&[(
            "/w/.editorconfig",
            "root = true\n[*]\nindent_size = 4\n[*.rs]\nindent_size = 8\n",
        )]);
        assert_eq!(
            for_file_with(&f, Path::new("/w/a.rs")).indent_width,
            Some(8)
        );
        assert_eq!(
            for_file_with(&f, Path::new("/w/a.py")).indent_width,
            Some(4)
        );
    }

    #[test]
    fn a_nearer_config_wins_over_an_outer_one() {
        let f = files(&[
            ("/w/.editorconfig", "root = true\n[*]\nindent_size = 4\n"),
            ("/w/vendor/.editorconfig", "[*]\nindent_size = 2\n"),
        ]);
        assert_eq!(
            for_file_with(&f, Path::new("/w/a.rs")).indent_width,
            Some(4)
        );
        assert_eq!(
            for_file_with(&f, Path::new("/w/vendor/a.rs")).indent_width,
            Some(2),
            "the nearer file wins"
        );
    }

    #[test]
    fn root_true_stops_the_upward_walk() {
        let f = files(&[
            ("/w/.editorconfig", "[*]\ninsert_final_newline = true\n"),
            (
                "/w/sub/.editorconfig",
                "root = true\n[*]\nindent_size = 2\n",
            ),
        ]);
        let p = for_file_with(&f, Path::new("/w/sub/a.rs"));
        assert_eq!(p.indent_width, Some(2));
        assert_eq!(
            p.insert_final_newline, None,
            "the outer file is never read past a root = true"
        );
    }

    #[test]
    fn tab_style_and_tab_width_resolve_together() {
        let f = files(&[(
            "/w/.editorconfig",
            "root = true\n[*]\nindent_style = tab\nindent_size = tab\ntab_width = 8\n",
        )]);
        let p = for_file_with(&f, Path::new("/w/Makefile"));
        assert_eq!(p.use_spaces, Some(false));
        assert_eq!(
            p.indent_width,
            Some(8),
            "indent_size = tab defers to tab_width"
        );
    }

    /// `tab_width` only stands in for an `indent_size` that is absent or
    /// `tab`; a numeric `indent_size` wins wherever `tab_width` appears.
    #[test]
    fn a_numeric_indent_size_beats_tab_width_in_either_order() {
        for body in [
            "[*]\nindent_style = space\nindent_size = 2\ntab_width = 8\n",
            "[*]\nindent_style = space\ntab_width = 8\nindent_size = 2\n",
        ] {
            let f = files(&[("/w/.editorconfig", &format!("root = true\n{body}"))]);
            assert_eq!(
                for_file_with(&f, Path::new("/w/a.rs")).indent_width,
                Some(2),
                "{body:?}"
            );
        }
    }

    /// The precedence holds across files too: a nearer file naming only
    /// `tab_width` does not override an outer file's numeric `indent_size`.
    #[test]
    fn a_nearer_tab_width_does_not_override_an_outer_indent_size() {
        let f = files(&[
            ("/w/.editorconfig", "root = true\n[*]\nindent_size = 2\n"),
            ("/w/sub/.editorconfig", "[*]\ntab_width = 8\n"),
        ]);
        assert_eq!(
            for_file_with(&f, Path::new("/w/sub/a.rs")).indent_width,
            Some(2)
        );
    }

    /// `tab_width = unset` clears `tab_width` alone, not `indent_size`.
    #[test]
    fn unsetting_tab_width_keeps_indent_size() {
        let f = files(&[(
            "/w/.editorconfig",
            "root = true\n[*]\nindent_size = 2\ntab_width = 8\n[*.rs]\ntab_width = unset\n",
        )]);
        assert_eq!(
            for_file_with(&f, Path::new("/w/a.rs")).indent_width,
            Some(2)
        );
    }

    /// Backtracking without memoisation is exponential in the number of
    /// stars, and the pattern comes from whatever repo the user opened a
    /// file in. This one took minutes before; it must be instant.
    #[test]
    fn a_many_star_glob_does_not_backtrack_exponentially() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let name = format!("{}.rs", "a".repeat(40));
            let _ = tx.send(section_matches("*a*a*a*a*a*a*a*a*a*a*a*b", &name));
        });
        let hit = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("glob matching must not take seconds");
        assert!(!hit, "no `b` in the name, so no match");
        assert!(
            section_matches("*a*a*a*b.rs", "xaayaazab.rs"),
            "the same shape still matches when it should"
        );
    }

    #[test]
    fn eol_and_the_on_save_flags_are_read() {
        let f = files(&[(
            "/w/.editorconfig",
            "root = true\n[*]\nend_of_line = crlf\ntrim_trailing_whitespace = true\ninsert_final_newline = false\n",
        )]);
        let p = for_file_with(&f, Path::new("/w/a.txt"));
        assert_eq!(p.eol, Some(Eol::Crlf));
        assert_eq!(p.trim_trailing_whitespace, Some(true));
        assert_eq!(p.insert_final_newline, Some(false));
    }

    #[test]
    fn unset_clears_an_inherited_property() {
        let f = files(&[
            ("/w/.editorconfig", "root = true\n[*]\nindent_size = 4\n"),
            ("/w/gen/.editorconfig", "[*]\nindent_size = unset\n"),
        ]);
        assert_eq!(
            for_file_with(&f, Path::new("/w/gen/a.rs")).indent_width,
            None
        );
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let f = files(&[(
            "/w/.editorconfig",
            "# a comment\nroot = true\n\n[*]\n; another\nindent_size = 3  # trailing\n",
        )]);
        assert_eq!(
            for_file_with(&f, Path::new("/w/a.rs")).indent_width,
            Some(3)
        );
    }

    #[test]
    fn unknown_properties_are_ignored_not_fatal() {
        let f = files(&[(
            "/w/.editorconfig",
            "root = true\n[*]\nmax_line_length = 100\nquux = 7\nindent_size = 2\n",
        )]);
        assert_eq!(
            for_file_with(&f, Path::new("/w/a.rs")).indent_width,
            Some(2)
        );
    }

    #[test]
    fn a_file_with_nothing_to_say_resolves_to_empty() {
        let f = files(&[("/w/.editorconfig", "root = true\n[*.py]\nindent_size = 2\n")]);
        assert!(for_file_with(&f, Path::new("/w/a.rs")).is_empty());
    }

    #[test]
    fn brace_alternatives_match() {
        assert!(section_matches("*.{js,ts,tsx}", "src/a.ts"));
        assert!(section_matches("*.{js,ts,tsx}", "a.tsx"));
        assert!(!section_matches("*.{js,ts,tsx}", "a.rs"));
    }

    /// Brace groups multiply: `{a,b}` thirty times is over a billion
    /// patterns, built before any matching starts, from a file any cloned
    /// repository can ship. Past a fixed cap the section matches nothing.
    #[test]
    fn a_brace_expansion_past_the_cap_matches_nothing() {
        // 12 groups = 4096 alternatives, over the cap; the name would match.
        let glob = format!("{}x", "{a,b}".repeat(12));
        let name = format!("{}x", "a".repeat(12));
        assert!(
            !section_matches(&glob, &name),
            "an oversized expansion is refused"
        );
        // Under the cap it still matches, so the refusal is the cap and not
        // a pattern that never matched.
        let small = format!("{}x", "{a,b}".repeat(4));
        assert!(section_matches(&small, "aaaax"));
    }

    #[test]
    fn a_bare_glob_matches_at_any_depth_but_a_slashed_one_is_anchored() {
        assert!(section_matches("*.rs", "deep/nested/a.rs"));
        assert!(section_matches("src/*.rs", "src/a.rs"));
        assert!(
            !section_matches("src/*.rs", "other/src/a.rs"),
            "a glob containing / is relative to the config's own directory"
        );
    }

    #[test]
    fn single_star_stops_at_a_slash_but_double_star_crosses_it() {
        assert!(!section_matches("src/*.rs", "src/deep/a.rs"));
        assert!(section_matches("src/**.rs", "src/deep/a.rs"));
        assert!(section_matches("src/**/*.rs", "src/deep/a.rs"));
        assert!(
            section_matches("src/**/*.rs", "src/a.rs"),
            "**/ must also match zero directories"
        );
    }

    #[test]
    fn question_marks_and_character_classes_match() {
        assert!(section_matches("a?.rs", "a1.rs"));
        assert!(!section_matches("a?.rs", "a12.rs"));
        assert!(section_matches("[abc].rs", "b.rs"));
        assert!(!section_matches("[abc].rs", "d.rs"));
        assert!(section_matches("[!abc].rs", "d.rs"));
        assert!(section_matches("[a-z].rs", "q.rs"));
        assert!(!section_matches("[a-z].rs", "Q.rs"));
    }

    #[test]
    fn a_leading_slash_anchors_to_the_config_directory() {
        assert!(section_matches("/a.rs", "a.rs"));
        assert!(!section_matches("/a.rs", "sub/a.rs"));
    }

    #[test]
    fn for_file_reads_real_files_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(".editorconfig"),
            "root = true\n[*.rs]\nindent_style = space\nindent_size = 2\n",
        )
        .unwrap();
        let sub = tmp.path().join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let p = for_file(&sub.join("main.rs"));
        assert_eq!(p.use_spaces, Some(true));
        assert_eq!(p.indent_width, Some(2));
    }

    /// #1432: `charset` names the encoding; `utf-8-bom` asks for the mark,
    /// a nearer `unset` clears it and an unknown value leaves it alone.
    #[test]
    fn charset_is_read_and_unset_and_bad_values_are_ignored() {
        let f = files(&[
            (
                "/w/.editorconfig",
                "root = true\n[*]\ncharset = latin1\n[*.cs]\ncharset = utf-8-bom\n[*.md]\ncharset = klingon\n",
            ),
            ("/w/gen/.editorconfig", "[*]\ncharset = unset\n"),
        ]);
        let name = |p: &str| {
            for_file_with(&f, Path::new(p))
                .charset
                .map(|c| (c.encoding.name(), c.bom))
        };
        assert_eq!(name("/w/a.txt"), Some(("windows-1252", false)));
        assert_eq!(name("/w/a.cs"), Some(("UTF-8", true)));
        assert_eq!(name("/w/a.md"), Some(("windows-1252", false)));
        assert_eq!(name("/w/gen/a.txt"), None);
    }

    #[test]
    fn no_editorconfig_anywhere_resolves_to_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(for_file(&tmp.path().join("a.rs")).is_empty());
    }
}
