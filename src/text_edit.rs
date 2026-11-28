//! The text-edit value that language servers, formatters and the editor pass
//! between them, how a set of them applies to plain lines, and the column and
//! newline helpers that rests on. A leaf, so the LSP layer and the formatters
//! can produce and apply edits without depending on the editor widget.

/// A single text replacement in char-indexed `(row, col)` coordinates, as
/// produced by an LSP rename `WorkspaceEdit`. Applied by
/// [`apply_span_edits_to_lines`] here and by the editor's `apply_span_edits`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSpanEdit {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub new_text: String,
    /// The columns are UTF-16 code units, as a language server sends them,
    /// not chars. Converted against the lines at apply time: read as char
    /// columns, every edit after an emoji on its line landed off by one per
    /// astral character, and a rename wrote `xrenamed+ 1` over `x + 1`.
    pub utf16: bool,
}

/// Character column for the LSP UTF-16 offset `u16_off` within `line`. LSP
/// positions count UTF-16 code units; the editor lays out by character, so a
/// line containing astral-plane characters (which take two UTF-16 units) needs
/// this conversion. An offset past the line clamps to the line's char length.
pub(crate) fn utf16_to_char_col(line: &str, u16_off: u32) -> usize {
    let mut units: u32 = 0;
    for (char_idx, ch) in line.chars().enumerate() {
        if units >= u16_off {
            return char_idx;
        }
        units += ch.len_utf16() as u32;
    }
    line.chars().count()
}

/// Convert a char index within `s` to a byte index, saturating at `s.len()`.
pub(crate) fn char_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Fold every line-ending convention to `\n` so callers can split on it
/// alone. Shared by the file loader and the find bar's replacement paths: a
/// replacement carrying `\r` (VS Code's escape, or pasted CRLF text) must
/// become a real line break, never a bare CR sitting inside a line — a CRLF
/// buffer would then save `\r\r\n` and every line count would disagree.
pub(crate) fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Apply `edits` to `lines` (char-indexed coords), bottom-to-top so an
/// earlier replacement never shifts the coordinates of a later one. Returns
/// the number of edits applied. Out-of-range edits are skipped.
pub fn apply_span_edits_to_lines(lines: &mut Vec<String>, edits: &[TextSpanEdit]) -> usize {
    let converted = char_span_edits(lines, edits);
    apply_char_span_edits(lines, converted)
        .iter()
        .map(|(_, count)| count)
        .sum()
}

/// Apply char-column `edits` in [`in_application_order`] and return the
/// ones that landed, in the order applied, each with the number of edits it
/// stands for; out-of-range edits are skipped and left out, so the caret is
/// mapped only through text that actually changed.
pub(crate) fn apply_char_span_edits(
    lines: &mut Vec<String>,
    edits: Vec<TextSpanEdit>,
) -> Vec<(TextSpanEdit, usize)> {
    in_application_order(edits)
        .into_iter()
        .filter(|(e, _)| replace_span(lines, e))
        .collect()
}

/// `edits` with their UTF-16 columns read as chars. Every edit names the
/// ORIGINAL text, so the columns convert against `lines` as they are before
/// any edit lands.
pub(crate) fn char_span_edits(lines: &[String], edits: &[TextSpanEdit]) -> Vec<TextSpanEdit> {
    let col = |row: usize, c: usize| lines.get(row).map_or(c, |l| utf16_to_char_col(l, c as u32));
    edits
        .iter()
        .map(|e| match e.utf16 {
            true => TextSpanEdit {
                start: (e.start.0, col(e.start.0, e.start.1)),
                end: (e.end.0, col(e.end.0, e.end.1)),
                new_text: e.new_text.clone(),
                utf16: false,
            },
            false => e.clone(),
        })
        .collect()
}

/// `edits` bottom-up, the order they apply in, each with the number of edits
/// it stands for. Edits that start at one position are joined into one, their
/// texts in the order sent: the LSP spec puts them in array order, and
/// applying them one by one bottom-up reversed them, so vtsls's `"\n"` then
/// `"  "` landed as trailing spaces above an unindented line (#1633).
pub(crate) fn in_application_order(mut edits: Vec<TextSpanEdit>) -> Vec<(TextSpanEdit, usize)> {
    edits.sort_by_key(|e| e.start);
    let mut joined: Vec<(TextSpanEdit, usize)> = Vec::with_capacity(edits.len());
    for e in edits {
        match joined.last_mut() {
            Some((last, count)) if last.start == e.start => {
                last.end = last.end.max(e.end);
                last.new_text.push_str(&e.new_text);
                *count += 1;
            }
            _ => joined.push((e, 1)),
        }
    }
    joined.reverse();
    joined
}

/// Where the text `e` writes ends, in the coordinates after it lands.
pub(crate) fn span_new_end(e: &TextSpanEdit) -> (usize, usize) {
    let rows = e.new_text.matches('\n').count();
    let last = e.new_text.rsplit('\n').next().unwrap_or("").chars().count();
    match rows {
        0 => (e.start.0, e.start.1 + last),
        _ => (e.start.0 + rows, last),
    }
}

