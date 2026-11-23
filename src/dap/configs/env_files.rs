//! `envFile`: reading a dotenv file as python-dotenv and VS Code do, its quotes,
//! escapes, comments and `${NAME}` references.

use super::*;

/// Parse a dotenv-style file as python-dotenv and VS Code read it (#1389):
/// `KEY=VALUE` lines, `#` comment lines, blank lines and an optional
/// `export ` prefix. An unquoted value ends at a `#` that follows whitespace,
/// an inline comment. A double-quoted value may span lines and takes the
/// `\n`, `\t`, `\r`, `\"` and `\\` escapes; a single-quoted one is literal.
/// `${NAME}` in an unquoted or double-quoted value expands to NAME from an
/// earlier line, else from croft's environment, else to nothing, and
/// `${NAME:-default}` to `default` when NAME is set nowhere.
pub fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    parse_env_file_in(text, |name| std::env::var(name).ok())
}

/// [`parse_env_file`] with `process_env` standing in for croft's
/// environment.
pub(super) fn parse_env_file_in(
    text: &str,
    process_env: impl Fn(&str) -> Option<String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut pos = 0;
    while pos < text.len() {
        let line_end = text[pos..].find('\n').map_or(text.len(), |i| pos + i);
        let line = &text[pos..line_end];
        pos = line_end + 1;
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let entry = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some((key, raw)) = entry.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            continue;
        }
        let value = raw.trim_start();
        // Where `value` starts in `text`, for a quoted value that runs on
        // past this line.
        let start = line_end - line.len() + (line.len() - value.len());
        let quoted = match value.chars().next() {
            Some(q @ ('"' | '\'')) => closing_quote(text, start + 1, q).map(|end| (q, end)),
            _ => None,
        };
        let value = match quoted {
            Some(('\'', end)) => {
                pos = pos.max(next_line(text, end));
                text[start + 1..end].to_string()
            }
            Some((_, end)) => {
                pos = pos.max(next_line(text, end));
                let unescaped = unescape_double_quoted(&text[start + 1..end]);
                expand_env_refs(&unescaped, &out, &process_env)
            }
            None => {
                let comment = raw
                    .char_indices()
                    .find(|&(i, c)| c == '#' && raw[..i].ends_with(char::is_whitespace))
                    .map_or(raw.len(), |(i, _)| i);
                expand_env_refs(raw[..comment].trim(), &out, &process_env)
            }
        };
        out.insert(key.to_string(), value);
    }
    out
}

/// The index of the quote closing a value whose body starts at `from`, which
/// may be on a later line; `None` when it never closes. A double-quoted body
/// skips `\"` and any other backslash pair.
pub(super) fn closing_quote(text: &str, from: usize, quote: char) -> Option<usize> {
    let mut chars = text[from..].char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' if quote == '"' => {
                chars.next();
            }
            c if c == quote => return Some(from + i),
            _ => {}
        }
    }
    None
}

/// Where the line holding `at` ends, past its newline: what follows a
/// closing quote on its line, a comment, is no part of the next entry.
pub(super) fn next_line(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |i| at + i + 1)
}

/// A double-quoted dotenv value's escapes, decoded; an unknown escape keeps
/// its backslash.
pub(super) fn unescape_double_quoted(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(c @ ('"' | '\\')) => out.push(c),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// `${NAME}` and `${NAME:-default}` in a dotenv value, expanded from the
/// file's earlier entries, then `process_env`. An unterminated `${` is kept
/// as written.
pub(super) fn expand_env_refs(
    value: &str,
    file: &BTreeMap<String, String>,
    process_env: &impl Fn(&str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let (name, default) = match after[..end].split_once(":-") {
            Some((name, default)) => (name, Some(default)),
            None => (&after[..end], None),
        };
        match file.get(name).cloned().or_else(|| process_env(name)) {
            Some(found) => out.push_str(&found),
            None => out.push_str(default.unwrap_or("")),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}
