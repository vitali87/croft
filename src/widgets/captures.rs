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

/// Rows of pane context on each side of a captured line that ride an ask
/// to the Navigator (#372), and the most characters the whole excerpt may
/// carry: a capture is one line, the question is about that line, and a
/// screenful of unrelated output would only dilute it.
pub const ASK_CONTEXT_ROWS: usize = 4;
pub const ASK_CONTEXT_CHARS: usize = 2000;

/// The rows around `hit` in `lines`, `radius` on each side, clipped to the
/// buffer.
pub fn context_window(lines: &[String], hit: usize, radius: usize) -> Vec<String> {
    let start = hit.saturating_sub(radius);
    let end = hit.saturating_add(radius + 1).min(lines.len());
    lines[start.min(end)..end].to_vec()
}

/// The first `path:line[:col]` (or traceback `File "x", line N`) reference
/// anywhere in `line`. `file_ref_at` is column-scoped, so every column is
/// tried until one answers.
pub fn first_file_ref(line: &str) -> Option<crate::file_ref::FileRef> {
    (0..line.chars().count()).find_map(|col| crate::file_ref::file_ref_at(line, col))
}

/// The instruction and excerpt an "Ask Navigator about this line" sends:
/// which pane and trigger the line came from, the line itself, and the
/// (already redacted) context rows, capped at [`ASK_CONTEXT_CHARS`].
pub fn ask_prompt(entry: &CapturedLine, context: &[String]) -> (String, String) {
    let instruction = format!(
        "This line was captured from terminal pane {:?} by the trigger {:?}:\n{}\nExplain what \
         went wrong and propose the fix; the excerpt after it is the surrounding pane output.",
        entry.pane,
        entry.message,
        entry.line.trim_end()
    );
    let mut excerpt = context.join("\n");
    if excerpt.chars().count() > ASK_CONTEXT_CHARS {
        excerpt = excerpt.chars().take(ASK_CONTEXT_CHARS).collect::<String>() + "\n[excerpt cut]";
    }
    (instruction, excerpt)
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
            self.theme.ui(Color::White)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capture_ask_carries_the_line_a_bounded_window_and_a_file_ref() {
        let lines: Vec<String> = (0..20).map(|i| format!("row {i}")).collect();
        assert_eq!(context_window(&lines, 10, 2), lines[8..13]);
        assert_eq!(
            context_window(&lines, 0, 4),
            lines[0..5],
            "clipped at the top"
        );
        assert_eq!(
            context_window(&lines, 19, 4),
            lines[15..20],
            "clipped at the bottom"
        );
        assert!(context_window(&[], 3, 4).is_empty());

        let fr = first_file_ref("error[E0308]: mismatched types --> src/main.rs:12:3").unwrap();
        assert_eq!(
            (fr.path.as_str(), fr.line, fr.column),
            ("src/main.rs", 12, Some(3))
        );
        // `host.com:443` is a lookalike the reference syntax admits on
        // purpose; the caller drops it when no such file exists, the same
        // way the Cmd+click on a pane does.
        assert_eq!(
            first_file_ref("connect to host.com:443 failed").map(|f| f.path),
            Some(String::from("host.com"))
        );
        assert!(first_file_ref("plain text").is_none());

        let entry = CapturedLine {
            pane: "Terminal 2".into(),
            shell_pid: None,
            message: "build failed".into(),
            line: "error: could not compile `croft`".into(),
        };
        let context: Vec<String> = (0..3).map(|i| format!("ctx {i}")).collect();
        let (instruction, excerpt) = ask_prompt(&entry, &context);
        assert!(instruction.contains("Terminal 2") && instruction.contains("build failed"));
        assert!(instruction.contains("could not compile `croft`"));
        assert_eq!(excerpt, "ctx 0\nctx 1\nctx 2");
        let long: Vec<String> = vec!["x".repeat(ASK_CONTEXT_CHARS * 2)];
        let (_, cut) = ask_prompt(&entry, &long);
        assert!(cut.ends_with("[excerpt cut]"));
        assert!(cut.chars().count() <= ASK_CONTEXT_CHARS + 20);
    }
}