/// `pos` moved past an edit that ends at or before it: by the rows the edit
/// added or removed and, when it ends on `pos`'s row, by the change in that
/// row's text before `pos`.
pub(crate) fn shift_past_span(
    pos: (usize, usize),
    e: &TextSpanEdit,
    new_end: (usize, usize),
) -> (usize, usize) {
    if pos.0 == e.end.0 {
        (new_end.0, new_end.1 + pos.1 - e.end.1)
    } else {
        (pos.0 + new_end.0 - e.end.0, pos.1)
    }
}

/// Where `pos` lands once char-column `edits` are applied (#1603): the
/// caret follows its text. An edit ending at or before `pos` shifts it past
/// the edit; a `pos` inside a replaced range keeps its row and column,
/// clamped to the end of the new text, so a whole-document format leaves the
/// caret on its line number as before and a deletion pulls it to where the
/// range began; edits after `pos` leave it alone. Mapped bottom-up, the
/// order the edits are applied in, so each edit's coordinates still name the
/// text it was written against.
pub fn map_through_span_edits(pos: (usize, usize), edits: &[TextSpanEdit]) -> (usize, usize) {
    map_through_edit_refs(pos, edits.iter().collect())
}

/// Where the viewport's top line lands once char-column `edits` are applied
/// (#918): lines added or removed above the view, by a peer's typing or an Add
/// import, move it with them, so the text on screen stays put. Only
/// edits that start above the top line's start count: text inserted right at
/// it shows at the top of the view, and the top of the file stays the top.
pub(crate) fn view_top_after_span_edits(top: usize, edits: &[TextSpanEdit]) -> usize {
    let start = (top, 0);
    map_through_edit_refs(start, edits.iter().filter(|e| e.start < start).collect()).0
}

/// [`map_through_span_edits`] over borrowed edits.
pub(crate) fn map_through_edit_refs(
    pos: (usize, usize),
    mut order: Vec<&TextSpanEdit>,
) -> (usize, usize) {
    order.retain(|e| e.end >= e.start);
    order.sort_by_key(|e| std::cmp::Reverse(e.start));
    order.into_iter().fold(pos, |p, e| {
        if p < e.end && p <= e.start {
            return p;
        }
        let new_end = span_new_end(e);
        match p < e.end {
            true => p.min(new_end),
            false => shift_past_span(p, e, new_end),
        }
    })
}

/// Where `pos` (char columns) lands once `edits` apply to `lines`: moved by
/// the text every edit before it adds or removes. A position inside an edit
/// lands at the end of its new text, unlike [`map_through_span_edits`]:
/// format on type rewrites the line just typed, and the caret belongs after
/// the text it typed, not at its old column (#1633).
pub fn position_after_span_edits(
    lines: &[String],
    pos: (usize, usize),
    edits: &[TextSpanEdit],
) -> (usize, usize) {
    let mut pos = pos;
    for (e, _) in in_application_order(char_span_edits(lines, edits)) {
        if pos < e.start || (pos == e.start && e.start != e.end) {
            continue;
        }
        let new_end = span_new_end(&e);
        pos = match pos < e.end {
            true => new_end,
            false => shift_past_span(pos, &e, new_end),
        };
    }
    pos
}

