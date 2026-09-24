//! SARIF regions (§3.30) mapped onto the text croft has in hand.
//!
//! A region is expressed in the producer's units: 1-based lines split by
//! `run.newlineSequences`, and columns counted per `run.columnKind`. croft's
//! editor wants 0-based lines and columns counted in `char`s, so every
//! conversion goes through [`TextRange`], whose columns are code points.

use super::model::{Region, Run};

/// §3.14.27. The spec gives no default, so a log that omits it is read as
/// UTF-16: that is what CodeQL, the .NET SDK and VS Code all assume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Utf16CodeUnits,
    UnicodeCodePoints,
}

pub fn column_kind(run: &Run) -> ColumnKind {
    match run.column_kind.as_deref() {
        Some("unicodeCodePoints") => ColumnKind::UnicodeCodePoints,
        _ => ColumnKind::Utf16CodeUnits,
    }
}

/// §3.14.26: the newline sequences the producer counted lines by, tried
/// greedily in order. Absent means `["\r\n", "\n"]`.
pub fn newline_sequences(run: &Run) -> Vec<String> {
    match &run.newline_sequences {
        Some(seqs) if seqs.iter().any(|s| !s.is_empty()) => {
            seqs.iter().filter(|s| !s.is_empty()).cloned().collect()
        }
        _ => vec!["\r\n".into(), "\n".into()],
    }
}

/// A position in croft terms: 0-based line, 0-based column in `char`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

/// A half-open text range. `clamped` is set when the region pointed past the
/// end of the text and had to be pulled back, which usually means the file
/// changed since the log was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRange {
    pub start: Pos,
    pub end: Pos,
    pub clamped: bool,
}

/// How the text was split into lines for a given set of newline sequences.
pub struct Lines<'a> {
    text: &'a str,
    /// `(start, end_excluding_newline, end_including_newline)` byte offsets.
    spans: Vec<(usize, usize, usize)>,
}

impl<'a> Lines<'a> {
    pub fn new(text: &'a str, newlines: &[String]) -> Lines<'a> {
        let mut spans = Vec::new();
        let mut start = 0;
        let mut i = 0;
        let bytes = text.as_bytes();
        while i < bytes.len() {
            match newlines.iter().find(|n| text[i..].starts_with(n.as_str())) {
                Some(n) => {
                    spans.push((start, i, i + n.len()));
                    i += n.len();
                    start = i;
                }
                None => i += text[i..].chars().next().map_or(1, char::len_utf8),
            }
        }
        if start < text.len() {
            spans.push((start, text.len(), text.len()));
        }
        Lines { text, spans }
    }

    pub fn count(&self) -> usize {
        self.spans.len()
    }

    /// Line `n` including its newline; the empty string past the end.
    fn full(&self, n: usize) -> &'a str {
        self.spans
            .get(n)
            .map_or("", |&(s, _, e)| &self.text[s..e])
    }

    /// Byte offset of `pos` (column in chars) in the whole text.
    fn byte_of(&self, pos: Pos) -> usize {
        match self.spans.get(pos.line) {
            None => self.text.len(),
            Some(&(s, _, e)) => {
                let line = &self.text[s..e];
                s + line
                    .char_indices()
                    .nth(pos.col)
                    .map_or(line.len(), |(b, _)| b)
            }
        }
    }
}

/// Units one `char` occupies under `kind`.
fn width(c: char, kind: ColumnKind) -> usize {
    match kind {
        ColumnKind::Utf16CodeUnits => c.len_utf16(),
        ColumnKind::UnicodeCodePoints => 1,
    }
}

/// Convert a 0-based offset in `kind` units within `s` into a `char` count.
/// Returns `(chars, overflowed)`; an offset inside a surrogate pair rounds up.
fn units_to_chars(s: &str, units: usize, kind: ColumnKind) -> (usize, bool) {
    let mut seen = 0;
    for (n, c) in s.chars().enumerate() {
        if seen >= units {
            return (n, false);
        }
        seen += width(c, kind);
    }
    (s.chars().count(), seen < units)
}

