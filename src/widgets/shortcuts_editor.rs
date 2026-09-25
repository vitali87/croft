//! The Keyboard Shortcuts editor modal (#612): search the commands, press
//! Enter to record a chord for the selected one, see what else it would
//! trigger, Enter again to write it to `keybindings.json`, Delete to remove
//! the selected command's own bindings.

use crate::shortcuts::Row;
use crate::widgets::command_palette::Command;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Widget};

/// A chord being recorded for `command`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording {
    pub command: Command,
    /// The chord pressed so far, as `keybindings.json` spells it.
    pub chord: Option<String>,
    /// Other commands the chord already triggers.
    pub conflicts: Vec<Command>,
}

#[derive(Debug, Default)]
pub struct ShortcutsEditor {
    pub rows: Vec<Row>,
    pub query: String,
    /// Index into [`Self::visible`].
    pub selected: usize,
    pub scroll: usize,
    pub recording: Option<Recording>,
    pub last_rect: Rect,
}

impl ShortcutsEditor {
    pub fn new(rows: Vec<Row>) -> Self {
        Self {
            rows,
            ..Self::default()
        }
    }

    /// Indices of the rows matching the query, in order.
    pub fn visible(&self) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|&i| crate::shortcuts::matches(&self.rows[i], &self.query))
            .collect()
    }

    /// The selected row, if any.
    pub fn selected_row(&self) -> Option<&Row> {
        let i = *self.visible().get(self.selected)?;
        self.rows.get(i)
    }

    /// Move the selection by `delta`, clamped.
    pub fn move_selection(&mut self, delta: isize) {
        let len = self.visible().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = self.selected.saturating_add_signed(delta).min(len - 1);
    }

    /// Replace the query; the selection returns to the top.
    pub fn set_query(&mut self, q: String) {
        self.query = q;
        self.selected = 0;
        self.scroll = 0;
    }
}

/// Paint the editor centred in `area`.
pub fn render(ed: &mut ShortcutsEditor, area: Rect, buf: &mut Buffer, theme: crate::theme::Theme) {
    let width = (area.width.saturating_mul(8) / 10).clamp(40.min(area.width), 120.min(area.width));
    let height = (area.height.saturating_mul(8) / 10).clamp(10.min(area.height), area.height);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 4,
        width,
        height,
    };
    ed.last_rect = rect;
    Widget::render(Clear, rect, buf);
    let bg = theme.ui(Color::Rgb(0x16, 0x18, 0x1f));
    let text = Style::default()
        .fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
        .bg(bg);
    let dim = Style::default()
        .fg(theme.ui(Color::Rgb(0x8a, 0x93, 0xa6)))
        .bg(bg);
    let warn = Style::default()
        .fg(theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b)))
        .bg(bg);
    let sel = text.bg(theme.ui(Color::Rgb(0x1e, 0x3a, 0x6e)));
    Widget::render(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))))
            .title(" Keyboard Shortcuts ")
            .style(Style::default().bg(bg)),
        rect,
        buf,
    );
    let inner = Rect {
        x: rect.x + 2,
        y: rect.y + 1,
        width: rect.width.saturating_sub(4),
        height: rect.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height < 4 {
        return;
    }
    let w = inner.width as usize;
    let mut y = inner.y;
    match &ed.recording {
        Some(rec) => {
            let chord = rec.chord.as_deref().unwrap_or("\u{2026}");
            buf.set_stringn(
                inner.x,
                y,
                format!("Press the new chord for {}: {chord}", rec.command.title()),
                w,
                text.add_modifier(Modifier::BOLD),
            );
            y += 1;
            let names: Vec<&str> = rec.conflicts.iter().map(|c| c.title()).collect();
            let line = if names.is_empty() {
                String::from("Enter saves \u{b7} Esc cancels")
            } else {
                format!(
                    "Also runs: {} \u{b7} Enter saves anyway \u{b7} Esc cancels",
                    names.join(", ")
                )
            };
            buf.set_stringn(
                inner.x,
                y,
                line,
                w,
                if names.is_empty() { dim } else { warn },
            );
        }
        None => {
            buf.set_stringn(inner.x, y, format!("> {}", ed.query), w, text);
            y += 1;
            buf.set_stringn(
                inner.x,
                y,
                "Enter record a chord \u{b7} Delete remove your bindings \u{b7} Esc close",
                w,
                dim,
            );
        }
    }
    y += 2;
    let visible = ed.visible();
    let rows_h = (inner.y + inner.height).saturating_sub(y) as usize;
    if ed.selected < ed.scroll {
        ed.scroll = ed.selected;
    } else if rows_h > 0 && ed.selected >= ed.scroll + rows_h {
        ed.scroll = ed.selected + 1 - rows_h;
    }
    let title_w = w.saturating_sub(40).max(20).min(w);
    for (n, &i) in visible.iter().enumerate().skip(ed.scroll).take(rows_h) {
        let row = &ed.rows[i];
        let style = if n == ed.selected { sel } else { text };
        let keys = if row.user.is_empty() {
            row.default.to_string()
        } else if row.default.is_empty() {
            row.user.join(", ")
        } else {
            format!("{} (default {})", row.user.join(", "), row.default)
        };
        let line = format!("{:<title_w$} {keys}", row.title);
        buf.set_stringn(inner.x, y, format!("{line:<w$}"), w, style);
        y += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> ShortcutsEditor {
        ShortcutsEditor::new(crate::shortcuts::rows(
            "[{ \"key\": \"f9\", \"command\": \"quick_open\" }]",
        ))
    }

    #[test]
    fn the_query_filters_and_the_selection_stays_inside_it() {
        let mut ed = editor();
        assert_eq!(ed.visible().len(), ed.rows.len());
        ed.set_query(String::from("go to file"));
        let vis = ed.visible();
        assert!(!vis.is_empty());
        assert!(
            vis.iter()
                .all(|&i| ed.rows[i].title.to_lowercase().contains("go to file"))
        );
        assert_eq!(
            ed.selected_row().map(|r| r.command),
            Some(Command::QuickOpen)
        );
        ed.move_selection(100);
        assert_eq!(ed.selected, vis.len() - 1);
        ed.move_selection(-100);
        assert_eq!(ed.selected, 0);
        ed.set_query(String::from("zzzz no such command"));
        assert!(ed.selected_row().is_none());
    }

    #[test]
    fn rows_show_the_title_and_both_the_default_and_user_keys() {
        let mut ed = editor();
        ed.set_query(String::from("quick_open"));
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 30));
        render(
            &mut ed,
            Rect::new(0, 0, 120, 30),
            &mut buf,
            crate::theme::Theme::default(),
        );
        let text: String = (0..30)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(text.contains("Keyboard Shortcuts"), "{text}");
        assert!(text.contains("Go to File"), "{text}");
        assert!(text.contains("Cmd+P"), "{text}");
        assert!(text.contains("f9"), "{text}");
    }
}
