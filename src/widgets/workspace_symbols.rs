//! VS Code "Go to Symbol in Workspace" (`#` in Quick Open, or the Command
//! Palette entry): a query box whose text is sent to every running language
//! server as a `workspace/symbol` request; the merged results list project
//! symbols across files. Selecting one opens its file at the definition.
//! Unlike the in-file symbol picker the matching happens server-side, so the
//! widget owns only the query, the latest result rows, and the selection;
//! the app re-queries on every keystroke and replaces the rows when the
//! newest reply lands.

use std::path::{Path, PathBuf};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::icons::for_outline_kind;
use crate::lsp::manager::WorkspaceSymbolItem;

pub struct WorkspaceSymbolPicker {
    pub query: String,
    pub cursor: usize,
    /// The workspace root, for rendering result paths relative to it.
    pub root: PathBuf,
    pub results: Vec<WorkspaceSymbolItem>,
    pub selected: usize,
    pub scroll: usize,
    /// True while a request is in flight and no reply has landed yet.
    pub loading: bool,
    /// True when no running server supports `workspace/symbol`.
    pub unsupported: bool,
    pub last_inner_height: u16,
    /// The popup's screen rect from the last render, for click-outside-to-close.
    pub last_rect: Rect,
}

impl WorkspaceSymbolPicker {
    pub fn new(root: PathBuf, initial_query: &str) -> Self {
        Self {
            query: initial_query.to_string(),
            cursor: initial_query.chars().count(),
            root,
            results: Vec::new(),
            selected: 0,
            scroll: 0,
            loading: false,
            unsupported: false,
            last_inner_height: 0,
            last_rect: Rect::default(),
        }
    }

    /// Install the newest reply's rows. Every reply answers a different
    /// query (stale ones are dropped upstream), so the selection restarts
    /// at the top: keeping row 5 of the previous set would hand Enter a
    /// symbol the user never picked.
    pub fn set_results(&mut self, results: Vec<WorkspaceSymbolItem>) {
        self.results = results;
        self.loading = false;
        self.selected = 0;
        self.scroll = 0;
    }