pub(crate) fn replace_span(lines: &mut Vec<String>, e: &TextSpanEdit) -> bool {
    let (sr, sc) = e.start;
    let (er, ec) = e.end;
    if sr >= lines.len() || er >= lines.len() || (er, ec) < (sr, sc) {
        return false;
    }
    let from = char_byte(&lines[sr], sc);
    let to = char_byte(&lines[er], ec);
    let head = lines[sr][..from].to_string();
    let tail = lines[er][to..].to_string();
    // Build the replacement text in full, then split it back on '\n' so a
    // multi-line `new_text` (e.g. ruff's "Organize Imports", a whole-document
    // reformat) becomes separate rows instead of one row with embedded newline
    // bytes. A single-line edit (rename) yields exactly one row, unchanged.
    let combined = format!("{head}{}{tail}", e.new_text);
    let new_lines: Vec<String> = combined.split('\n').map(str::to_string).collect();
    let _ = lines.splice(sr..=er, new_lines);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An insert of `text` at `(row, col)`, in UTF-16 columns as a
    /// language server sends it.
    fn lsp_insert(row: usize, col: usize, text: &str) -> TextSpanEdit {
        TextSpanEdit {
            start: (row, col),
            end: (row, col),
            new_text: text.to_string(),
            utf16: true,
        }
    }

    /// vtsls 0.3.0's Format Document reply for
    /// `export function f(a: number) {if(a>1){return  0;}`, captured with a
    /// minimal LSP client (#1633). The first two edits share a position.
    fn vtsls_format_reply() -> Vec<TextSpanEdit> {
        let mut edits = vec![
            lsp_insert(0, 30, "\n"),
            lsp_insert(0, 30, "  "),
            lsp_insert(0, 32, " "),
            lsp_insert(0, 34, " "),
            lsp_insert(0, 35, " "),
            lsp_insert(0, 37, " "),
            lsp_insert(0, 38, " "),
        ];
        edits.push(TextSpanEdit {
            start: (0, 44),
            end: (0, 46),
            new_text: " ".to_string(),
            utf16: true,
        });
        edits.push(lsp_insert(0, 48, " "));
        edits
    }

    #[test]
    fn apply_span_edits_applies_bottom_to_top() {
        let mut lines = vec!["foo bar foo".to_string()];
        let edits = vec![
            TextSpanEdit {
                start: (0, 0),
                end: (0, 3),
                new_text: "baz".to_string(),
                utf16: false,
            },
            TextSpanEdit {
                start: (0, 8),
                end: (0, 11),
                new_text: "baz".to_string(),
                utf16: false,
            },
        ];
        assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 2);
        assert_eq!(lines, vec!["baz bar baz".to_string()]);
    }

    /// #1633: edits that start at one position land in the order the server
    /// sent them, as the LSP spec says.
    #[test]
    fn inserts_at_one_position_land_in_the_order_sent() {
        let mut lines = vec!["ab".to_string()];
        let edits = vec![lsp_insert(0, 1, "\n"), lsp_insert(0, 1, "  ")];
        assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 2);
        assert_eq!(lines, vec!["a".to_string(), "  b".to_string()]);
        // An insert sent before a replacement of the text after it.
        let mut lines = vec!["foobar".to_string()];
        let edits = vec![
            lsp_insert(0, 0, "X"),
            TextSpanEdit {
                start: (0, 0),
                end: (0, 3),
                new_text: String::new(),
                utf16: true,
            },
        ];
        assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 2);
        assert_eq!(lines, vec!["Xbar".to_string()]);
    }

    /// #1633: the issue's TypeScript sample formats to an indented line with
    /// no trailing spaces.
    #[test]
    fn a_vtsls_format_reply_indents_the_line_it_breaks() {
        let mut lines: Vec<String> = [
            "export function f(a: number) {if(a>1){return  0;}",
            "  return a;",
            "}",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            apply_span_edits_to_lines(&mut lines, &vtsls_format_reply()),
            9
        );
        assert_eq!(
            lines,
            [
                "export function f(a: number) {",
                "  if (a > 1) { return 0; }",
                "  return a;",
                "}"
            ]
            .map(String::from)
            .to_vec()
        );
    }

    /// #1633 negative: edits at different positions, and a replacement sent
    /// before an insert at its start, keep their meaning.
    #[test]
    fn edits_at_different_positions_keep_their_places() {
        let mut lines = vec!["a b c".to_string()];
        let edits = vec![
            lsp_insert(0, 4, "Z"),
            lsp_insert(0, 0, "X"),
            lsp_insert(0, 2, "Y"),
        ];
        assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 3);
        assert_eq!(lines, vec!["Xa Yb Zc".to_string()]);
        let mut lines = vec!["foobar".to_string()];
        let edits = vec![
            TextSpanEdit {
                start: (0, 0),
                end: (0, 3),
                new_text: String::new(),
                utf16: true,
            },
            lsp_insert(0, 0, "X"),
        ];
        assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 2);
        assert_eq!(lines, vec!["Xbar".to_string()]);
    }

    #[test]
    fn apply_span_edits_splits_multiline_new_text_into_separate_lines() {
        // Ruff's "Organize Imports" returns ONE edit whose new_text spans several
        // lines. The result must become several Vec entries, not one line with
        // embedded '\n' (the bug that mashed the imports onto a single line).
        let mut lines = vec![
            "import typing".to_string(),
            "import pandas".to_string(),
            "import logging".to_string(),
            "import re".to_string(),
        ];
        let edits = vec![TextSpanEdit {
            start: (0, 0),
            end: (3, 9),
            new_text: "import logging\nimport re\nimport typing\n\nimport pandas".to_string(),
            utf16: false,
        }];
        assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 1);
        assert_eq!(
            lines,
            vec![
                "import logging".to_string(),
                "import re".to_string(),
                "import typing".to_string(),
                String::new(),
                "import pandas".to_string(),
            ]
        );
    }

    #[test]
    fn apply_span_edits_single_line_range_with_multiline_new_text() {
        // A same-line range whose new_text introduces a newline must split the
        // line in two, keeping the untouched prefix and suffix.
        let mut lines = vec!["abXYef".to_string()];
        let edits = vec![TextSpanEdit {
            start: (0, 2),
            end: (0, 4),
            new_text: "C\nD".to_string(),
            utf16: false,
        }];
        assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 1);
        assert_eq!(lines, vec!["abC".to_string(), "Def".to_string()]);
    }

    #[test]
    fn a_language_servers_utf16_columns_land_after_an_emoji() {
        let mut lines = vec![String::from("let s = \"😀\"; x + 1")];
        let n = apply_span_edits_to_lines(
            &mut lines,
            &[TextSpanEdit {
                start: (0, 14),
                end: (0, 15),
                new_text: String::from("renamed"),
                utf16: true,
            }],
        );
        assert_eq!(n, 1);
        assert_eq!(lines[0], "let s = \"😀\"; renamed + 1");
    }
}
