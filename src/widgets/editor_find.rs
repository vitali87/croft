use crate::widgets::search::{Pattern, SearchOpts, split_for_highlight};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

/// Which input row of the find bar owns the keyboard (Tab toggles).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FindField {
    #[default]
    Query,
    Replace,
}

#[derive(Default)]
pub struct EditorFind {
    pub query: String,
    pub opts: SearchOpts,
    pub last_rect: Rect,
    pub match_count: usize,
    pub match_index: Option<usize>,
    /// Set when `match_count` covers only part of the searched content, so
    /// the bar says `N+` rather than presenting a partial count as a total.
    /// Only the windowed log view (#257) sets it true: every other surface
    /// counts an in-memory buffer exhaustively.
    ///
    /// Written only through [`EditorFind::set_match_count`], so a surface
    /// that counts exhaustively cannot inherit a `N+` left behind by a log
    /// tab searched a moment earlier.
    count_truncated: bool,
    /// Second input row (VS Code's expanded find widget, `Cmd+Opt+F`).
    pub replace_visible: bool,
    pub replace: String,
    pub focus: FindField,
    /// The whole query is selected (#1278): the seed Ctrl+F put in, or
    /// Ctrl+A. The next typed character or paste replaces it and Backspace
    /// clears it, as in VS Code; a caret key just drops the selection.
    pub query_selected: bool,
    /// Find in Selection (#1577): the `(start, end)` positions, row and
    /// char column, the matches are limited to. `Alt+L` sets it from the
    /// selection and clears it again.
    pub in_selection: Option<FindRange>,
}

/// A buffer range as `(start, end)` `(row, char column)` positions.
pub type FindRange = ((usize, usize), (usize, usize));

/// `range` held inside `lines`: rows past the end come back to the last
/// line, columns past a line's end to that end, and the ends in order.
pub fn clamp_range(lines: &[String], range: FindRange) -> FindRange {
    let last = lines.len().saturating_sub(1);
    let fit = |(r, c): (usize, usize)| {
        let r = r.min(last);
        let len = lines.get(r).map_or(0, |l| l.chars().count());
        (r, c.min(len))
    };
    let (a, b) = (fit(range.0), fit(range.1));
    if a <= b { (a, b) } else { (b, a) }
}

/// The char columns of row `row` a find limited to `range` covers
/// (#1577): `None` with no range (the whole line), `Some((0, 0))` for a row
/// outside it.
pub fn find_window(range: Option<FindRange>, row: usize) -> Option<(usize, usize)> {
    let ((r0, c0), (r1, c1)) = range?;
    if row < r0 || row > r1 {
        return Some((0, 0));
    }
    Some((
        if row == r0 { c0 } else { 0 },
        if row == r1 { c1 } else { usize::MAX },
    ))
}

/// Whether the match at `(col, len)` lies wholly inside `window`.
fn in_window(window: Option<(usize, usize)>, col: usize, len: usize) -> bool {
    window.is_none_or(|(lo, hi)| col >= lo && col + len <= hi)
}

impl EditorFind {
    /// A fresh find bar over `query` with `opts`, counting nothing yet.
    ///
    /// A constructor rather than a struct literal so the count and its
    /// truncation flag stay unreachable from outside: they are one fact,
    /// written only through [`EditorFind::set_match_count`].
    pub fn new(query: String, opts: SearchOpts) -> Self {
        Self {
            query,
            opts,
            ..Default::default()
        }
    }

    /// Record a match count together with whether it covers everything that
    /// was searched.
    ///
    /// The two travel together because they are one fact. Setting the count
    /// alone is what let a log tab's budgeted `N+` survive onto the next
    /// text tab, where the count was exact and the `+` was a lie.
    pub fn set_match_count(&mut self, count: usize, truncated: bool) {
        self.match_count = count;
        self.count_truncated = truncated;
    }

    /// Whether the last count stopped short of the whole content.
    pub fn count_truncated(&self) -> bool {
        self.count_truncated
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchPos {
    pub row: usize,
    pub col_chars: usize,
    pub len_chars: usize,
}

pub fn line_matches(line: &str, opts: SearchOpts, needle: &str) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut col_chars = 0usize;
    for (chunk, is_match) in split_for_highlight(line, needle, opts) {
        let chunk_chars = chunk.chars().count();
        // Zero-width matches are invisible: nothing to paint, count, or
        // step to — the find bar's layer skips them even though the split
        // now reports them (the workspace replace preview needs them).
        if is_match && chunk_chars > 0 {
            out.push((col_chars, chunk_chars));
        }
        col_chars = col_chars.saturating_add(chunk_chars);
    }
    out
}

/// [`line_matches`] kept to those wholly inside `window` (see
/// [`find_window`]). The line is matched whole, so a word or line boundary
/// is judged on the real text, never on one the window cut (#1577).
pub fn line_matches_in(
    line: &str,
    opts: SearchOpts,
    needle: &str,
    window: Option<(usize, usize)>,
) -> Vec<(usize, usize)> {
    let mut out = line_matches(line, opts, needle);
    out.retain(|&(col, len)| in_window(window, col, len));
    out
}

pub fn count_matches(lines: &[String], needle: &str, opts: SearchOpts) -> usize {
    count_matches_in(lines, needle, opts, None)
}

/// [`count_matches`] limited to `range` of `lines` (Find in Selection).
pub fn count_matches_in(
    lines: &[String],
    needle: &str,
    opts: SearchOpts,
    range: Option<FindRange>,
) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut total = 0usize;
    for (r, line) in lines.iter().enumerate() {
        total =
            total.saturating_add(line_matches_in(line, opts, needle, find_window(range, r)).len());
    }
    total
}

