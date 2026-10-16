//! User snippets, loaded from `~/.config/croft/snippets.json`.
//!
//! Format mirrors VS Code's global snippets file: a JSON object keyed by a
//! human name, each value carrying a `prefix`, a `body` (string or array of
//! lines), an optional `description`, and an optional comma-separated `scope`
//! of language ids the snippet applies to (empty = every language).
//!
//! ```json
//! {
//!   "Print to console": {
//!     "prefix": "log",
//!     "body": "console.log($1)$0",
//!     "scope": "javascript,typescript"
//!   }
//! }
//! ```
//!
//! The body is a VS Code snippet string: `$1`, `$2`, … are tab stops, `$0` the
//! final caret, `${1:name}` a placeholder. [`parse_body`] turns it into the
//! literal text to insert plus the ordered stop positions the editor drives.
//!
//! ponytail: supported tab-stop syntax is `$N`, `${N}`, nested
//! `${N:placeholder}`, choices `${N|a,b|}` (the first option is inserted) and
//! the `\$` / `\}` / `\\` escapes. Not supported (name the ceiling, add on
//! demand): variables (`$TM_FILENAME`), transforms, and live mirroring of a
//! repeated `$N` (the placeholder text is duplicated, but only the first
//! occurrence is a navigable stop).

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One resolved tab stop within an expanded snippet body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabStop {
    /// The stop number (`$1` -> 1, `$0` -> 0).
    pub num: u32,
    /// Char offset of the stop within the resolved text.
    pub offset: usize,
    /// Length in chars of the placeholder selected on landing (0 for a bare stop).
    pub len: usize,
}

/// The literal text an expanded snippet inserts, plus its ordered tab stops
/// (ascending by number, with `$0` last).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedBody {
    pub text: String,
    pub stops: Vec<TabStop>,
}

/// Parse a VS Code snippet body into the text to insert and its tab stops.
///
/// Language servers send the same syntax for snippet completions, so it
/// follows the LSP grammar: placeholders nest (`${1:foo(${2:x})}`), a choice
/// inserts its first option (`${1|one,two|}`), and only `\$`, `\}` and `\\`
/// are escapes - any other backslash is text, as in `printf("%d\n", $1)`.
pub fn parse_body(body: &str) -> ParsedBody {
    let mut p = Parser {
        text: String::new(),
        pos: 0,
        stops: Vec::new(),
        seen: Vec::new(),
    };
    let mut chars = body.chars().peekable();
    p.seq(&mut chars, false);
    let mut stops = p.stops;
    // Ascending by number, but `$0` (the final caret) always sorts last.
    stops.sort_by_key(|s| if s.num == 0 { u32::MAX } else { s.num });
    ParsedBody {
        text: p.text,
        stops,
    }
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

struct Parser {
    text: String,
    /// Char offset into `text`.
    pos: usize,
    stops: Vec<TabStop>,
    seen: Vec<u32>,
}

impl Parser {
    fn push(&mut self, c: char) {
        self.text.push(c);
        self.pos += 1;
    }

    /// A stop at `offset` spanning `len` chars. A repeated `$N` duplicates
    /// its text, but only the first occurrence is a navigable stop.
    fn stop(&mut self, num: u32, offset: usize, len: usize) {
        if !self.seen.contains(&num) {
            self.seen.push(num);
            self.stops.push(TabStop { num, offset, len });
        }
    }

    /// Text up to the end, or inside a placeholder up to its closing `}`
    /// (consumed).
    fn seq(&mut self, chars: &mut Chars, in_placeholder: bool) {
        while let Some(c) = chars.next() {
            match c {
                '\\' => match chars.peek() {
                    Some(&n) if matches!(n, '$' | '}' | '\\') => {
                        chars.next();
                        self.push(n);
                    }
                    _ => self.push('\\'),
                },
                '}' if in_placeholder => return,
                '$' => self.dollar(chars),
                _ => self.push(c),
            }
        }
    }

    /// What follows a `$`: a stop, a placeholder, a choice, or a literal `$`.
    fn dollar(&mut self, chars: &mut Chars) {
        let mut look = chars.clone();
        let braced = look.peek() == Some(&'{');
        if braced {
            look.next();
        }
        let mut digits = String::new();
        while let Some(&d) = look.peek().filter(|d| d.is_ascii_digit()) {
            digits.push(d);
            look.next();
        }
        let Ok(num) = digits.parse::<u32>() else {
            self.push('$');
            return;
        };
        if !braced {
            *chars = look;
            self.stop(num, self.pos, 0);
            return;
        }
        match look.next() {
            Some('}') => {
                *chars = look;
                self.stop(num, self.pos, 0);
            }
            Some(':') => {
                *chars = look;
                let start = self.pos;
                self.seq(chars, true);
                self.stop(num, start, self.pos - start);
            }
            Some('|') => {
                let Some(first) = choice_first(&mut look) else {
                    self.push('$');
                    return;
                };
                *chars = look;
                let start = self.pos;
                for c in first.chars() {
                    self.push(c);
                }
                self.stop(num, start, self.pos - start);
            }
            // Malformed `${N…`: not a stop, the `$` is literal.
            _ => self.push('$'),
        }
    }
}

/// The first option of a choice, reading past its closing `|}`. `\,`,
/// `\|` and `\\` escape inside it. `None` when the choice is unterminated.
fn choice_first(chars: &mut Chars) -> Option<String> {
    let mut first = String::new();
    let mut in_first = true;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let n = match chars.peek() {
                    Some(&n) if matches!(n, ',' | '|' | '\\') => {
                        chars.next();
                        n
                    }
                    _ => '\\',
                };
                if in_first {
                    first.push(n);
                }
            }
            ',' => in_first = false,
            '|' if chars.peek() == Some(&'}') => {
                chars.next();
                return Some(first);
            }
            _ if in_first => first.push(c),
            _ => {}
        }
    }
    None
}

