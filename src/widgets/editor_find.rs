use crate::widgets::search::{SearchOpts, split_for_highlight};
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
    /// Second input row (VS Code's expanded find widget, `Cmd+Opt+F`).
    pub replace_visible: bool,
    pub replace: String,
    pub focus: FindField,
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
        if is_match {
            out.push((col_chars, chunk_chars));
        }
        col_chars = col_chars.saturating_add(chunk_chars);
    }
    out
}

pub fn count_matches(lines: &[String], needle: &str, opts: SearchOpts) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut total = 0usize;
    for line in lines {
        total = total.saturating_add(line_matches(line, opts, needle).len());
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
    if needle.is_empty() || lines.is_empty() {
        return None;
    }
    let n = lines.len();
    for delta in 0..=n {
        let row = (from_row + delta) % n;
        let matches = line_matches(&lines[row], opts, needle);
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
    if needle.is_empty() || lines.is_empty() {
        return None;
    }
    let n = lines.len();
    for delta in 0..=n {
        let row = (from_row + n - (delta % n)) % n;
        let matches = line_matches(&lines[row], opts, needle);
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

/// Compile the find query + `SearchOpts` into a `regex::Regex` mirroring
/// `split_for_highlight`'s matching exactly (no trimming — a query with
/// leading / trailing spaces matches literally, like the highlighter), so
/// replace touches precisely what the find bar shows.
fn build_find_regex(needle: &str, opts: SearchOpts) -> Option<regex::Regex> {
    if needle.is_empty() {
        return None;
    }
    let body = if opts.use_regex {
        needle.to_string()
    } else {
        regex::escape(needle)
    };
    let mut pattern = String::new();
    if !opts.case_sensitive {
        pattern.push_str("(?i)");
    }
    if opts.whole_word {
        pattern.push_str("\\b(?:");
        pattern.push_str(&body);
        pattern.push_str(")\\b");
    } else {
        pattern.push_str(&body);
    }
    regex::Regex::new(&pattern).ok()
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
    let re = build_find_regex(needle, opts)?;
    for caps in re.captures_iter(line) {
        let m = caps.get(0)?;
        let start_chars = line[..m.start()].chars().count();
        if start_chars != col_chars {
            continue;
        }
        if line[m.start()..m.end()].chars().count() != len_chars {
            return None;
        }
        if !opts.use_regex {
            return Some(replacement.to_string());
        }
        let mut dst = String::new();
        caps.expand(replacement, &mut dst);
        return Some(dst);
    }
    None
}

/// Replace every match of `needle` across `lines` (per line, exactly what
/// the highlighter shows), returning the new lines and the number of
/// matches replaced. Returns `None` when the needle is empty or the
/// pattern can't compile, so callers leave the buffer untouched.
pub fn replace_all_in_lines(
    lines: &[String],
    needle: &str,
    replacement: &str,
    opts: SearchOpts,
) -> Option<(Vec<String>, usize)> {
    let re = build_find_regex(needle, opts)?;
    let mut total = 0usize;
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for line in lines {
        let count = re.find_iter(line).count();
        if count == 0 {
            out.push(line.clone());
            continue;
        }
        total += count;
        let replaced = if opts.use_regex {
            re.replace_all(line, replacement).into_owned()
        } else {
            // Literal replacement: escape `$` so the substituted text is
            // inserted verbatim rather than parsed as a capture reference.
            let escaped = replacement.replace('$', "$$");
            re.replace_all(line, escaped.as_str()).into_owned()
        };
        out.push(replaced);
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
    if needle.is_empty() {
        return None;
    }
    let mut idx = 0usize;
    for (r, line) in lines.iter().enumerate() {
        for (col, _len) in line_matches(line, opts, needle) {
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
    gradient: bool,
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
    let title = match (state.match_count, state.match_index) {
        (0, _) if state.query.is_empty() => String::from(" Find "),
        (0, _) => " Find — No results ".to_string(),
        (total, Some(idx)) => format!(" Find — {idx} of {total} "),
        (total, None) => format!(" Find — {total} matches "),
    };
    let title = Span::styled(
        title,
        Style::default()
            .fg(Color::Rgb(0xff, 0xff, 0xff))
            .add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
        .title(title.clone())
        .style(Style::default().bg(Color::Rgb(0x16, 0x18, 0x1f)));
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    Widget::render(block, rect, buf);
    // Black theme: gradient border over the solid one, then re-stamp the title.
    if gradient {
        crate::gradient::paint_gradient_box(buf, rect);
        buf.set_span(rect.x + 1, rect.y, &title, title.width() as u16);
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let input_row = |marker: &str, text: &str, focused: bool| {
        let mut spans = vec![
            Span::styled(
                marker.to_string(),
                Style::default().fg(Color::Rgb(0x88, 0xc0, 0xd0)),
            ),
            Span::styled(
                text.to_string(),
                Style::default()
                    .fg(Color::Rgb(0xec, 0xef, 0xf4))
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if focused {
            spans.push(Span::styled(
                "_",
                Style::default()
                    .fg(Color::Rgb(0xec, 0xef, 0xf4))
                    .add_modifier(Modifier::SLOW_BLINK),
            ));
        }
        Line::from(spans)
    };
    if state.replace_visible && inner.height >= 2 {
        let rows = vec![
            input_row("> ", &state.query, state.focus == FindField::Query),
            input_row("⤷ ", &state.replace, state.focus == FindField::Replace),
        ];
        Widget::render(Paragraph::new(rows), inner, buf);
    } else {
        let prompt = input_row("> ", &state.query, true);
        Widget::render(Paragraph::new(prompt), inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
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

    #[test]
    fn expand_replacement_returns_none_for_a_stale_position() {
        // No match starts at col 1 — a stale MatchPos must never splice.
        let got = expand_replacement("alpha", 1, 5, "alpha", "beta", SearchOpts::default());
        assert_eq!(got, None);
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
