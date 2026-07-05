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
//! ponytail: supported tab-stop syntax is `$N`, `${N}`, `${N:placeholder}` and
//! the `\$` / `\\` escapes. Not supported (name the ceiling, add on demand):
//! choices `${1|a,b|}`, variables (`$TM_FILENAME`), transforms, and live
//! mirroring of a repeated `$N` (the placeholder text is duplicated, but only
//! the first occurrence is a navigable stop).

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
pub fn parse_body(body: &str) -> ParsedBody {
    let mut text = String::new();
    let mut stops: Vec<TabStop> = Vec::new();
    let mut seen: Vec<u32> = Vec::new();
    let mut pos = 0usize; // char offset into `text`
    let mut chars = body.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                // `\$` and `\\` escape the next char; a trailing lone `\` is literal.
                if let Some(&next) = chars.peek() {
                    chars.next();
                    text.push(next);
                    pos += 1;
                } else {
                    text.push('\\');
                    pos += 1;
                }
            }
            '$' => {
                // Look ahead on a clone so a `$` that turns out not to introduce
                // a tab stop (e.g. `$foo`, `${bar}`) leaves the real iterator
                // untouched and the `$` is emitted literally.
                let mut look = chars.clone();
                if let Some((num, placeholder)) = try_parse_stop(&mut look) {
                    chars = look;
                    let ph_len = placeholder.chars().count();
                    // Mirror dedup: the placeholder text is duplicated at every
                    // occurrence, but only the first `$N` is a navigable stop.
                    if !seen.contains(&num) {
                        seen.push(num);
                        stops.push(TabStop {
                            num,
                            offset: pos,
                            len: ph_len,
                        });
                    }
                    text.push_str(&placeholder);
                    pos += ph_len;
                } else {
                    text.push('$');
                    pos += 1;
                }
            }
            _ => {
                text.push(c);
                pos += 1;
            }
        }
    }

    // Ascending by number, but `$0` (the final caret) always sorts last.
    stops.sort_by_key(|s| if s.num == 0 { u32::MAX } else { s.num });
    ParsedBody { text, stops }
}

/// Parse a tab stop starting just after a `$`, from a *lookahead* iterator the
/// caller commits only on success: `N`, `{N}`, or `{N:placeholder}`. Returns
/// `(number, placeholder_text)`, or `None` when what follows is not a stop (so
/// the `$` is literal and nothing is consumed).
fn try_parse_stop(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<(u32, String)> {
    let braced = chars.peek() == Some(&'{');
    if braced {
        chars.next(); // consume '{'
    }
    let mut digits = String::new();
    while let Some(&d) = chars.peek() {
        if d.is_ascii_digit() {
            digits.push(d);
            chars.next();
        } else {
            break;
        }
    }
    let num: u32 = digits.parse().ok()?; // empty digits -> None

    let mut placeholder = String::new();
    if braced {
        match chars.peek() {
            Some(&':') => {
                chars.next(); // consume ':'
                // Read to the closing '}' (no nesting, per the ceiling).
                while let Some(&pc) = chars.peek() {
                    chars.next();
                    if pc == '}' {
                        break;
                    }
                    placeholder.push(pc);
                }
            }
            Some(&'}') => {
                chars.next(); // bare `${N}`
            }
            // Malformed `${N…` with no `:` or `}`: not a stop, leave `$` literal.
            _ => return None,
        }
    }
    Some((num, placeholder))
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
    fn applies_to(&self, language: &str) -> bool {
        self.scope.is_empty() || self.scope.iter().any(|s| s == language)
    }
}

/// The loaded set of user snippets.
#[derive(Debug, Clone, Default)]
pub struct SnippetSet {
    snippets: Vec<Snippet>,
}

/// The on-disk value shape: `body` is a string or an array of lines; `scope` is
/// an optional comma-separated language list.
#[derive(Debug, Deserialize)]
struct RawSnippet {
    prefix: String,
    #[serde(default)]
    body: Body,
    #[serde(default)]
    scope: Option<String>,
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
        let stripped = crate::keymap::strip_line_comments(json);
        let raw: std::collections::BTreeMap<String, RawSnippet> =
            serde_json::from_str(&stripped).unwrap_or_default();
        let snippets = raw
            .into_values()
            .filter(|r| !r.prefix.is_empty())
            .map(|r| Snippet {
                prefix: r.prefix,
                body: r.body.joined(),
                scope: r
                    .scope
                    .map(|s| {
                        s.split(',')
                            .map(|p| p.trim().to_string())
                            .filter(|p| !p.is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect();
        Self { snippets }
    }

    pub fn is_empty(&self) -> bool {
        self.snippets.is_empty()
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
    crate::prefs::config_dir().join("snippets.json")
}

/// Seeded on first "Configure User Snippets" so the user starts from a working
/// example rather than a blank buffer.
pub const TEMPLATE: &str = r#"// croft user snippets. Keyed by name; each has a "prefix" you type then expand
// with Tab, a "body" (string or array of lines), and an optional "scope"
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
}