/// Convert a char offset within a block of text to a (line delta, column)
/// relative to the block's start. Column is chars since the last newline.
pub fn offset_to_line_col(text: &str, offset: usize) -> (usize, usize) {
    let mut line = 0usize;
    let mut col = 0usize;
    for (i, c) in text.chars().enumerate() {
        if i == offset {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// One user snippet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub prefix: String,
    pub body: String,
    pub scope: Vec<String>,
}

impl Snippet {
    /// Whether this snippet applies to `language` (its scope is empty or names it).
    ///
    /// A `.jsx` file is `javascriptreact` and a `.jsonc` file `jsonc`, as in
    /// VS Code (#1641), so their own snippets apply there. Snippets scoped to
    /// the base language still apply too: before, croft called those files
    /// `javascript` and `json`, and a user's existing snippets keep working.
    fn applies_to(&self, language: &str) -> bool {
        let base = match language {
            "javascriptreact" => "javascript",
            "jsonc" => "json",
            other => other,
        };
        self.scope.is_empty() || self.scope.iter().any(|s| s == language || s == base)
    }
}

/// The loaded set of user snippets.
#[derive(Debug, Clone, Default)]
pub struct SnippetSet {
    snippets: Vec<Snippet>,
    /// What could not be loaded: the file itself, or one entry by name. The
    /// rest still loads (#1483).
    warnings: Vec<String>,
    /// The file did not parse at all, so nothing was loaded from it (#1191).
    broken: bool,
}

/// The on-disk value shape: `prefix` is a string or an array of them (VS Code
/// allows both); `body` is a string or an array of lines; `scope` is an
/// optional comma-separated language list.
#[derive(Debug, Deserialize)]
struct RawSnippet {
    #[serde(default)]
    prefix: Prefix,
    #[serde(default)]
    body: Body,
    #[serde(default)]
    scope: Option<String>,
}

/// A snippet's trigger words. VS Code lets one snippet answer to several
/// (`"prefix": ["for", "fori"]`), and to none when it is only meant for
/// Insert Snippet.
#[derive(Debug, Deserialize, Default)]
#[serde(untagged)]
enum Prefix {
    #[default]
    Missing,
    One(String),
    Many(Vec<String>),
}

impl Prefix {
    fn into_vec(self) -> Vec<String> {
        match self {
            Prefix::Missing => Vec::new(),
            Prefix::One(p) => vec![p],
            Prefix::Many(ps) => ps,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(untagged)]
enum Body {
    #[default]
    Missing,
    One(String),
    Many(Vec<String>),
}

impl Body {
    fn joined(&self) -> String {
        match self {
            Body::Missing => String::new(),
            Body::One(s) => s.clone(),
            Body::Many(lines) => lines.join("\n"),
        }
    }
}

impl SnippetSet {
    pub fn load(path: &Path) -> Self {
        let Ok(json) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        Self::from_json(&json)
    }

    pub fn from_json(json: &str) -> Self {
        Self::parse(json).0
    }

    /// The snippets in `json`, read as JSONC like VS Code's snippet files
    /// and croft's settings (comments and trailing commas, #1191), entry by
    /// entry, so one snippet croft cannot read is skipped and named in
    /// [`warnings`](Self::warnings) instead of taking every other snippet
    /// with it (#1483). The second half is why none were loaded when the
    /// file does not parse at all; it is in the warnings too.
    pub fn parse(json: &str) -> (Self, Option<String>) {
        let stripped = crate::tasks::strip_jsonc(json);
        if stripped.trim().is_empty() {
            return (Self::default(), None);
        }
        let entries: serde_json::Map<String, serde_json::Value> = match serde_json::from_str(
            &stripped,
        ) {
            Ok(entries) => entries,
            Err(e) => {
                let why = format!(
                    "snippets.json could not be read: the file does not parse ({e}); NO snippets were loaded from it"
                );
                let set = Self {
                    snippets: Vec::new(),
                    warnings: vec![why.clone()],
                    broken: true,
                };
                return (set, Some(why));
            }
        };
        let mut snippets = Vec::new();
        let mut warnings = Vec::new();
        for (name, value) in entries {
            let raw: RawSnippet = match serde_json::from_value(value) {
                Ok(raw) => raw,
                Err(e) => {
                    warnings.push(format!("snippet {name:?} skipped: {e}"));
                    continue;
                }
            };
            let body = raw.body.joined();
            let scope: Vec<String> = raw
                .scope
                .map(|s| {
                    s.split(',')
                        .map(|p| p.trim().to_string())
                        .filter(|p| !p.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            for prefix in raw.prefix.into_vec() {
                if prefix.is_empty() {
                    continue;
                }
                snippets.push(Snippet {
                    prefix,
                    body: body.clone(),
                    scope: scope.clone(),
                });
            }
        }
        let set = Self {
            snippets,
            warnings,
            broken: false,
        };
        (set, None)
    }

    pub fn is_empty(&self) -> bool {
        self.snippets.is_empty()
    }

    /// What could not be loaded, one line each, for OUTPUT · Snippets.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Whether the file did not parse at all, so nothing was loaded (#1191).
    pub fn is_broken(&self) -> bool {
        self.broken
    }

    /// Snippets in `language` whose prefix begins with `word` (case-sensitive,
    /// like VS Code); for injecting into the completion popup. An empty `word`
    /// still filters by language but matches every prefix.
    pub fn matching(&self, word: &str, language: &str) -> Vec<&Snippet> {
        self.snippets
            .iter()
            .filter(|s| s.applies_to(language) && s.prefix.starts_with(word))
            .collect()
    }

    /// The single snippet whose prefix exactly equals `word` in `language`, if
    /// any — the Tab-to-expand lookup.
    pub fn exact(&self, word: &str, language: &str) -> Option<&Snippet> {
        self.snippets
            .iter()
            .find(|s| s.prefix == word && s.applies_to(language))
    }
}

pub fn snippets_path() -> PathBuf {
    crate::profiles::file("snippets.json")
}

/// Seeded on first "Configure User Snippets" so the user starts from a working
/// example rather than a blank buffer.
pub const TEMPLATE: &str = r#"// croft user snippets. Keyed by name; each has a "prefix" (or an array of
// them) you type then expand with Tab, a "body" (string or array of lines), and an optional "scope"
// (comma-separated language ids; omit for all languages).
// Tab stops: $1, $2 … in order, $0 final caret, ${1:placeholder} with default text.
{
  "Print to console": {
    "prefix": "log",
    "body": "console.log($1)$0",
    "scope": "javascript,typescript"
  },
  "Python main guard": {
    "prefix": "main",
    "body": [
      "if __name__ == \"__main__\":",
      "    ${1:main()}$0"
    ],
    "scope": "python"
  }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// #1641: `javascriptreact` and `jsonc` snippets apply to their own
    /// language; snippets scoped to the base language still apply there,
    /// and not the other way round.
    #[test]
    fn react_and_jsonc_scopes_take_their_base_language_too() {
        let set = SnippetSet::from_json(
            r#"{
              "rfc": { "prefix": "rfc", "body": "x", "scope": "javascriptreact" },
              "clg": { "prefix": "clg", "body": "y", "scope": "javascript" },
              "key": { "prefix": "key", "body": "z", "scope": "json" }
            }"#,
        );
        assert!(set.exact("rfc", "javascriptreact").is_some());
        assert!(set.exact("clg", "javascriptreact").is_some());
        assert!(set.exact("key", "jsonc").is_some());
        assert!(set.exact("rfc", "javascript").is_none());
        assert!(set.exact("clg", "typescriptreact").is_none());
    }

    #[test]
    fn snippets_with_a_trailing_comma_or_block_comment_still_load() {
        // #1191: VS Code's snippet files are JSONC, and a pasted one has both.
        let set = SnippetSet::from_json(
            "{\n  /* logging */\n  \"log\": { \"prefix\": \"log\", \"body\": \"console.log($1);\" },\n  \"todo\": { \"prefix\": \"todo\", \"body\": [\"// TODO: $1\",], },\n}\n",
        );
        assert_eq!(set.matching("log", "javascript").len(), 1);
        let todo = set.matching("todo", "javascript");
        assert_eq!(todo.len(), 1);
        assert_eq!(
            todo[0].body, "// TODO: $1",
            "a comment marker inside a string stays"
        );
    }

    #[test]
    fn snippet_bodies_keep_what_only_looks_like_jsonc() {
        // Negative: commas before a brace and comment markers inside strings
        // are the snippet's text, not JSONC to strip.
        let set =
            SnippetSet::from_json(r#"{"c": {"prefix": "cm", "body": "/* $1 */ f(a, }) // end"}}"#);
        assert_eq!(set.matching("cm", "c")[0].body, "/* $1 */ f(a, }) // end");
    }

    #[test]
    fn a_snippets_file_that_does_not_parse_says_so() {
        // A file that is broken (not merely JSONC) loads nothing, as before,
        // but no longer silently: the reason comes back for OUTPUT.
        let (set, warning) = SnippetSet::parse(r#"{"log": {"prefix": "log" "body": "x"}}"#);
        assert!(set.matching("log", "javascript").is_empty());
        let warning = warning.expect("a warning");
        assert!(warning.contains("does not parse"), "{warning}");
        let (_, none) = SnippetSet::parse("{}");
        assert_eq!(none, None, "an empty file is fine");
    }

    #[test]
    fn lsp_snippets_keep_backslashes_nest_placeholders_and_pick_a_choice() {
        let p = parse_body(r#"printf("%d\n", $1)"#);
        assert_eq!(p.text, r#"printf("%d\n", )"#);
        let p = parse_body("${1:foo(${2:x})}$0");
        assert_eq!(p.text, "foo(x)");
        assert_eq!(
            p.stops[0],
            TabStop {
                num: 1,
                offset: 0,
                len: 6
            }
        );
        assert_eq!(
            p.stops[1],
            TabStop {
                num: 2,
                offset: 4,
                len: 1
            }
        );
        let p = parse_body(r"${1|one,two|} \$5 \} a\b");
        assert_eq!(p.text, "one $5 } a\\b");
        assert_eq!(
            p.stops[0],
            TabStop {
                num: 1,
                offset: 0,
                len: 3
            }
        );
    }

    #[test]
    fn parses_single_bare_stop() {
        let p = parse_body("console.log($1)");
        assert_eq!(p.text, "console.log()");
        assert_eq!(
            p.stops,
            vec![TabStop {
                num: 1,
                offset: 12,
                len: 0
            }]
        );
    }

    #[test]
    fn placeholder_text_is_inserted_and_measured() {
        let p = parse_body("for ${1:i} in ${2:range}:");
        assert_eq!(p.text, "for i in range:");
        assert_eq!(
            p.stops,
            vec![
                TabStop {
                    num: 1,
                    offset: 4,
                    len: 1
                },
                TabStop {
                    num: 2,
                    offset: 9,
                    len: 5
                },
            ]
        );
    }

    #[test]
    fn final_zero_stop_sorts_last_regardless_of_position() {
        let p = parse_body("try:\n    $0\nexcept $1:");
        let nums: Vec<u32> = p.stops.iter().map(|s| s.num).collect();
        assert_eq!(nums, vec![1, 0], "$0 must be visited last");
    }

    #[test]
    fn repeated_number_duplicates_text_but_only_first_is_a_stop() {
        let p = parse_body("$1 = $1 + 1");
        assert_eq!(p.text, " =  + 1");
        assert_eq!(p.stops.len(), 1, "mirror occurrences are not extra stops");
        assert_eq!(p.stops[0].offset, 0);
    }

    #[test]
    fn escaped_dollar_is_literal() {
        let p = parse_body("cost is \\$5 for $1");
        assert_eq!(p.text, "cost is $5 for ");
        assert_eq!(p.stops.len(), 1);
    }

    #[test]
    fn dollar_before_non_stop_is_literal() {
        let p = parse_body("$foo and ${bar}");
        assert_eq!(p.text, "$foo and ${bar}");
        assert!(p.stops.is_empty());
    }

    #[test]
    fn offset_to_line_col_spans_newlines() {
        assert_eq!(offset_to_line_col("ab\ncd", 0), (0, 0));
        assert_eq!(offset_to_line_col("ab\ncd", 2), (0, 2));
        assert_eq!(offset_to_line_col("ab\ncd", 4), (1, 1));
    }

    #[test]
    fn loads_string_and_array_bodies_with_scope() {
        let json = r#"{
            "Log": { "prefix": "log", "body": "console.log($1)", "scope": "javascript, typescript" },
            "Guard": { "prefix": "main", "body": ["line1", "line2"] }
        }"#;
        let set = SnippetSet::from_json(json);
        // Scope with a space after the comma still matches typescript.
        assert!(set.exact("log", "typescript").is_some());
        assert!(set.exact("log", "python").is_none());
        // Array body joined with newlines; no scope -> every language.
        let guard = set
            .exact("main", "python")
            .expect("no-scope snippet is universal");
        assert_eq!(guard.body, "line1\nline2");
    }

    #[test]
    fn scope_filters_by_language() {
        let json = r#"{ "Log": { "prefix": "log", "body": "x", "scope": "javascript" } }"#;
        let set = SnippetSet::from_json(json);
        assert!(set.exact("log", "python").is_none());
        assert!(set.exact("log", "javascript").is_some());
    }

    #[test]
    fn matching_filters_by_prefix_and_scope_for_the_popup() {
        let json = r#"{
            "Log": { "prefix": "log", "body": "x", "scope": "javascript" },
            "Loop": { "prefix": "loop", "body": "y" }
        }"#;
        let set = SnippetSet::from_json(json);
        // Prefix "lo" matches both in JS (Loop has no scope -> universal).
        let js: Vec<&str> = set
            .matching("lo", "javascript")
            .iter()
            .map(|s| s.prefix.as_str())
            .collect();
        assert_eq!(js.len(), 2);
        // In Python only the unscoped "loop" survives.
        let py: Vec<&str> = set
            .matching("lo", "python")
            .iter()
            .map(|s| s.prefix.as_str())
            .collect();
        assert_eq!(py, vec!["loop"]);
    }

    #[test]
    fn exact_requires_full_prefix_match() {
        let json = r#"{ "Log": { "prefix": "log", "body": "x" } }"#;
        let set = SnippetSet::from_json(json);
        assert!(set.exact("lo", "javascript").is_none());
        assert!(set.exact("log", "javascript").is_some());
    }

    #[test]
    fn template_is_valid_and_expands() {
        let set = SnippetSet::from_json(TEMPLATE);
        assert!(!set.is_empty());
        let log = set
            .exact("log", "javascript")
            .expect("template log snippet");
        let parsed = parse_body(&log.body);
        assert_eq!(parsed.text, "console.log()");
    }

    #[test]
    fn junk_json_is_empty_not_fatal() {
        assert!(SnippetSet::from_json("not json").is_empty());
        assert!(SnippetSet::from_json("").is_empty());
    }

    #[test]
    fn an_array_prefix_expands_from_each_of_its_words() {
        let json = r#"{
            "For loop": {
                "prefix": ["for", "fori"],
                "body": ["for ${1:i} in range(${2:10}):", "    $0"],
                "scope": "python"
            }
        }"#;
        let set = SnippetSet::from_json(json);
        for word in ["for", "fori"] {
            let s = set
                .exact(word, "python")
                .unwrap_or_else(|| panic!("{word} must expand"));
            assert_eq!(s.body, "for ${1:i} in range(${2:10}):\n    $0");
        }
        assert!(set.exact("fori", "rust").is_none(), "scope still applies");
        assert!(set.warnings().is_empty());
    }

    #[test]
    fn an_array_prefix_does_not_hide_the_other_snippets() {
        let json = format!(
            "{}, \"For loop\": {{ \"prefix\": [\"for\", \"fori\"], \"body\": \"x\" }} }}",
            TEMPLATE.trim_end().trim_end_matches('}').trim_end()
        );
        let set = SnippetSet::from_json(&json);
        assert!(
            set.exact("log", "javascript").is_some(),
            "template snippets stay"
        );
        assert!(set.exact("fori", "python").is_some());
    }

    #[test]
    fn a_malformed_entry_is_skipped_and_named_while_the_rest_load() {
        let json = r#"{
            "Log": { "prefix": "log", "body": "console.log($1)" },
            "Broken": { "prefix": 42, "body": "x" },
            "Main": { "prefix": "main", "body": "if __name__ == '__main__':" }
        }"#;
        let set = SnippetSet::from_json(json);
        assert!(set.exact("log", "javascript").is_some());
        assert!(set.exact("main", "python").is_some());
        assert_eq!(set.warnings().len(), 1, "{:?}", set.warnings());
        assert!(
            set.warnings()[0].contains("\"Broken\""),
            "{:?}",
            set.warnings()
        );
    }

    #[test]
    fn an_entry_with_no_prefix_is_not_an_error() {
        // Legal in VS Code: such a snippet is only reachable from Insert Snippet.
        let json = r##"{
            "Header": { "body": "# header" },
            "Log": { "prefix": "log", "body": "x" }
        }"##;
        let set = SnippetSet::from_json(json);
        assert!(set.exact("log", "javascript").is_some());
        assert!(set.warnings().is_empty(), "{:?}", set.warnings());
    }

    #[test]
    fn an_unreadable_file_reports_an_error_instead_of_loading_nothing_silently() {
        let set = SnippetSet::from_json("{ \"Log\": { \"prefix\": \"log\", ");
        assert!(set.is_empty());
        assert_eq!(set.warnings().len(), 1);
        assert!(set.warnings()[0].starts_with("snippets.json could not be read"));
    }

    /// Negative: an empty or comment-only file is not a mistake.
    #[test]
    fn an_empty_file_has_no_warnings() {
        assert!(SnippetSet::from_json("").warnings().is_empty());
        assert!(
            SnippetSet::from_json("// nothing yet\n")
                .warnings()
                .is_empty()
        );
        assert!(SnippetSet::from_json("{}").warnings().is_empty());
    }
}
