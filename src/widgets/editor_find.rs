use crate::widgets::search::{SearchOpts, split_for_highlight};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

#[derive(Default)]
pub struct EditorFind {
    pub query: String,
    pub opts: SearchOpts,
    pub last_rect: Rect,
    pub match_count: usize,
    pub match_index: Option<usize>,
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

pub fn render_editor_find(state: &mut EditorFind, editor_area: Rect, buf: &mut Buffer) {
    if editor_area.width < 30 || editor_area.height < 3 {
        state.last_rect = Rect::default();
        return;
    }
    let width = 48u16.min(editor_area.width.saturating_sub(2));
    let height = 3u16;
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
        (0, _) => format!(" Find — No results "),
        (total, Some(idx)) => format!(" Find — {idx} of {total} "),
        (total, None) => format!(" Find — {total} matches "),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Rgb(0xff, 0xff, 0xff))
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Rgb(0x16, 0x18, 0x1f)));
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    Widget::render(block, rect, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let prompt = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Rgb(0x88, 0xc0, 0xd0))),
        Span::styled(
            state.query.clone(),
            Style::default()
                .fg(Color::Rgb(0xec, 0xef, 0xf4))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "_",
            Style::default()
                .fg(Color::Rgb(0xec, 0xef, 0xf4))
                .add_modifier(Modifier::SLOW_BLINK),
        ),
    ]);
    Widget::render(Paragraph::new(prompt), inner, buf);
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