pub fn find_next_match(
    lines: &[String],
    needle: &str,
    opts: SearchOpts,
    from_row: usize,
    from_col_chars: usize,
    skip_current: bool,
) -> Option<MatchPos> {
    find_next_match_in(
        lines,
        needle,
        opts,
        None,
        from_row,
        from_col_chars,
        skip_current,
    )
}

/// [`find_next_match`] over the matches inside `range` of `lines` only.
pub fn find_next_match_in(
    lines: &[String],
    needle: &str,
    opts: SearchOpts,
    range: Option<FindRange>,
    from_row: usize,
    from_col_chars: usize,
    skip_current: bool,
) -> Option<MatchPos> {
    if needle.is_empty() || lines.is_empty() {
        return None;
    }
    let n = lines.len();
    for delta in 0..=n {
        let row = (from_row + delta) % n;
        let matches = line_matches_in(&lines[row], opts, needle, find_window(range, row));
        for (col, len) in matches {
            if delta == 0 && row == from_row {
                let cmp = if skip_current {
                    col <= from_col_chars
                } else {
                    col < from_col_chars
                };
                if cmp {
                    continue;
                }
            }
            return Some(MatchPos {
                row,
                col_chars: col,
                len_chars: len,
            });
        }
        if delta == n {
            break;
        }
    }
    None
}

pub fn find_prev_match(
    lines: &[String],
    needle: &str,
    opts: SearchOpts,
    from_row: usize,
    from_col_chars: usize,
    skip_current: bool,
) -> Option<MatchPos> {
    find_prev_match_in(
        lines,
        needle,
        opts,
        None,
        from_row,
        from_col_chars,
        skip_current,
    )
}

/// [`find_prev_match`] over the matches inside `range` of `lines` only.
pub fn find_prev_match_in(
    lines: &[String],
    needle: &str,
    opts: SearchOpts,
    range: Option<FindRange>,
    from_row: usize,
    from_col_chars: usize,
    skip_current: bool,
) -> Option<MatchPos> {
    if needle.is_empty() || lines.is_empty() {
        return None;
    }
    let n = lines.len();
    for delta in 0..=n {
        let row = (from_row + n - (delta % n)) % n;
        let matches = line_matches_in(&lines[row], opts, needle, find_window(range, row));
        let mut best: Option<(usize, usize)> = None;
        for (col, len) in matches {
            if delta == 0 && row == from_row {
                let cmp = if skip_current {
                    col >= from_col_chars
                } else {
                    col > from_col_chars
                };
                if cmp {
                    continue;
                }
            }
            best = Some((col, len));
        }
        if let Some((col, len)) = best {
            return Some(MatchPos {
                row,
                col_chars: col,
                len_chars: len,
            });
        }
        if delta == n {
            break;
        }
    }
    None
}

/// Compile the find query + `SearchOpts` into the pattern
/// `split_for_highlight` matches with (no trimming — a query with leading /
/// trailing spaces matches literally, like the highlighter), look-around and
/// backreferences included (#1552), so replace touches precisely what the
/// find bar shows.
fn build_find_regex(needle: &str, opts: SearchOpts) -> Option<Pattern> {
    if needle.is_empty() {
        return None;
    }
    let body = if opts.use_regex {
        needle.to_string()
    } else {
        regex::escape(needle)
    };
    Pattern::for_find(&body, opts)
}

