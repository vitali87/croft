//! The Explorer "jump" popup (Cmd+Z): a zoxide-backed fuzzy directory
//! jumper. The user types keywords, sees zoxide's frecency-ranked
//! matches, and Enter re-roots the workspace there (and `cd`s the
//! terminal — see `App::change_workspace_root`).
//!
//! This widget is deliberately pure: it owns the query text, the result
//! list, and the selection, but never spawns zoxide itself. The app
//! layer runs `crate::zoxide::query` and feeds results in via
//! `set_results`, which keeps every behaviour here unit-testable without
//! a zoxide binary and concentrates the one subprocess call off the
//! render path.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use std::path::{Path, PathBuf};

pub struct ZoxideJump {
    pub query: String,
    pub cursor: usize,
    pub results: Vec<PathBuf>,
    pub selected: usize,
    pub scroll: usize,
    pub last_rect: Rect,
    pub last_inner_height: u16,
    /// False once a `set_results(None)` reports the zoxide binary could
    /// not be found on this host; drives the "unavailable" message.
    pub available: bool,
}

impl Default for ZoxideJump {
    fn default() -> Self {
        Self {
            query: String::new(),
            cursor: 0,
            results: Vec::new(),
            selected: 0,
            scroll: 0,
            last_rect: Rect::default(),
            last_inner_height: 0,
            available: true,
        }
    }
}

impl ZoxideJump {
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

