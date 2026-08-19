//! The bottom panel's CAPTURES tab: lines collected by `capture` triggers
//! (iTerm2's Capture Output). A trigger row in `triggers.json` with
//! `"action": "capture"` funnels every matching output line — compiler
//! errors, test failures, panics — into this list; activating an entry
//! jumps its pane back to the captured line and selects it.
//!
//! The panel owns the list and the selection; the jump itself needs the
//! panes, so the app reads [`CapturesPanel::selected_entry`] and acts
//! (mirrors the PORTS panel split).

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::theme::Theme;

const COLOR_DIM: Color = Color::Rgb(0x80, 0x88, 0x98);
const COLOR_HEAD: Color = Color::Rgb(0x8b, 0x93, 0xa1);
const COLOR_MSG: Color = Color::Rgb(0xCC, 0xCC, 0xCC);

/// Keep the newest N captures; a chatty trigger on a huge build log must
/// not grow without bound.
const CAPTURES_MAX: usize = 500;

/// One captured output line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedLine {
    /// The capturing pane's label at capture time (display only).
    pub pane: String,
    /// The capturing pane's shell pid — the stable key the jump uses to
    /// find the pane again (labels change with the foreground process).
    pub shell_pid: Option<i32>,
    /// The trigger's interpolated message (the entry's headline).
    pub message: String,
    /// The whole escape-stripped output line that matched.
    pub line: String,
}

pub struct CapturesPanel {
    entries: Vec<CapturedLine>,
    selected: usize,

    pub focus_gradient: bool,
    pub theme: Theme,
    pub focused: bool,
    pub hover_pointer: Option<(u16, u16)>,

    pub last_area: Rect,
    /// Body-row hit rects (index into `entries`), recomputed each render.
    row_rects: Vec<(Rect, usize)>,
}

impl CapturesPanel {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            selected: 0,
            focus_gradient: false,
            theme: Theme::default(),
            focused: false,
            hover_pointer: None,
            last_area: Rect::default(),
            row_rects: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Append a capture (newest last, like a log) and follow it with the
    /// selection so Enter always acts on the latest hit by default.
    pub fn push(&mut self, entry: CapturedLine) {
        self.entries.push(entry);
        if self.entries.len() > CAPTURES_MAX {
            let drop = self.entries.len() - CAPTURES_MAX;
            self.entries.drain(..drop);
        }
        self.selected = self.entries.len() - 1;
    }

    pub fn selected_entry(&self) -> Option<&CapturedLine> {
        self.entries.get(self.selected)
    }

    pub fn move_selection(&mut self, delta: i32) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() as i32 - 1;
        self.selected = (self.selected as i32 + delta).clamp(0, last) as usize;
    }

    pub fn select_index(&mut self, idx: usize) {
        if idx < self.entries.len() {
            self.selected = idx;
        }
    }

    pub fn remove_selected(&mut self) {
        if self.selected < self.entries.len() {
            self.entries.remove(self.selected);
            if self.selected >= self.entries.len() && self.selected > 0 {
                self.selected -= 1;
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.selected = 0;
    }

    /// The entry index under screen cell (col, row), if any.
    pub fn row_at(&self, col: u16, row: u16) -> Option<usize> {
        self.row_rects
            .iter()
            .find(|(r, _)| col >= r.x && col < r.x + r.width && row == r.y)
            .map(|&(_, idx)| idx)
    }
}

impl Widget for &mut CapturesPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.last_area = area;
        self.row_rects.clear();
        if area.width == 0 || area.height == 0 {
            return;
        }
        let accent = if self.focus_gradient {
            crate::gradient::rgb_color(crate::gradient::PANEL_TITLE_FG)
        } else {
            Color::White
        };
        buf.set_style(area, Style::default().bg(self.theme.editor_bg()));

        if self.entries.is_empty() {
            Paragraph::new(Line::from(Span::styled(
                "No captured output yet. Add a trigger with \"action\": \"capture\" to triggers.json (Preferences: Open Terminal Triggers) and matching lines collect here.",
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            )))
            .render(area, buf);
            return;
        }

        // Header row.
        buf.set_string(
            area.x + 1,
            area.y,
            format!("{:<12}{:<28}{}", "PANE", "MATCH", "LINE"),
            Style::default()
                .fg(self.theme.ui(COLOR_HEAD))
                .add_modifier(Modifier::BOLD),
        );

        // Body rows; reserve the last line for the keyboard hints. The
        // newest capture is the most interesting one, so when the list
        // outgrows the body the window slides to keep the selection visible.
        let body_h = area.height.saturating_sub(2) as usize;
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
        let first = self.selected.saturating_sub(body_h.saturating_sub(1));
        for (row, idx) in (first..self.entries.len()).take(body_h).enumerate() {
            let entry = &self.entries[idx];
            let y = area.y + 1 + row as u16;
            let r = Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            };
            self.row_rects.push((r, idx));
            let selected = idx == self.selected;
            let hovered =
                crate::widgets::hover::row_hover_bg(r, self.hover_pointer, self.theme).is_some();
            if selected || hovered {
                buf.set_style(
                    r,
                    Style::default().bg(crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG)),
                );
            }
            // Clipped plainly (no ellipsis, matching the search panel's
            // truncation style); the row width clips LINE naturally.
            let msg: String = entry.message.chars().take(26).collect();
            let spans = vec![
                Span::styled(
                    format!("{:<12}", truncated(&entry.pane, 10)),
                    Style::default().fg(accent),
                ),
                Span::styled(
                    format!("{msg:<28}"),
                    Style::default()
                        .fg(self.theme.ui(COLOR_MSG))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    entry.line.clone(),
                    Style::default().fg(self.theme.ui(COLOR_DIM)),
                ),
            ];
            Paragraph::new(Line::from(spans)).render(
                Rect {
                    x: r.x + 1,
                    y: r.y,
                    width: r.width.saturating_sub(1),
                    height: 1,
                },
                buf,
            );
        }

        if area.height >= 2 {
            buf.set_stringn(
                area.x,
                area.y + area.height - 1,
                "  ⏎ jump to line   ·   x remove   ·   c clear all",
                area.width as usize,
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            );
        }
    }
}

/// First `max` chars, clipped plainly.
fn truncated(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}