/// Expand what the match at `(col_chars, len_chars)` in `line` should be
/// replaced with. Literal mode returns `replacement` verbatim; regex mode
/// expands `$1` / `${name}` capture references against that exact match,
/// like VS Code. Returns `None` when no match starts at `col_chars` with
/// that length (a stale position), so callers never splice blindly.
pub fn expand_replacement(
    line: &str,
    col_chars: usize,
    len_chars: usize,
    needle: &str,
    replacement: &str,
    opts: SearchOpts,
) -> Option<String> {
    // Literal mode asks the find bar's own matcher, which is what put the
    // match on screen. Its whole-word test accepts `$x` in `a $x b`, where
    // the regex's `\b` (between a space and `$`) never matches, so Replace
    // on `$var` or `@user` jumped without ever replacing.
    if !opts.use_regex {
        return line_matches(line, opts, needle)
            .contains(&(col_chars, len_chars))
            .then(|| shaped(line, col_chars, len_chars, replacement.to_string(), opts));
    }
    let re = build_find_regex(needle, opts)?;
    for (start, end) in re.spans(line) {
        if line[..start].chars().count() != col_chars {
            continue;
        }
        if line[start..end].chars().count() != len_chars {
            return None;
        }
        return re
            .expand_at(line, start, end, &unescape_escapes(replacement))
            .map(|dst| shaped(line, col_chars, len_chars, dst, opts));
    }
    None
}

/// `replacement` in the case of the `len_chars` characters at `col_chars`
/// when Preserve Case is on (#1612), as typed otherwise.
fn shaped(
    line: &str,
    col_chars: usize,
    len_chars: usize,
    replacement: String,
    opts: SearchOpts,
) -> String {
    if !opts.preserve_case {
        return replacement;
    }
    let matched: String = line.chars().skip(col_chars).take(len_chars).collect();
    crate::widgets::search::preserve_case(&matched, &replacement)
}

/// Translate VS Code's regex-replacement escapes (`\n`, `\t`, `\r`, `\\`)
/// into their characters, unknown escapes passing through; the `$`
/// references are read against the pattern when `Pattern::expand_at`
/// expands them. Literal (non-regex) replacements are never unescaped.
fn unescape_escapes(replacement: &str) -> String {
    let mut out = String::with_capacity(replacement.len());
    let mut it = replacement.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// `replacement` shaped after `matched` under Preserve Case (#1612).
fn case_of(matched: &str, replacement: &str, opts: SearchOpts) -> String {
    if opts.preserve_case {
        crate::widgets::search::preserve_case(matched, replacement)
    } else {
        replacement.to_string()
    }
}

/// Replace every match of `needle` across `lines` — exactly the matches
/// the find bar counts, paints, and navigates: all of them enumerate via
/// [`split_for_highlight`], so Replace All can never rewrite text the bar
/// refused to show (zero-width regex matches, case foldings the literal
/// scanner cannot map back into the line). Regex replacements expand `$n`
/// capture references and VS Code's `\n`/`\t` escapes; a replacement
/// newline splits the line. Returns `None` when the needle is empty or
/// the pattern can't compile, so callers leave the buffer untouched.
#[cfg(test)]
pub fn replace_all_in_lines(
    lines: &[String],
    needle: &str,
    replacement: &str,
    opts: SearchOpts,
) -> Option<(Vec<String>, usize)> {
    replace_all_in_lines_in(lines, needle, replacement, opts, None)
}

/// [`replace_all_in_lines`] touching only the matches wholly inside `range`
/// of `lines`, found on the whole lines (#1577).
pub fn replace_all_in_lines_in(
    lines: &[String],
    needle: &str,
    replacement: &str,
    opts: SearchOpts,
    range: Option<FindRange>,
) -> Option<(Vec<String>, usize)> {
    if needle.is_empty() {
        return None;
    }
    let re = if opts.use_regex {
        Some(build_find_regex(needle, opts)?)
    } else {
        None
    };
    let unescaped;
    let replacement = if opts.use_regex {
        unescaped = unescape_escapes(replacement);
        unescaped.as_str()
    } else {
        replacement
    };
    let mut total = 0usize;
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for (r, line) in lines.iter().enumerate() {
        let window = find_window(range, r);
        let segs = split_for_highlight(line, needle, opts);
        if !segs.iter().any(|(_, is_match)| *is_match) {
            out.push(line.clone());
            continue;
        }
        let mut new = String::new();
        // Byte offset of the current segment in `line`, for captures_at.
        let mut pos = 0usize;
        // Char column of the current segment, for the window test.
        let mut col = 0usize;
        for (chunk, is_match) in &segs {
            let chunk_chars = chunk.chars().count();
            let inside = in_window(window, col, chunk_chars);
            col += chunk_chars;
            if !is_match || chunk.is_empty() || !inside {
                // Zero-width matches stay unreplaced in the find bar (the
                // highlighter shows nothing there); the empty chunk still
                // contributes no text.
                new.push_str(chunk);
            } else {
                total += 1;
                match &re {
                    // The segment came from this same pattern (the two
                    // builders emit identical regexes), so re-running it at
                    // the segment's offset recovers the capture groups. The
                    // guard falls back to the original text rather than
                    // ever splicing a mismatched span.
                    Some(re) => match re.expand_at(line, pos, pos + chunk.len(), replacement) {
                        Some(expanded) => new.push_str(&case_of(chunk, &expanded, opts)),
                        None => new.push_str(chunk),
                    },
                    // Literal replacement goes in verbatim: no capture
                    // references, no escapes.
                    None => new.push_str(&case_of(chunk, replacement, opts)),
                }
            }
            pos += chunk.len();
        }
        out.extend(
            crate::widgets::editor::normalize_newlines(&new)
                .split('\n')
                .map(str::to_string),
        );
    }
    Some((out, total))
}

/// 1-based "N of M" for the current cursor + first match at-or-after.
pub fn match_index_at(
    lines: &[String],
    needle: &str,
    opts: SearchOpts,
    target_row: usize,
    target_col_chars: usize,
) -> Option<usize> {
    match_index_at_in(lines, needle, opts, None, target_row, target_col_chars)
}

/// [`match_index_at`] numbering only the matches inside `range` of `lines`.
pub fn match_index_at_in(
    lines: &[String],
    needle: &str,
    opts: SearchOpts,
    range: Option<FindRange>,
    target_row: usize,
    target_col_chars: usize,
) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let mut idx = 0usize;
    for (r, line) in lines.iter().enumerate() {
        for (col, _len) in line_matches_in(line, opts, needle, find_window(range, r)) {
            idx += 1;
            if r == target_row && col == target_col_chars {
                return Some(idx);
            }
            if r > target_row {
                return None;
            }
        }
    }
    None
}

