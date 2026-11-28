use crate::find::FindRange;
use crate::search::SearchOpts;
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
    /// Char index of the caret in `query` (#1729). The bar is a real input
    /// line: Left/Right/Home/End/Delete edit where the caret sits instead of
    /// only at the end. Reads go through [`EditorFind::caret`], which clamps
    /// to the query's char count.
    pub caret: usize,
    /// Char index of the caret in `replace`, the same model as `caret`.
    pub replace_caret: usize,
    /// Find in Selection (#1577): the `(start, end)` positions, row and
    /// char column, the matches are limited to. `Alt+L` sets it from the
    /// selection and clears it again.
    pub in_selection: Option<FindRange>,
}

impl EditorFind {
    /// A fresh find bar over `query` with `opts`, counting nothing yet.
    ///
    /// A constructor rather than a struct literal so the count and its
    /// truncation flag stay unreachable from outside: they are one fact,
    /// written only through [`EditorFind::set_match_count`].
    pub fn new(query: String, opts: SearchOpts) -> Self {
        let caret = query.chars().count();
        Self {
            query,
            opts,
            caret,
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

    /// The caret's char index, clamped to the query.
    pub fn caret(&self) -> usize {
        crate::widgets::line_edit::clamp(&self.query, self.caret)
    }

    /// A left click at `(col, row)`. True when it landed on the bar, which
    /// then owns it: a click on an input row focuses that row and puts its
    /// caret under the pointer, the way the row was last drawn (the origin
    /// and scroll come from the same geometry the renderer uses).
    pub fn click_at(&mut self, col: u16, row: u16) -> bool {
        let r = self.last_rect;
        if r.width < 3
            || r.height < 3
            || col < r.x
            || col >= r.x + r.width
            || row < r.y
            || row >= r.y + r.height
        {
            return false;
        }
        let inner_y = r.y + 1;
        let field = match row.checked_sub(inner_y) {
            Some(0) => FindField::Query,
            Some(1) if self.replace_visible => FindField::Replace,
            _ => return true,
        };
        let text_x = r.x + 1 + FIND_MARKER_CELLS;
        let avail = find_row_avail(r.width.saturating_sub(2));
        let cell = usize::from(col.saturating_sub(text_x));
        self.focus = field;
        match field {
            FindField::Query => {
                self.query_selected = false;
                let win = crate::widgets::line_edit::window(&self.query, self.caret(), avail);
                let at = crate::widgets::line_edit::char_at_cell(&self.query, win.start, cell);
                self.set_caret(at);
            }
            FindField::Replace => {
                let win =
                    crate::widgets::line_edit::window(&self.replace, self.replace_caret(), avail);
                let at = crate::widgets::line_edit::char_at_cell(&self.replace, win.start, cell);
                self.set_replace_caret(at);
            }
        }
        true
    }

    /// Put the caret at char index `at`, clamped to the query.
    pub fn set_caret(&mut self, at: usize) {
        self.caret = crate::widgets::line_edit::clamp(&self.query, at);
    }

    /// The replace field's caret, clamped to its text.
    pub fn replace_caret(&self) -> usize {
        crate::widgets::line_edit::clamp(&self.replace, self.replace_caret)
    }

    /// Put the replace field's caret at char index `at`, clamped.
    pub fn set_replace_caret(&mut self, at: usize) {
        self.replace_caret = crate::widgets::line_edit::clamp(&self.replace, at);
    }
}

// The caret editing the find bars share with every other input row.
pub use crate::widgets::line_edit::{caret_left, caret_right, delete_at, delete_before, insert_at};

/// Cells the marker (`> ` or `⤷ `) takes at the left of an input row.
const FIND_MARKER_CELLS: u16 = 2;

/// Cells an input row's text may use in a bar whose inner width is
/// `inner_width`: the marker and the selection chip's two cells stay free.
fn find_row_avail(inner_width: u16) -> usize {
    inner_width.saturating_sub(FIND_MARKER_CELLS + 3).max(1) as usize
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
    if theme.framed_overlays() {
        crate::widgets::surface::paint_overlay_frame(buf, rect, theme);
        buf.set_span(rect.x + 1, rect.y, &title, title.width() as u16);
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let query_selected = state.query_selected && state.focus == FindField::Query;
    // One input row: the marker, then the text with the caret drawn where it
    // sits (#1729) rather than always after the last character. A query wider
    // than the row scrolls so the caret stays in view; two cells stay free at
    // the right for the selection chip.
    let input_row = |marker: &str, text: &str, focused: bool, selected: bool, caret: usize| {
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
        let cursor_style = Style::default()
            .fg(theme.ui(Color::Rgb(0x16, 0x18, 0x1f)))
            .bg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
            .add_modifier(Modifier::SLOW_BLINK);
        let mut spans = vec![Span::styled(
            marker.to_string(),
            Style::default().fg(theme.ui(Color::Rgb(0x88, 0xc0, 0xd0))),
        )];
        let chars: Vec<char> = text.chars().collect();
        let avail = find_row_avail(inner.width);
        let win = crate::widgets::line_edit::window(text, caret, avail);
        let caret = crate::widgets::line_edit::clamp(text, caret);
        let before: String = chars[win.start..caret].iter().collect();
        if !before.is_empty() {
            spans.push(Span::styled(before, text_style));
        }
        let mut drawn = caret;
        // A selected query paints as one band, so its caret is not drawn.
        if focused && !selected {
            if caret < win.end {
                spans.push(Span::styled(chars[caret].to_string(), cursor_style));
                drawn = caret + 1;
            } else if caret >= chars.len() {
                spans.push(Span::styled("_", cursor_style));
            }
        }
        let tail: String = chars[drawn.min(win.end)..win.end].iter().collect();
        if !tail.is_empty() {
            spans.push(Span::styled(tail, text_style));
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
        let query_caret = state.caret();
        let replace_caret = state.replace_caret();
        let rows = vec![
            input_row(
                "> ",
                &state.query,
                state.focus == FindField::Query,
                query_selected,
                query_caret,
            ),
            input_row(
                "⤷ ",
                &state.replace,
                state.focus == FindField::Replace,
                false,
                replace_caret,
            ),
        ];
        Widget::render(Paragraph::new(rows), inner, buf);
    } else {
        let query_caret = state.caret();
        let prompt = input_row(
            "> ",
            &state.query,
            state.focus == FindField::Query,
            query_selected,
            query_caret,
        );
        Widget::render(Paragraph::new(prompt), inner, buf);
    }
    paint_chip(buf);
}