/// A 1-based SARIF line/column (column in `kind` units) as a croft position.
/// `default_end` places a missing column one past the line's last character.
fn line_col(lines: &Lines, line: i64, col: Option<i64>, kind: ColumnKind) -> (Pos, bool) {
    let mut clamped = false;
    let mut l = usize::try_from(line.max(1) - 1).unwrap_or(0);
    let last = lines.count().saturating_sub(1);
    if lines.count() == 0 {
        return (Pos { line: 0, col: 0 }, line > 1);
    }
    if l > last {
        l = last;
        clamped = true;
    }
    let (s, e, _) = lines.spans[l];
    let body = &lines.text[s..e];
    let col = match col {
        None => body.chars().count(),
        Some(c) => {
            let (n, over) =
                units_to_chars(lines.full(l), usize::try_from(c.max(1) - 1).unwrap_or(0), kind);
            let body_chars = body.chars().count();
            let full_chars = lines.full(l).chars().count();
            // A column may address the newline itself (to include it); one
            // past the whole line is the next line's column 1.
            if over || n > full_chars {
                clamped = true;
                body_chars
            } else if n == full_chars && full_chars > body_chars {
                return (Pos { line: l + 1, col: 0 }, clamped);
            } else {
                n
            }
        }
    };
    (Pos { line: l, col }, clamped)
}

/// §3.30.2: resolve a text region. Line/column properties take precedence;
/// `charOffset`/`charLength` are used when `startLine` is absent. `None` for a
/// region with no text properties (binary-only).
pub fn text_range(region: &Region, lines: &Lines, kind: ColumnKind) -> Option<TextRange> {
    if let Some(start_line) = region.start_line.filter(|&l| l > 0) {
        let (start, c1) = line_col(lines, start_line, region.start_column.or(Some(1)), kind);
        let end_line = region.end_line.unwrap_or(start_line).max(start_line);
        let (end, c2) = line_col(lines, end_line, region.end_column, kind);
        let end = end.max(start);
        return Some(TextRange {
            start,
            end,
            clamped: c1 || c2,
        });
    }
    let offset = region.char_offset.filter(|&o| o >= 0)?;
    let length = region.char_length.unwrap_or(0).max(0);
    let (start, c1) = offset_pos(lines, offset as usize, kind);
    let (end, c2) = offset_pos(lines, (offset + length) as usize, kind);
    Some(TextRange {
        start,
        end,
        clamped: c1 || c2,
    })
}

/// A 0-based character offset (in `kind` units) as a position.
fn offset_pos(lines: &Lines, units: usize, kind: ColumnKind) -> (Pos, bool) {
    let mut left = units;
    for (n, &(s, _, e)) in lines.spans.iter().enumerate() {
        let full = &lines.text[s..e];
        let w: usize = full.chars().map(|c| width(c, kind)).sum();
        if left < w {
            let (col, _) = units_to_chars(full, left, kind);
            return (Pos { line: n, col }, false);
        }
        left -= w;
    }
    // Exactly at the end is a valid insertion point; beyond it is clamped.
    let at_end = Pos {
        line: lines.count(),
        col: 0,
    };
    let at_end = match lines.spans.last() {
        Some(&(_, e, full_e)) if e == full_e => Pos {
            line: lines.count() - 1,
            col: lines.full(lines.count() - 1).chars().count(),
        },
        _ => at_end,
    };
    (at_end, left > 0)
}

/// §3.30.3: `(offset, length)` of a binary region.
pub fn byte_range(region: &Region) -> Option<(u64, u64)> {
    let offset = u64::try_from(region.byte_offset?).ok()?;
    let length = region
        .byte_length
        .and_then(|l| u64::try_from(l).ok())
        .unwrap_or(0);
    Some((offset, length))
}

/// The text a range covers, for comparing against `region.snippet`.
pub fn slice<'t>(lines: &Lines<'t>, range: &TextRange) -> &'t str {
    let a = lines.byte_of(range.start);
    let b = lines.byte_of(range.end).max(a);
    &lines.text[a..b]
}

