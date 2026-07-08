//! The terminal's command-history popup (Ctrl+Shift+R): searches the
//! durable cross-session store (`crate::command_history`) the way atuin's
//! enhanced Ctrl+R searches shell history — newest first, deduplicated,
//! with scope filters (all / this directory / failed only) cycled by
//! Ctrl+R inside the popup. Enter types the selected command into the
//! active pane (without running it).
//!
//! Pure widget: it owns the query text, the filtered results and the
//! selection; the app layer runs the store search on each edit and feeds
//! results in via [`HistoryPopup::set_results`].

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::command_history::{HistoryEntry, HistoryScope};

pub struct HistoryPopup {
    pub query: String,
    pub cursor: usize,
    pub results: Vec<HistoryEntry>,
    pub selected: usize,
    pub scope: HistoryScope,
    /// The active pane's directory when the popup opened; the Dir scope
    /// filters against it and the footer shows it.
    pub cwd: String,
    pub last_rect: Rect,
}

impl Default for HistoryPopup {
    fn default() -> Self {
        Self {
            query: String::new(),
            cursor: 0,
            results: Vec::new(),
            selected: 0,
            scope: HistoryScope::All,
            cwd: String::new(),
            last_rect: Rect::default(),
        }
    }
}

impl HistoryPopup {
    pub fn new() -> Self {
        Self::default()
    }

    fn char_count(&self) -> usize {
        self.query.chars().count()
    }

    fn byte_offset(&self, char_idx: usize) -> usize {
        self.query
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.query.len())
    }

    /// Replace the result list after the app re-ran the store search.
    pub fn set_results(&mut self, results: Vec<HistoryEntry>) {
        self.results = results;
        self.selected = 0;
    }

    pub fn push_char(&mut self, c: char) {
        let at = self.byte_offset(self.cursor);
        self.query.insert(at, c);
        self.cursor += 1;
    }

    pub fn pop_char(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = self.byte_offset(self.cursor - 1);
        self.query.remove(at);
        self.cursor -= 1;
    }

    pub fn move_cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_count());
    }

    pub fn select_next(&mut self) {
        if !self.results.is_empty() && self.selected + 1 < self.results.len() {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn selected_entry(&self) -> Option<&HistoryEntry> {
        self.results.get(self.selected)
    }
}

/// `2m 05s` style, mirroring the decoration duration formatting but from
/// milliseconds.
fn dur_label(ms: u64) -> String {
    crate::widgets::terminal::human_duration(std::time::Duration::from_millis(ms))
}

/// Draw the popup into `rect` exactly as given; placement is the caller's
/// job (same contract as `render_zoxide_jump`).
pub fn render_history_popup(pop: &mut HistoryPopup, rect: Rect, buf: &mut Buffer, gradient: bool) {
    pop.last_rect = rect;

    Widget::render(Clear, rect, buf);
    let title = Span::styled(
        " Command History — ⏎ types it, ⌃R cycles scope, Esc closes ",
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
    if gradient {
        crate::gradient::paint_gradient_box(buf, rect);
        buf.set_span(rect.x + 1, rect.y, &title, title.width() as u16);
    }
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let sel_bg = if gradient {
        let (r, g, b) = crate::gradient::POPUP_SEL_BG;
        Color::Rgb(r, g, b)
    } else {
        Color::Rgb(0x1e, 0x3a, 0x6e)
    };

    // Query line with a block caret, the zoxide-jump idiom.
    let query_style = Style::default()
        .fg(Color::Rgb(0xec, 0xef, 0xf4))
        .add_modifier(Modifier::BOLD);
    let caret_style = Style::default()
        .fg(Color::Rgb(0x16, 0x18, 0x1f))
        .bg(Color::Rgb(0xec, 0xef, 0xf4));
    let cursor = pop.cursor.min(pop.char_count());
    let before: String = pop.query.chars().take(cursor).collect();
    let at: String = pop.query.chars().skip(cursor).take(1).collect();
    let after: String = pop.query.chars().skip(cursor + 1).collect();
    let caret_glyph = if at.is_empty() { String::from(" ") } else { at };
    let scope_note = match pop.scope {
        HistoryScope::All => String::new(),
        HistoryScope::Dir => format!("  [{}]", pop.cwd),
        HistoryScope::Failed => String::from("  [failed only]"),
    };
    let prompt_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Rgb(0x88, 0xc0, 0xd0))),
        Span::styled(before, query_style),
        Span::styled(caret_glyph, caret_style),
        Span::styled(after, query_style),
        Span::styled(
            scope_note,
            Style::default().fg(Color::Rgb(0xe5, 0xc0, 0x7b)),
        ),
    ]);
    Widget::render(
        Paragraph::new(prompt_line),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
        buf,
    );

    let list_rect = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };
    if list_rect.height == 0 {
        return;
    }
    if pop.results.is_empty() {
        Widget::render(
            Paragraph::new(Line::from(Span::styled(
                "  No matching commands yet (history records shell-integrated panes).",
                Style::default().fg(Color::Rgb(0x80, 0x88, 0x98)),
            ))),
            list_rect,
            buf,
        );
        return;
    }
    // Keep the selection visible: scroll the window, newest at the top.
    let h = list_rect.height as usize;
    let first = pop.selected.saturating_sub(h.saturating_sub(1));
    for (row, idx) in (first..pop.results.len()).take(h).enumerate() {
        let e = &pop.results[idx];
        let y = list_rect.y + row as u16;
        let r = Rect {
            x: list_rect.x,
            y,
            width: list_rect.width,
            height: 1,
        };
        if idx == pop.selected {
            buf.set_style(r, Style::default().bg(sel_bg));
        }
        let ok = e.exit == Some(0);
        let mark_style = Style::default().fg(if ok {
            Color::Rgb(0x4f, 0xb1, 0xa6)
        } else {
            Color::Rgb(0xf1, 0x4c, 0x4c)
        });
        let meta = format!(
            "  {} · {}{}",
            dur_label(e.dur_ms),
            e.cwd,
            e.exit
                .filter(|c| *c != 0)
                .map(|c| format!(" · exit {c}"))
                .unwrap_or_default()
        );
        let spans = vec![
            Span::styled("● ", mark_style),
            Span::styled(
                e.cmd.clone(),
                Style::default()
                    .fg(Color::Rgb(0xec, 0xef, 0xf4))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(meta, Style::default().fg(Color::Rgb(0x80, 0x88, 0x98))),
        ];
        Widget::render(
            Paragraph::new(Line::from(spans)),
            Rect {
                x: r.x + 1,
                y: r.y,
                width: r.width.saturating_sub(1),
                height: 1,
            },
            buf,
        );
    }
}