    /// The selected row's jump target.
    pub fn selected_item(&self) -> Option<&WorkspaceSymbolItem> {
        self.results.get(self.selected)
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

    pub fn push_char(&mut self, c: char) {
        let at = self.byte_offset(self.cursor);
        self.query.insert(at, c);
        self.cursor += 1;
    }

    pub fn pop_char(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let from = self.byte_offset(self.cursor - 1);
        let to = self.byte_offset(self.cursor);
        self.query.replace_range(from..to, "");
        self.cursor -= 1;
    }

    pub fn delete_char(&mut self) {
        if self.cursor >= self.char_count() {
            return;
        }
        let from = self.byte_offset(self.cursor);
        let to = self.byte_offset(self.cursor + 1);
        self.query.replace_range(from..to, "");
    }

    pub fn move_cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_count());
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor = self.char_count();
    }

    pub fn select_next(&mut self) {
        if !self.results.is_empty() && self.selected + 1 < self.results.len() {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
}

/// A result path shown workspace-relative when possible, absolute otherwise.
fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

pub fn render_workspace_symbols(
    picker: &mut WorkspaceSymbolPicker,
    area: Rect,
    buf: &mut Buffer,
    gradient: bool,
    center: bool,
) {
    let width = area.width.saturating_mul(7) / 10;
    let width = width.clamp(40, 110.min(area.width));
    let height = area.height.saturating_mul(6) / 10;
    let height = height.clamp(10, area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = if center {
        area.y + (area.height.saturating_sub(height)) / 2
    } else {
        area.y + (area.height.saturating_sub(height)) / 4
    };
    let rect = Rect {
        x,
        y,
        width,
        height,
    };

    picker.last_rect = rect;
    Widget::render(Clear, rect, buf);
    let title = Span::styled(
        " Go to Symbol in Workspace — Esc to close, ↑/↓ to navigate, Enter to go ",
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
    let sel_bg = if gradient {
        let (r, g, b) = crate::gradient::POPUP_SEL_BG;
        Color::Rgb(r, g, b)
    } else {
        Color::Rgb(0x1e, 0x3a, 0x6e)
    };

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let query_style = Style::default()
        .fg(Color::Rgb(0xec, 0xef, 0xf4))
        .add_modifier(Modifier::BOLD);
    let caret_style = Style::default()
        .fg(Color::Rgb(0x16, 0x18, 0x1f))
        .bg(Color::Rgb(0xec, 0xef, 0xf4))
        .add_modifier(Modifier::SLOW_BLINK);
    let cursor = picker.cursor.min(picker.query.chars().count());
    let before: String = picker.query.chars().take(cursor).collect();
    let at: String = picker.query.chars().skip(cursor).take(1).collect();
    let after: String = picker.query.chars().skip(cursor + 1).collect();
    let caret_glyph = if at.is_empty() { String::from(" ") } else { at };
    let prompt_line = Line::from(vec![
        Span::styled("# ", Style::default().fg(Color::Rgb(0x88, 0xc0, 0xd0))),
        Span::styled(before, query_style),
        Span::styled(caret_glyph, caret_style),
        Span::styled(after, query_style),
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

    let sep_line = Line::from(Span::styled(
        "─".repeat(inner.width as usize),
        Style::default().fg(Color::Rgb(0x3b, 0x42, 0x52)),
    ));
    Widget::render(
        Paragraph::new(sep_line),
        Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: 1,
        },
        buf,
    );

    let list_rect = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(2),
    };
    picker.last_inner_height = list_rect.height;
    if list_rect.height == 0 {
        return;
    }

    let visible = list_rect.height as usize;
    let total = picker.results.len();
    if picker.selected >= picker.scroll + visible {
        picker.scroll = picker.selected + 1 - visible;
    }
    if picker.selected < picker.scroll {
        picker.scroll = picker.selected;
    }
    let end = (picker.scroll + visible).min(total);

    if total == 0 {
        let msg = if picker.unsupported {
            "  No running language server supports workspace symbols"
        } else if picker.loading {
            "  Searching workspace symbols"
        } else if picker.query.trim().is_empty() {
            "  Type to search every symbol in the workspace"
        } else {
            "  No symbols match"
        };
        Widget::render(
            Paragraph::new(Line::from(Span::styled(
                msg.to_string(),
                Style::default().fg(Color::Rgb(0x7a, 0x82, 0x90)),
            ))),
            list_rect,
            buf,
        );
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end - picker.scroll);
    for (offset, item) in picker.results[picker.scroll..end].iter().enumerate() {
        let row_idx = picker.scroll + offset;
        let is_selected = row_idx == picker.selected;
        let row_style = if is_selected {
            Style::default().bg(sel_bg).fg(Color::White)
        } else {
            Style::default().fg(Color::Rgb(0xec, 0xef, 0xf4))
        };
        let detail_style = if is_selected {
            Style::default().bg(sel_bg).fg(Color::Rgb(0xa0, 0xb4, 0xd8))
        } else {
            Style::default().fg(Color::Rgb(0x8e, 0x95, 0xa4))
        };
        let icon = for_outline_kind(item.kind);
        let icon_style = if is_selected {
            Style::default().bg(sel_bg).fg(icon.color)
        } else {
            Style::default().fg(icon.color)
        };
        let prefix = if is_selected { "> " } else { "  " };
        // Right-align the workspace-relative path + line, like the in-file
        // picker right-aligns the line number.
        let path_label = format!(
            "{}:{} ",
            display_path(&picker.root, &item.path),
            item.line + 1
        );
        let name = match &item.container {
            Some(c) if !c.is_empty() => format!("{} — {c}", item.name),
            _ => item.name.clone(),
        };
        let used = prefix.chars().count() + 2 + name.chars().count() + path_label.chars().count();
        let pad = (list_rect.width as usize).saturating_sub(used).max(1);
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), row_style),
            Span::styled(format!("{} ", icon.glyph), icon_style),
            Span::styled(name, row_style),
            Span::styled(" ".repeat(pad), row_style),
            Span::styled(path_label, detail_style),
        ]));
    }
    Widget::render(Paragraph::new(lines), list_rect, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::manager::OutlineKind;

    fn item(name: &str, path: &str) -> WorkspaceSymbolItem {
        WorkspaceSymbolItem {
            name: name.to_string(),
            kind: OutlineKind::Function,
            path: PathBuf::from(path),
            line: 3,
            character: 0,
            container: None,
        }
    }

    #[test]
    fn a_new_result_set_resets_the_selection_to_the_top() {
        // Arrow to row 5, type another character: the reply is a different
        // result set, and Enter must not land on row 5 of it.
        let mut p = WorkspaceSymbolPicker::new(PathBuf::from("/w"), "");
        p.set_results((0..10).map(|i| item(&format!("a{i}"), "a.rs")).collect());
        p.selected = 5;
        p.set_results((0..10).map(|i| item(&format!("b{i}"), "b.rs")).collect());
        assert_eq!(p.selected, 0, "a fresh reply starts at the top");
    }

    #[test]
    fn set_results_clamps_a_stale_selection() {
        let mut p = WorkspaceSymbolPicker::new(PathBuf::from("/w"), "");
        p.set_results(vec![item("a", "/w/a.rs"), item("b", "/w/b.rs")]);
        p.selected = 1;
        p.set_results(vec![item("a", "/w/a.rs")]);
        assert_eq!(p.selected, 0, "selection clamps to the shrunk result set");
        assert_eq!(p.selected_item().unwrap().name, "a");
    }

    #[test]
    fn display_path_is_workspace_relative() {
        assert_eq!(
            display_path(Path::new("/w"), Path::new("/w/src/main.rs")),
            "src/main.rs"
        );
        assert_eq!(
            display_path(Path::new("/w"), Path::new("/elsewhere/x.rs")),
            "/elsewhere/x.rs",
            "paths outside the root stay absolute"
        );
    }
}