/// Re-anchor a region whose file drifted since the log was written: if the
/// text at the stated range no longer equals `snippet.text`, find the
/// occurrence of the snippet nearest the stated start line. `None` when the
/// region has no snippet, already matches, or the snippet is gone.
pub fn reanchor(region: &Region, lines: &Lines, kind: ColumnKind) -> Option<TextRange> {
    let snippet = region.snippet.as_ref()?.text.as_deref()?;
    if snippet.is_empty() {
        return None;
    }
    let stated = text_range(region, lines, kind);
    if let Some(r) = stated
        && !r.clamped
        && slice(lines, &r) == snippet
    {
        return None;
    }
    let near = stated.map_or(0, |r| lines.byte_of(r.start));
    let best = lines
        .text
        .match_indices(snippet)
        .map(|(b, _)| b)
        .min_by_key(|&b| b.abs_diff(near))?;
    let start = pos_of_byte(lines, best);
    let end = pos_of_byte(lines, best + snippet.len());
    Some(TextRange {
        start,
        end,
        clamped: false,
    })
}

fn pos_of_byte(lines: &Lines, byte: usize) -> Pos {
    for (n, &(s, _, e)) in lines.spans.iter().enumerate() {
        if byte < e || (byte == e && n + 1 == lines.count() && byte == lines.text.len()) {
            if byte >= e {
                return Pos {
                    line: n,
                    col: lines.text[s..e].chars().count(),
                };
            }
            return Pos {
                line: n,
                col: lines.text[s..byte].chars().count(),
            };
        }
    }
    Pos {
        line: lines.count(),
        col: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's own example text (§3.30.2 NOTE 1).
    const SPEC: &str = "abcd\r\nefg\r\nhijk\r\nlmn\r\n";

    fn crlf() -> Vec<String> {
        vec!["\r\n".into(), "\n".into()]
    }

    fn region(json: &str) -> Region {
        serde_json::from_str(json).unwrap()
    }

    fn run(json: &str) -> Run {
        serde_json::from_str(json).unwrap()
    }

    fn text_of(text: &str, json: &str) -> String {
        let lines = Lines::new(text, &crlf());
        let r = text_range(&region(json), &lines, ColumnKind::UnicodeCodePoints).unwrap();
        slice(&lines, &r).to_string()
    }

    // ── run-level settings ──────────────────────────────────────────────

    #[test]
    fn column_kind_defaults_to_utf16() {
        assert_eq!(column_kind(&run("{}")), ColumnKind::Utf16CodeUnits);
        assert_eq!(
            column_kind(&run(r#"{"columnKind":"unicodeCodePoints"}"#)),
            ColumnKind::UnicodeCodePoints
        );
        assert_eq!(
            column_kind(&run(r#"{"columnKind":"utf16CodeUnits"}"#)),
            ColumnKind::Utf16CodeUnits
        );
    }

    #[test]
    fn newline_sequences_default() {
        assert_eq!(newline_sequences(&run("{}")), crlf());
        assert_eq!(
            newline_sequences(&run(r#"{"newlineSequences":["\n"," "]}"#)),
            vec!["\n".to_string(), "\u{2028}".to_string()]
        );
    }

    #[test]
    fn lines_split_greedily() {
        assert_eq!(Lines::new(SPEC, &crlf()).count(), 4);
        // With "\r" listed before "\r\n", "\r\n" is two breaks.
        let odd = vec!["\r".to_string(), "\n".to_string()];
        assert_eq!(Lines::new("a\r\nb", &odd).count(), 3);
        // No trailing newline: the last line still counts.
        assert_eq!(Lines::new("a\nb", &crlf()).count(), 2);
    }

    // ── §3.30.2 examples ────────────────────────────────────────────────

    #[test]
    fn example_1_line_column() {
        assert_eq!(
            text_of(SPEC, r#"{"startLine":1,"startColumn":2,"endLine":1,"endColumn":4}"#),
            "bc"
        );
    }

    #[test]
    fn example_1_char_offset() {
        assert_eq!(text_of(SPEC, r#"{"charOffset":1,"charLength":2}"#), "bc");
    }

    #[test]
    fn example_3_whole_line_without_newline() {
        assert_eq!(text_of(SPEC, r#"{"startLine":2}"#), "efg");
    }

    #[test]
    fn example_4_two_lines() {
        assert_eq!(
            text_of(SPEC, r#"{"startLine":2,"endLine":3}"#),
            "efg\r\nhijk"
        );
    }

    #[test]
    fn example_5_line_with_its_newline() {
        assert_eq!(
            text_of(SPEC, r#"{"startLine":2,"endLine":3,"endColumn":1}"#),
            "efg\r\n"
        );
    }

    #[test]
    fn example_6_insertion_point() {
        let lines = Lines::new(SPEC, &crlf());
        let r = text_range(
            &region(r#"{"startLine":1,"startColumn":2,"endColumn":2}"#),
            &lines,
            ColumnKind::UnicodeCodePoints,
        )
        .unwrap();
        assert_eq!(r.start, Pos { line: 0, col: 1 });
        assert_eq!(r.end, r.start);
        let r = text_range(
            &region(r#"{"charOffset":1}"#),
            &lines,
            ColumnKind::UnicodeCodePoints,
        )
        .unwrap();
        assert_eq!((r.start, r.end), (Pos { line: 0, col: 1 }, Pos { line: 0, col: 1 }));
    }

    #[test]
    fn example_8_end_of_file() {
        let lines = Lines::new(SPEC, &crlf());
        let r = text_range(
            &region(r#"{"charOffset":22,"charLength":0}"#),
            &lines,
            ColumnKind::UnicodeCodePoints,
        )
        .unwrap();
        assert!(!r.clamped);
        assert_eq!(slice(&lines, &r), "");
        assert_eq!(r.start, Pos { line: 4, col: 0 });
    }

    #[test]
    fn line_properties_win_over_char_offset() {
        // §3.30.2: startLine > 0 makes it a line/column region.
        assert_eq!(
            text_of(SPEC, r#"{"startLine":3,"charOffset":0,"charLength":2}"#),
            "hijk"
        );
    }

    // ── column units ────────────────────────────────────────────────────

    #[test]
    fn utf16_columns_count_surrogate_pairs_twice() {
        // "😀" is one code point, two UTF-16 units.
        let text = "a😀bc\n";
        let lines = Lines::new(text, &crlf());
        let r = text_range(
            &region(r#"{"startLine":1,"startColumn":4,"endColumn":5}"#),
            &lines,
            ColumnKind::Utf16CodeUnits,
        )
        .unwrap();
        assert_eq!(slice(&lines, &r), "b");
        assert_eq!(r.start, Pos { line: 0, col: 2 });
    }

    #[test]
    fn code_point_columns_count_surrogate_pairs_once() {
        let text = "a😀bc\n";
        let lines = Lines::new(text, &crlf());
        let r = text_range(
            &region(r#"{"startLine":1,"startColumn":3,"endColumn":4}"#),
            &lines,
            ColumnKind::UnicodeCodePoints,
        )
        .unwrap();
        assert_eq!(slice(&lines, &r), "b");
    }

    #[test]
    fn utf16_char_offset() {
        let text = "😀x";
        let lines = Lines::new(text, &crlf());
        let r = text_range(
            &region(r#"{"charOffset":2,"charLength":1}"#),
            &lines,
            ColumnKind::Utf16CodeUnits,
        )
        .unwrap();
        assert_eq!(slice(&lines, &r), "x");
    }

    #[test]
    fn leading_whitespace_is_not_skipped() {
        // VS Code pushes startColumn past indentation; the spec does not.
        assert_eq!(
            text_of("    let x;\n", r#"{"startLine":1,"startColumn":1,"endColumn":5}"#),
            "    "
        );
    }

    // ── out of range ────────────────────────────────────────────────────

    #[test]
    fn line_past_end_is_clamped_and_flagged() {
        let lines = Lines::new("one\ntwo\n", &crlf());
        let r = text_range(
            &region(r#"{"startLine":9}"#),
            &lines,
            ColumnKind::UnicodeCodePoints,
        )
        .unwrap();
        assert!(r.clamped);
        assert!(r.start.line < 3);
    }

    #[test]
    fn column_past_end_of_line_is_clamped() {
        let lines = Lines::new("one\ntwo\n", &crlf());
        let r = text_range(
            &region(r#"{"startLine":1,"startColumn":2,"endColumn":40}"#),
            &lines,
            ColumnKind::UnicodeCodePoints,
        )
        .unwrap();
        assert!(r.clamped);
        assert_eq!(slice(&lines, &r), "ne");
    }

    #[test]
    fn binary_only_region_has_no_text_range() {
        let lines = Lines::new(SPEC, &crlf());
        assert!(text_range(&region(r#"{"byteOffset":4}"#), &lines, ColumnKind::Utf16CodeUnits).is_none());
        assert_eq!(byte_range(&region(r#"{"byteOffset":4}"#)), Some((4, 0)));
        assert_eq!(
            byte_range(&region(r#"{"byteOffset":4,"byteLength":8}"#)),
            Some((4, 8))
        );
        assert_eq!(byte_range(&region(r#"{"startLine":1}"#)), None);
    }

    // ── re-anchoring ────────────────────────────────────────────────────

    #[test]
    fn reanchor_follows_a_moved_snippet() {
        // The log was written when `bad()` was on line 2; two lines were
        // inserted above it since.
        let text = "fn a() {}\n// new\n// new\n    bad();\n";
        let lines = Lines::new(text, &crlf());
        let r = region(
            r#"{"startLine":2,"startColumn":5,"endColumn":11,"snippet":{"text":"bad();"}}"#,
        );
        let moved = reanchor(&r, &lines, ColumnKind::UnicodeCodePoints).unwrap();
        assert_eq!(moved.start, Pos { line: 3, col: 4 });
        assert_eq!(slice(&lines, &moved), "bad();");
    }

    #[test]
    fn reanchor_prefers_the_nearest_occurrence() {
        let text = "x();\nfiller\nfiller\nfiller\nfiller\nfiller\nstated\nx();\n";
        let lines = Lines::new(text, &crlf());
        // Stated on line 7 ("stated"); the x() on line 8 is nearer than line 1.
        let r = region(r#"{"startLine":7,"startColumn":1,"endColumn":5,"snippet":{"text":"x();"}}"#);
        let moved = reanchor(&r, &lines, ColumnKind::UnicodeCodePoints).unwrap();
        assert_eq!(moved.start.line, 7);
    }

    #[test]
    fn reanchor_is_none_when_already_matching_or_gone() {
        let lines = Lines::new("ok();\n", &crlf());
        let here = region(r#"{"startLine":1,"startColumn":1,"endColumn":6,"snippet":{"text":"ok();"}}"#);
        assert!(reanchor(&here, &lines, ColumnKind::UnicodeCodePoints).is_none());
        let gone = region(r#"{"startLine":1,"snippet":{"text":"missing"}}"#);
        assert!(reanchor(&gone, &lines, ColumnKind::UnicodeCodePoints).is_none());
        let none = region(r#"{"startLine":1}"#);
        assert!(reanchor(&none, &lines, ColumnKind::UnicodeCodePoints).is_none());
    }

    #[test]
    fn reanchor_multiline_snippet() {
        let text = "a\nb\nfoo(\n  1)\n";
        let lines = Lines::new(text, &crlf());
        let r = region(r#"{"startLine":1,"endLine":2,"snippet":{"text":"foo(\n  1)"}}"#);
        let moved = reanchor(&r, &lines, ColumnKind::UnicodeCodePoints).unwrap();
        assert_eq!(moved.start, Pos { line: 2, col: 0 });
        assert_eq!(moved.end, Pos { line: 3, col: 4 });
    }
}