    /// Replace the result list after the app re-queries zoxide. `None`
    /// means the binary is unavailable; `Some(paths)` is zoxide's ranked
    /// output (possibly empty). Selection/scroll reset because the list
    /// just changed shape.
    pub fn set_results(&mut self, results: Option<Vec<PathBuf>>) {
        match results {
            Some(paths) => {
                self.available = true;
                self.results = paths;
            }
            None => {
                self.available = false;
                self.results = Vec::new();
            }
        }
        self.selected = 0;
        self.scroll = 0;
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

    pub fn delete_char(&mut self) {
        if self.cursor >= self.char_count() {
            return;
        }
        let at = self.byte_offset(self.cursor);
        self.query.remove(at);
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
        if self.results.is_empty() {
            return;
        }
        if self.selected + 1 < self.results.len() {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn selected_path(&self) -> Option<&Path> {
        self.results.get(self.selected).map(|p| p.as_path())
    }

    #[cfg(test)]
    pub fn selected_index(&self) -> usize {
        self.selected
    }
}

/// Collapse a leading `$HOME` to `~` for display, so the result list
/// reads like the shell (`~/Documents/croft`) instead of a long absolute
/// path. Purely cosmetic; the jump still uses the absolute `PathBuf`.
fn display_path(path: &Path) -> String {
    let full = path.to_string_lossy();
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if !home.is_empty() {
            if full == home {
                return String::from("~");
            }
            if let Some(rest) = full.strip_prefix(&format!("{home}/")) {
                return format!("~/{rest}");
            }
        }
    }
    full.into_owned()
}

/// Draw the popup into `rect` exactly as given. Placement (anchoring over
/// the Explorer column vs. a centered fallback) is decided by the caller
/// in `App::render_zoxide_jump`, which knows the pane layout; this widget
/// just fills the box it is handed.
pub fn render_zoxide_jump(jump: &mut ZoxideJump, rect: Rect, buf: &mut Buffer) {
    jump.last_rect = rect;

    Widget::render(Clear, rect, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff)))
        .title(Span::styled(
            " Jump (zoxide) — Esc to close, ↑/↓ to navigate, Enter to jump ",
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
    let cursor = jump.cursor.min(jump.query.chars().count());
    let before: String = jump.query.chars().take(cursor).collect();
    let at: String = jump.query.chars().skip(cursor).take(1).collect();
    let after: String = jump.query.chars().skip(cursor + 1).collect();
    let caret_glyph = if at.is_empty() { String::from(" ") } else { at };
    let prompt_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Rgb(0x88, 0xc0, 0xd0))),
        Span::styled(before, query_style),
        Span::styled(caret_glyph, caret_style),
        Span::styled(after, query_style),
    ]);
    let prompt_rect = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    Widget::render(Paragraph::new(prompt_line), prompt_rect, buf);

    let separator_rect = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: 1,
    };
    let sep_line = Line::from(Span::styled(
        "─".repeat(separator_rect.width as usize),
        Style::default().fg(Color::Rgb(0x3b, 0x42, 0x52)),
    ));
    Widget::render(Paragraph::new(sep_line), separator_rect, buf);

    let list_rect = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(2),
    };
    jump.last_inner_height = list_rect.height;
    if list_rect.height == 0 {
        return;
    }

    if !jump.available {
        let msg = Line::from(Span::styled(
            "  zoxide is not installed on this host — installing in the background; try again shortly.",
            Style::default().fg(Color::Rgb(0xe5, 0xc0, 0x7b)),
        ));
        Widget::render(Paragraph::new(msg), list_rect, buf);
        return;
    }

    let visible = list_rect.height as usize;
    let total = jump.results.len();
    if jump.selected >= jump.scroll + visible {
        jump.scroll = jump.selected + 1 - visible;
    }
    if jump.selected < jump.scroll {
        jump.scroll = jump.selected;
    }
    let end = (jump.scroll + visible).min(total);

    if total == 0 {
        let empty = if jump.query.trim().is_empty() {
            Line::from(Span::styled(
                "  (no directories in the zoxide database yet)",
                Style::default().fg(Color::Rgb(0x7a, 0x82, 0x90)),
            ))
        } else {
            Line::from(Span::styled(
                format!("  No matches for '{}'", jump.query),
                Style::default().fg(Color::Rgb(0x7a, 0x82, 0x90)),
            ))
        };
        Widget::render(Paragraph::new(empty), list_rect, buf);
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end - jump.scroll);
    for (offset, path) in jump.results[jump.scroll..end].iter().enumerate() {
        let row_idx = jump.scroll + offset;
        let is_selected = row_idx == jump.selected;
        let row_style = if is_selected {
            Style::default()
                .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
                .fg(Color::White)
        } else {
            Style::default().fg(Color::Rgb(0xec, 0xef, 0xf4))
        };
        let prefix = if is_selected { "> " } else { "  " };
        let spans: Vec<Span<'static>> = vec![
            Span::styled(prefix.to_string(), row_style),
            Span::styled(display_path(path), row_style),
        ];
        lines.push(Line::from(spans));
    }
    Widget::render(Paragraph::new(lines), list_rect, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(rels: &[&str]) -> Vec<PathBuf> {
        rels.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn set_results_some_marks_available_and_resets_selection() {
        let mut j = ZoxideJump::new();
        j.set_results(Some(paths(&["/a", "/b", "/c"])));
        j.select_next();
        j.select_next();
        assert_eq!(j.selected_index(), 2);
        j.set_results(Some(paths(&["/x"])));
        assert!(j.available);
        assert_eq!(
            j.selected_index(),
            0,
            "new result list must reset selection"
        );
        assert_eq!(j.results.len(), 1);
    }

    #[test]
    fn set_results_none_marks_unavailable() {
        let mut j = ZoxideJump::new();
        j.set_results(None);
        assert!(
            !j.available,
            "None from query means the zoxide binary is missing"
        );
        assert!(j.results.is_empty());
        assert!(j.selected_path().is_none());
    }

    #[test]
    fn selected_path_tracks_navigation() {
        let mut j = ZoxideJump::new();
        j.set_results(Some(paths(&["/one", "/two", "/three"])));
        assert_eq!(j.selected_path(), Some(Path::new("/one")));
        j.select_next();
        assert_eq!(j.selected_path(), Some(Path::new("/two")));
        j.select_prev();
        assert_eq!(j.selected_path(), Some(Path::new("/one")));
    }

    #[test]
    fn select_next_caps_at_last_and_prev_caps_at_zero() {
        let mut j = ZoxideJump::new();
        j.set_results(Some(paths(&["/a", "/b"])));
        j.select_next();
        j.select_next();
        j.select_next();
        assert_eq!(j.selected_index(), 1, "select_next must not overshoot");
        j.select_prev();
        j.select_prev();
        j.select_prev();
        assert_eq!(j.selected_index(), 0, "select_prev must not underflow");
    }

    #[test]
    fn cursor_supports_mid_string_insert_and_delete() {
        let mut j = ZoxideJump::new();
        for c in "abc".chars() {
            j.push_char(c);
        }
        assert_eq!((j.query.as_str(), j.cursor), ("abc", 3));
        j.move_cursor_left();
        j.move_cursor_left();
        assert_eq!(j.cursor, 1);
        j.push_char('X');
        assert_eq!((j.query.as_str(), j.cursor), ("aXbc", 2));
        j.pop_char();
        assert_eq!((j.query.as_str(), j.cursor), ("abc", 1));
        j.delete_char();
        assert_eq!((j.query.as_str(), j.cursor), ("ac", 1));
        j.move_cursor_home();
        assert_eq!(j.cursor, 0);
        j.move_cursor_end();
        assert_eq!(j.cursor, 2);
        j.move_cursor_right();
        assert_eq!(
            j.cursor, 2,
            "cursor must not move past the end of the query"
        );
    }

    #[test]
    fn display_path_collapses_home_to_tilde() {
        let saved = std::env::var_os("HOME");
        // SAFETY: this test sets and restores $HOME; no other test in
        // this module reads it concurrently (cargo serializes within a
        // binary only across threads, but these are display-only reads).
        unsafe {
            std::env::set_var("HOME", "/Users/v");
        }
        assert_eq!(
            display_path(Path::new("/Users/v/Documents/croft")),
            "~/Documents/croft"
        );
        assert_eq!(display_path(Path::new("/Users/v")), "~");
        assert_eq!(display_path(Path::new("/etc/hosts")), "/etc/hosts");
        match saved {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}