pub fn render_editor_find(
    state: &mut EditorFind,
    editor_area: Rect,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
) {
    if editor_area.width < 30 || editor_area.height < 3 {
        state.last_rect = Rect::default();
        return;
    }
    let width = 48u16.min(editor_area.width.saturating_sub(2));
    let height = if state.replace_visible && editor_area.height >= 4 {
        4u16
    } else {
        3u16
    };
    let x = editor_area
        .x
        .saturating_add(editor_area.width.saturating_sub(width).saturating_sub(1));
    let y = editor_area.y.saturating_add(1);
    let rect = Rect {
        x,
        y,
        width,
        height,
    };
    state.last_rect = rect;

    Widget::render(Clear, rect, buf);
    // A budgeted count (the windowed log view) reports what it could see
    // plus a `+`, so a partial total is never shown as a whole one.
    let total_txt = if state.count_truncated() {
        format!("{}+", state.match_count)
    } else {
        state.match_count.to_string()
    };
    let title = match (state.match_count, state.match_index) {
        (0, _) if state.query.is_empty() => String::from(" Find "),
        // A sweep can stop short because it ran out of budget OR because
        // bytes could not be read, and the user cannot act on the
        // difference: both mean "not the whole file". Naming a byte figure
        // would be wrong in the second case, so the arm says what is true of
        // both.
        (0, _) if state.count_truncated() => {
            // Kept under the bar's 48-cell width: a longer string is
            // truncated mid-word by the block title.
            String::from(" Find: no match in the part searched ")
        }
        (0, _) => " Find — No results ".to_string(),
        (_, Some(idx)) => format!(" Find — {idx} of {total_txt} "),
        (_, None) => format!(" Find — {total_txt} matches "),
    };
    let title = Span::styled(
        title,
        Style::default()
            .fg(theme.ui(Color::Rgb(0xff, 0xff, 0xff)))
            .add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))))
        .title(title.clone())
        .style(Style::default().bg(theme.ui(Color::Rgb(0x16, 0x18, 0x1f))));
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    Widget::render(block, rect, buf);
    // Black theme: gradient border over the solid one, then re-stamp the title.
    if theme.gradient() {
        crate::gradient::paint_gradient_box(buf, rect);
        buf.set_span(rect.x + 1, rect.y, &title, title.width() as u16);
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let query_selected = state.query_selected && state.focus == FindField::Query;
    let input_row = |marker: &str, text: &str, focused: bool, selected: bool| {
        let text_style = if selected {
            Style::default()
                .fg(theme.ui(Color::Rgb(0xff, 0xff, 0xff)))
                .bg(theme.ui(Color::Rgb(0x26, 0x4f, 0x78)))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
                .add_modifier(Modifier::BOLD)
        };
        let mut spans = vec![
            Span::styled(
                marker.to_string(),
                Style::default().fg(theme.ui(Color::Rgb(0x88, 0xc0, 0xd0))),
            ),
            Span::styled(text.to_string(), text_style),
        ];
        if focused {
            spans.push(Span::styled(
                "_",
                Style::default()
                    .fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
                    .add_modifier(Modifier::SLOW_BLINK),
            ));
        }
        Line::from(spans)
    };
    // Find in Selection (#1577): `≡` at the query row's right edge, lit
    // while the find is limited to the selection.
    let paint_chip = |buf: &mut Buffer| {
        if inner.width < 4 {
            return;
        }
        let style = if state.in_selection.is_some() {
            Style::default()
                .fg(theme.ui(Color::Rgb(0xff, 0xff, 0xff)))
                .bg(theme.ui(Color::Rgb(0x26, 0x4f, 0x78)))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.ui(Color::Rgb(0x6b, 0x73, 0x85)))
        };
        buf.set_string(inner.x + inner.width - 2, inner.y, "≡", style);
    };
    if state.replace_visible && inner.height >= 2 {
        let rows = vec![
            input_row(
                "> ",
                &state.query,
                state.focus == FindField::Query,
                query_selected,
            ),
            input_row(
                "⤷ ",
                &state.replace,
                state.focus == FindField::Replace,
                false,
            ),
        ];
        Widget::render(Paragraph::new(rows), inner, buf);
    } else {
        let prompt = input_row("> ", &state.query, true, query_selected);
        Widget::render(Paragraph::new(prompt), inner, buf);
    }
    paint_chip(buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_ascii_case_folds_neither_panic_nor_shift_the_replacement() {
        let opts = SearchOpts::default();
        let lines = vec![String::from("ẞ foo İ"), String::from("\u{212A} foo İİ")];
        assert_eq!(count_matches(&lines, "foo", opts), 2);
        let (out, n) = replace_all_in_lines(&lines, "foo", "X", opts).unwrap();
        assert_eq!(n, 2);
        assert_eq!(out, vec!["ẞ X İ".to_string(), "\u{212A} X İİ".to_string()]);
    }

    #[test]
    fn a_numbered_group_followed_by_a_word_character_still_expands() {
        let opts = SearchOpts {
            use_regex: true,
            ..SearchOpts::default()
        };
        let (out, _) =
            replace_all_in_lines(&[String::from("foo bar")], "(foo)", "$1_old", opts).unwrap();
        assert_eq!(out, vec!["foo_old bar".to_string()]);
        let (out, _) = replace_all_in_lines(&[String::from("foo")], "foo", "$$1", opts).unwrap();
        assert_eq!(out, vec!["$1".to_string()]);
    }

    fn lines(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    /// #1577: a range limits the count to the matches inside it.
    #[test]
    fn a_range_limits_the_count_to_its_matches() {
        let src = lines(&[
            "def a(x):",
            "    return x + 1",
            "def b(x):",
            "    return x * 2",
        ]);
        let x = SearchOpts::default();
        assert_eq!(count_matches_in(&src, "x", x, Some(((2, 0), (3, 16)))), 2);
        assert_eq!(count_matches_in(&src, "x", x, Some(((1, 11), (1, 12)))), 1);
        assert_eq!(count_matches_in(&src, "x", x, Some(((0, 6), (1, 4)))), 1);
        assert_eq!(count_matches_in(&src, "x", x, None), 4);
    }

    /// #1577: matches are found on the whole lines, so the range's edges are
    /// no word or line boundary: selecting `foo` out of `foobar` finds no
    /// whole word `foo` and no `^foo$`, as the highlighter shows none.
    #[test]
    fn a_range_edge_is_not_a_word_or_line_boundary() {
        let src = lines(&["foobar", "foo"]);
        let range = Some(((0, 0), (0, 3)));
        let word = SearchOpts {
            whole_word: true,
            ..SearchOpts::default()
        };
        assert_eq!(count_matches_in(&src, "foo", word, range), 0);
        let re = SearchOpts {
            use_regex: true,
            ..SearchOpts::default()
        };
        assert_eq!(count_matches_in(&src, "^foo$", re, range), 0);
        assert_eq!(
            replace_all_in_lines_in(&src, "foo", "X", word, range).map(|(_, n)| n),
            Some(0)
        );
        assert_eq!(
            find_next_match_in(&src, "foo", word, range, 0, 0, false),
            None
        );
        // The same text selected whole is a word and a line.
        assert_eq!(
            count_matches_in(&src, "foo", word, Some(((1, 0), (1, 3)))),
            1
        );
    }

    /// #1577: Replace All in a range touches the matches inside it only and
    /// keeps the text either side as it was.
    #[test]
    fn a_range_replace_keeps_the_text_around_it() {
        let src = lines(&["x = x", "y = x + x", "z = x"]);
        let (out, n) = replace_all_in_lines_in(
            &src,
            "x",
            "long",
            SearchOpts::default(),
            Some(((0, 4), (1, 5))),
        )
        .unwrap();
        assert_eq!(n, 2);
        assert_eq!(out, lines(&["x = long", "y = long + x", "z = x"]));
    }

    /// Negative (#1577): a range past the buffer is held inside it and an
    /// inverted one is put in order, rather than panicking.
    #[test]
    fn a_stale_range_is_clamped_not_trusted() {
        let src = lines(&["ab", "cd"]);
        assert_eq!(clamp_range(&src, ((9, 9), (0, 1))), ((0, 1), (1, 2)));
    }

    #[test]
    fn replace_all_touches_exactly_what_the_highlighter_shows() {
        // The find bar's count, paint, and navigation all enumerate via
        // split_for_highlight; Replace All must not rewrite text they
        // never showed. Zero-width regex matches are skipped by the
        // highlighter, and used to be rewritten anyway.
        let re_opts = SearchOpts {
            use_regex: true,
            ..SearchOpts::default()
        };
        let (out, n) = replace_all_in_lines(&lines(&["banana"]), "a*", "-", re_opts).unwrap();
        assert_eq!(
            (out, n),
            (lines(&["b-n-n-"]), 3),
            "zero-width matches are not replaced"
        );
        let (out, n) = replace_all_in_lines(&lines(&["banana"]), "^", "// ", re_opts).unwrap();
        assert_eq!(
            (out, n),
            (lines(&["banana"]), 0),
            "an all-zero-width pattern replaces nothing"
        );
        let lit = SearchOpts::default();
        // U+212A KELVIN SIGN: lowercasing changes its byte length, so the
        // line is matched on its own bytes by case folding; the highlighter
        // shows the match and Replace All rewrites exactly that.
        let kelvin = "300\u{212A}";
        let segs = split_for_highlight(kelvin, "k", lit);
        assert!(segs.iter().any(|(t, m)| *m && t == "\u{212A}"), "{segs:?}");
        let (out, n) = replace_all_in_lines(&lines(&[kelvin]), "k", "x", lit).unwrap();
        assert_eq!((out, n), (lines(&["300x"]), 1));
    }

    #[test]
    fn regex_replacements_interpret_backslash_escapes() {
        // VS Code's regex replace turns \n and \t into real characters; a
        // replacement newline splits the line. Literal mode stays verbatim.
        let re_opts = SearchOpts {
            use_regex: true,
            ..SearchOpts::default()
        };
        let (out, n) = replace_all_in_lines(&lines(&["abc"]), "b", r"x\ny", re_opts).unwrap();
        assert_eq!((out, n), (lines(&["ax", "yc"]), 1));
        let (out, _) = replace_all_in_lines(&lines(&["abc"]), "b", r"x\ty", re_opts).unwrap();
        assert_eq!(out, lines(&["ax\tyc"]));
        let (out, _) = replace_all_in_lines(&lines(&["abc"]), "b", r"x\\ny", re_opts).unwrap();
        assert_eq!(
            out,
            lines(&[r"ax\nyc"]),
            "an escaped backslash stays literal"
        );
        let (out, _) =
            replace_all_in_lines(&lines(&["abc"]), "b", r"x\ny", SearchOpts::default()).unwrap();
        assert_eq!(
            out,
            lines(&[r"ax\nyc"]),
            "literal mode inserts the text verbatim"
        );
    }

    #[test]
    fn carriage_returns_in_a_replacement_become_real_line_breaks() {
        // The `\r` escape (and a pasted CRLF) must not leave a bare CR
        // inside a buffer line: a CRLF file would then save `\r\r\n`, and
        // every consumer counting lines would disagree with the text.
        let re_opts = SearchOpts {
            use_regex: true,
            ..SearchOpts::default()
        };
        let (out, n) = replace_all_in_lines(&lines(&["abc"]), "b", r"x\r\ny", re_opts).unwrap();
        assert_eq!(
            (out, n),
            (lines(&["ax", "yc"]), 1),
            "CRLF splits once, no stray CR"
        );
        let (out, _) = replace_all_in_lines(&lines(&["abc"]), "b", r"x\ry", re_opts).unwrap();
        assert_eq!(out, lines(&["ax", "yc"]), "a lone CR is a line break too");
        for line in replace_all_in_lines(&lines(&["abc"]), "b", r"x\r\ny", re_opts)
            .unwrap()
            .0
        {
            assert!(
                !line.contains('\r'),
                "no CR may survive in a line: {line:?}"
            );
        }
    }

    #[test]
    fn count_matches_returns_zero_for_empty_needle() {
        let buf = lines(&["alpha", "beta"]);
        assert_eq!(count_matches(&buf, "", SearchOpts::default()), 0);
    }

    #[test]
    fn count_matches_counts_every_occurrence_across_lines() {
        let buf = lines(&["alpha alpha", "beta", "alpha gamma"]);
        assert_eq!(count_matches(&buf, "alpha", SearchOpts::default()), 3);
    }

    #[test]
    fn find_next_match_returns_first_match_after_cursor_on_same_line() {
        let buf = lines(&["the quick brown fox"]);
        let m = find_next_match(&buf, "o", SearchOpts::default(), 0, 0, false).unwrap();
        assert_eq!(m.row, 0);
        assert_eq!(m.col_chars, 12);
    }

    #[test]
    fn find_next_match_wraps_around_to_top_when_cursor_is_past_the_last_match() {
        let buf = lines(&["alpha", "beta", "alpha"]);
        let m = find_next_match(&buf, "alpha", SearchOpts::default(), 2, 5, true).unwrap();
        assert_eq!(m.row, 0);
        assert_eq!(m.col_chars, 0);
    }

    #[test]
    fn find_prev_match_returns_closest_match_before_cursor_not_the_leftmost() {
        let buf = lines(&["alpha beta alpha"]);
        let m = find_prev_match(&buf, "alpha", SearchOpts::default(), 0, 12, true).unwrap();
        assert_eq!(m.row, 0);
        assert_eq!(
            m.col_chars, 11,
            "Shift+Enter must walk the user back match-by-match — the nearest match before the cursor (col 11) wins over the leftmost one"
        );
    }

    #[test]
    fn find_prev_match_wraps_backwards_when_cursor_is_before_every_match() {
        let buf = lines(&["alpha", "beta", "alpha"]);
        let m = find_prev_match(&buf, "alpha", SearchOpts::default(), 0, 0, true).unwrap();
        assert_eq!(m.row, 2);
        assert_eq!(m.col_chars, 0);
    }

    #[test]
    fn find_returns_none_when_needle_is_absent() {
        let buf = lines(&["alpha", "beta"]);
        assert!(find_next_match(&buf, "zzz", SearchOpts::default(), 0, 0, true).is_none());
        assert!(find_prev_match(&buf, "zzz", SearchOpts::default(), 0, 0, true).is_none());
    }

    #[test]
    fn expand_replacement_literal_returns_replacement_verbatim() {
        let got = expand_replacement(
            "let alpha = 1;",
            4,
            5,
            "alpha",
            "beta",
            SearchOpts::default(),
        );
        assert_eq!(got.as_deref(), Some("beta"));
    }

    #[test]
    fn expand_replacement_literal_keeps_dollar_verbatim() {
        // A `$` in a literal replacement is inserted as-is, never parsed as
        // a capture reference.
        let got = expand_replacement("price", 0, 5, "price", "$total", SearchOpts::default());
        assert_eq!(got.as_deref(), Some("$total"));
    }

    #[test]
    fn expand_replacement_regex_expands_capture_groups() {
        let opts = SearchOpts {
            use_regex: true,
            ..SearchOpts::default()
        };
        let got = expand_replacement("foo(1, 2)", 0, 9, r"foo\((\d), (\d)\)", "foo($2, $1)", opts);
        assert_eq!(got.as_deref(), Some("foo(2, 1)"));
    }

    /// #1552: a backreference the find bar now shows is replaced too, by
    /// Replace and Replace All, `$1` and escapes included.
    #[test]
    fn replace_runs_a_backreference_the_find_bar_shows() {
        let opts = SearchOpts {
            use_regex: true,
            ..SearchOpts::default()
        };
        let got = expand_replacement("saw the the cat", 4, 7, r"(\w+) \1", "[$1]", opts);
        assert_eq!(got.as_deref(), Some("[the]"));
        let buf = lines(&["saw the the cat", "ok ok", "one"]);
        let (new_lines, n) = replace_all_in_lines(&buf, r"(\w+) \1", r"$1\t", opts).unwrap();
        assert_eq!(n, 2);
        assert_eq!(new_lines, lines(&["saw the\t cat", "ok\t", "one"]));
    }

    #[test]
    fn expand_replacement_returns_none_for_a_stale_position() {
        // No match starts at col 1 — a stale MatchPos must never splice.
        let got = expand_replacement("alpha", 1, 5, "alpha", "beta", SearchOpts::default());
        assert_eq!(got, None);
    }

    #[test]
    fn expand_replacement_takes_a_whole_word_match_the_find_bar_shows() {
        let opts = SearchOpts {
            whole_word: true,
            ..SearchOpts::default()
        };
        assert_eq!(line_matches("a $x b", opts, "$x"), vec![(2, 2)]);
        let got = expand_replacement("a $x b", 2, 2, "$x", "$y", opts);
        assert_eq!(got.as_deref(), Some("$y"));
    }

    #[test]
    fn expand_replacement_matches_case_insensitively_by_default() {
        let got = expand_replacement("Alpha beta", 0, 5, "alpha", "gamma", SearchOpts::default());
        assert_eq!(got.as_deref(), Some("gamma"));
    }

    #[test]
    fn replace_all_in_lines_replaces_every_match_and_counts() {
        let buf = lines(&["alpha alpha", "beta", "alpha"]);
        let (new_lines, n) =
            replace_all_in_lines(&buf, "alpha", "x", SearchOpts::default()).unwrap();
        assert_eq!(n, 3);
        assert_eq!(new_lines, lines(&["x x", "beta", "x"]));
    }

    /// #1612: the editor's Replace / Replace All keep each match's case
    /// under Preserve Case, literal and regex alike.
    #[test]
    fn find_bar_replace_preserves_each_match_s_case() {
        let opts = SearchOpts {
            preserve_case: true,
            ..SearchOpts::default()
        };
        let buf = lines(&["UserService USER_TABLE get_user"]);
        let (new_lines, n) = replace_all_in_lines(&buf, "user", "account", opts).unwrap();
        assert_eq!(n, 3);
        assert_eq!(
            new_lines,
            lines(&["AccountService ACCOUNT_TABLE get_account"])
        );
        let got = expand_replacement("x USER_TABLE", 2, 4, "user", "account", opts);
        assert_eq!(got.as_deref(), Some("ACCOUNT"));
        let re = SearchOpts {
            use_regex: true,
            ..opts
        };
        let (new_lines, _) =
            replace_all_in_lines(&lines(&["User_1"]), r"user_(\d)", "account_$1", re).unwrap();
        assert_eq!(new_lines, lines(&["Account_1"]));
        let got = expand_replacement("User_1", 0, 6, r"user_(\d)", "account_$1", re);
        assert_eq!(got.as_deref(), Some("Account_1"));
    }

    /// #1612 negative: Preserve Case off, and a stale position, behave as
    /// before.
    #[test]
    fn find_bar_replace_without_preserve_case_is_as_typed() {
        let buf = lines(&["UserService"]);
        let (new_lines, _) =
            replace_all_in_lines(&buf, "user", "account", SearchOpts::default()).unwrap();
        assert_eq!(new_lines, lines(&["accountService"]));
        let opts = SearchOpts {
            preserve_case: true,
            ..SearchOpts::default()
        };
        assert_eq!(
            expand_replacement("UserService", 3, 4, "user", "x", opts),
            None
        );
    }

    #[test]
    fn replace_all_in_lines_honours_whole_word() {
        let buf = lines(&["cat catalog cat"]);
        let opts = SearchOpts {
            whole_word: true,
            ..SearchOpts::default()
        };
        let (new_lines, n) = replace_all_in_lines(&buf, "cat", "dog", opts).unwrap();
        assert_eq!(n, 2, "'catalog' must survive a whole-word replace");
        assert_eq!(new_lines, lines(&["dog catalog dog"]));
    }

    #[test]
    fn replace_all_in_lines_regex_expands_captures_per_match() {
        let buf = lines(&["a=1 b=2"]);
        let opts = SearchOpts {
            use_regex: true,
            ..SearchOpts::default()
        };
        let (new_lines, n) = replace_all_in_lines(&buf, r"(\w)=(\d)", "$2:$1", opts).unwrap();
        assert_eq!(n, 2);
        assert_eq!(new_lines, lines(&["1:a 2:b"]));
    }

    /// #1399: the editor's find bar keeps a `$NAME` that is no group as
    /// text, in Replace and Replace All, while `$1` still expands.
    #[test]
    fn a_dollar_name_that_is_no_group_stays_literal_in_the_editor() {
        let opts = SearchOpts {
            use_regex: true,
            ..SearchOpts::default()
        };
        let got = expand_replacement("cp /opt/app", 3, 8, r"/opt/(\S+)", "$PREFIX/$1", opts);
        assert_eq!(got.as_deref(), Some("$PREFIX/app"));
        let buf = lines(&["cp /opt/app", "cp /opt/app.conf"]);
        let (new_lines, n) =
            replace_all_in_lines(&buf, r"/opt/(\S+)", "${PREFIX}/$1", opts).unwrap();
        assert_eq!(n, 2);
        assert_eq!(
            new_lines,
            lines(&["cp ${PREFIX}/app", "cp ${PREFIX}/app.conf"])
        );
    }

    /// #1636: the editor's find bar keeps a numbered reference with no
    /// group as text, in Replace and Replace All.
    #[test]
    fn a_numbered_reference_with_no_group_stays_literal_in_the_editor() {
        let opts = SearchOpts {
            use_regex: true,
            ..SearchOpts::default()
        };
        let got = expand_replacement("apple", 0, 5, "(apple)", "v$10 costs $5", opts);
        assert_eq!(got.as_deref(), Some("vapple0 costs $5"));
        let buf = lines(&["apple", "apple pie"]);
        let (new_lines, n) = replace_all_in_lines(&buf, "(apple)", "$1 costs $5", opts).unwrap();
        assert_eq!(n, 2);
        assert_eq!(new_lines, lines(&["apple costs $5", "apple costs $5 pie"]));
    }

    #[test]
    fn replace_all_in_lines_returns_none_for_empty_needle() {
        let buf = lines(&["alpha"]);
        assert!(replace_all_in_lines(&buf, "", "x", SearchOpts::default()).is_none());
    }

    #[test]
    fn case_sensitive_opt_distinguishes_cases() {
        let buf = lines(&["Alpha alpha"]);
        let opts = SearchOpts {
            case_sensitive: true,
            ..SearchOpts::default()
        };
        let m = find_next_match(&buf, "alpha", opts, 0, 0, false).unwrap();
        assert_eq!(
            m.col_chars, 6,
            "case-sensitive 'alpha' must skip the leading 'Alpha'"
        );
    }
}
