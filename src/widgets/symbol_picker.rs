//! VS Code "Go to Symbol in Editor" (Cmd/Ctrl+Shift+O): a fuzzy-filtered list
//! of the active file's symbols (functions, classes, methods, ...). Selecting
//! one jumps the editor to its definition. Typing a leading `:` switches to
//! Go to Line — `:42` jumps to line 42 — mirroring VS Code's shared quick
//! input. The widget owns only the query and selection; the app performs the
//! jump from `selected_target()` / `line_target()`.

use std::path::PathBuf;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::icons::for_outline_kind;
use crate::lsp::manager::OutlineSymbol;
use crate::widgets::file_finder::fuzzy_score;

pub struct SymbolPicker {
    pub query: String,
    pub cursor: usize,
    /// The active file the symbols belong to; the jump target's path.
    pub path: PathBuf,
    symbols: Vec<OutlineSymbol>,
    /// Indices into `symbols`, filtered + ranked against the query.
    pub results: Vec<usize>,
    pub selected: usize,
    pub scroll: usize,
    pub last_inner_height: u16,
    /// The popup's screen rect from the last render, for click-outside-to-close.
    pub last_rect: Rect,
}

impl SymbolPicker {
    pub fn new(path: PathBuf, symbols: Vec<OutlineSymbol>) -> Self {
        let mut me = Self {
            query: String::new(),
            cursor: 0,
            path,
            symbols,
            results: Vec::new(),
            selected: 0,
            scroll: 0,
            last_inner_height: 0,
            last_rect: Rect::default(),
        };
        me.refresh_results();
        me
    }

    /// True when the query is a Go to Line request (`:` then optional digits),
    /// so the app routes Enter to a line jump instead of a symbol jump.
    pub fn is_line_mode(&self) -> bool {
        self.query.trim_start().starts_with(':')
    }

    /// The 1-based line a `:N` query targets, if any.
    pub fn line_target(&self) -> Option<usize> {
        self.query
            .trim_start()
            .strip_prefix(':')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&n| n >= 1)
    }

    /// The selected symbol's 0-based (line, character) jump target.
    pub fn selected_target(&self) -> Option<(u32, u32)> {
        let idx = *self.results.get(self.selected)?;
        let sym = self.symbols.get(idx)?;
        Some((sym.line, sym.character))
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
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn pop_char(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = self.byte_offset(self.cursor - 1);
        self.query.remove(at);
        self.cursor -= 1;
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn delete_char(&mut self) {
        if self.cursor >= self.char_count() {
            return;
        }
        let at = self.byte_offset(self.cursor);
        self.query.remove(at);
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
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
        self.selected = self.selected.saturating_sub(1);
    }

    /// The result index at screen row `y`, if `y` lands on a visible row.
    /// The list body starts three rows below `last_rect.y` (top border, the
    /// query prompt, then the separator) and runs `last_inner_height` rows,
    /// so this stays in lock-step with [`render_symbol_picker`]. Used to map
    /// a mouse click to a result row. Line mode (`:N`) renders a "Go to
    /// line" hint in the list area while `results` still holds every
    /// symbol, so clicks there map to nothing.
    pub fn row_index_at(&self, y: u16) -> Option<usize> {
        if self.is_line_mode() {
            return None;
        }
        let list_top = self.last_rect.y.saturating_add(3);
        if y < list_top || y - list_top >= self.last_inner_height {
            return None;
        }
        let idx = self.scroll + (y - list_top) as usize;
        (idx < self.results.len()).then_some(idx)
    }

    /// Re-rank the symbol list against the query. Line mode (`:`) shows the
    /// symbols untouched (the list is irrelevant; Enter jumps to the line). An
    /// empty query lists every symbol in document order; otherwise rows are
    /// kept when their lower-cased name fuzzy-matches, ranked by score, ties
    /// broken by document order for stability.
    fn refresh_results(&mut self) {
        if self.is_line_mode() {
            self.results = (0..self.symbols.len()).collect();
            return;
        }
        let needle = self.query.trim().to_lowercase();
        if needle.is_empty() {
            self.results = (0..self.symbols.len()).collect();
            return;
        }
        let mut scored: Vec<(i32, usize)> = self
            .symbols
            .iter()
            .enumerate()
            .filter_map(|(idx, sym)| {
                fuzzy_score(&needle, &sym.name.to_lowercase(), 0).map(|score| (score, idx))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.results = scored.into_iter().map(|(_, idx)| idx).collect();
    }
}

pub fn render_symbol_picker(
    picker: &mut SymbolPicker,
    area: Rect,
    buf: &mut Buffer,
    gradient: bool,
    center: bool,
) {
    let width = area.width.saturating_mul(7) / 10;
    let width = width.clamp(40, 100.min(area.width));
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
        " Go to Symbol — Esc to close, ↑/↓ to navigate, Enter to go, : for line ",
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
        Span::styled("@ ", Style::default().fg(Color::Rgb(0x88, 0xc0, 0xd0))),
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

    // Line mode: the symbol list is moot; show the target so the user sees
    // where Enter lands.
    if picker.is_line_mode() {
        let msg = match picker.line_target() {
            Some(n) => format!("  Go to line {n}"),
            None => String::from("  Type a line number, e.g. :42"),
        };
        Widget::render(
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(Color::Rgb(0xa0, 0xb4, 0xd8)),
            ))),
            list_rect,
            buf,
        );
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
        let empty = Line::from(Span::styled(
            String::from("  No symbols in this file"),
            Style::default().fg(Color::Rgb(0x7a, 0x82, 0x90)),
        ));
        Widget::render(Paragraph::new(empty), list_rect, buf);
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end - picker.scroll);
    for (offset, &sym_idx) in picker.results[picker.scroll..end].iter().enumerate() {
        let row_idx = picker.scroll + offset;
        let is_selected = row_idx == picker.selected;
        let Some(sym) = picker.symbols.get(sym_idx) else {
            continue;
        };
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
        let icon = for_outline_kind(sym.kind);
        let icon_style = if is_selected {
            Style::default().bg(sel_bg).fg(icon.color)
        } else {
            Style::default().fg(icon.color)
        };
        // Indent nested symbols by depth (capped), like the outline tree.
        let indent = "  ".repeat((sym.depth as usize).min(6));
        let prefix = if is_selected { "> " } else { "  " };
        // Right-align the line number, like the palette's chord hint.
        let line_label = format!("{} ", sym.line + 1);
        let used = prefix.chars().count()
            + indent.chars().count()
            + 2 // icon + space
            + sym.name.chars().count()
            + line_label.chars().count();
        let pad = (list_rect.width as usize).saturating_sub(used).max(1);
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), row_style),
            Span::styled(indent, row_style),
            Span::styled(format!("{} ", icon.glyph), icon_style),
            Span::styled(sym.name.clone(), row_style),
            Span::styled(" ".repeat(pad), row_style),
            Span::styled(line_label, detail_style),
        ]));
    }
    Widget::render(Paragraph::new(lines), list_rect, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::manager::OutlineKind;

    fn sym(name: &str, kind: OutlineKind, depth: u16, line: u32) -> OutlineSymbol {
        OutlineSymbol {
            name: name.to_string(),
            detail: None,
            kind,
            depth,
            line,
            character: 0,
            range_start_line: line,
            range_end_line: line,
        }
    }

    fn picker() -> SymbolPicker {
        SymbolPicker::new(
            PathBuf::from("f.rs"),
            vec![
                sym("alpha", OutlineKind::Function, 0, 1),
                sym("beta", OutlineKind::Method, 1, 5),
                sym("gamma_helper", OutlineKind::Function, 0, 20),
            ],
        )
    }

    #[test]
    fn empty_query_lists_every_symbol_in_order() {
        let p = picker();
        assert_eq!(p.results, vec![0, 1, 2]);
    }

    /// In line mode (`:42`) the list area renders only a "Go to line" hint,
    /// yet `results` still holds every symbol; a click there must map to
    /// nothing rather than jump to an invisible symbol row.
    #[test]
    fn row_index_at_refuses_clicks_in_line_mode() {
        let mut p = picker();
        p.last_rect = Rect {
            x: 10,
            y: 5,
            width: 60,
            height: 10,
        };
        p.last_inner_height = 6;
        assert_eq!(
            p.row_index_at(8),
            Some(0),
            "outside line mode the first list row maps to the first symbol"
        );
        for c in [':', '4', '2'] {
            p.push_char(c);
        }
        assert!(p.is_line_mode());
        assert_eq!(
            p.row_index_at(8),
            None,
            "line mode renders a hint, not rows; clicks must map to nothing"
        );
    }

    #[test]
    fn fuzzy_query_filters_and_ranks() {
        let mut p = picker();
        p.push_char('g');
        p.push_char('h');
        // "gh" subsequence-matches gamma_helper, not alpha/beta.
        assert_eq!(p.results, vec![2]);
        assert_eq!(p.selected_target(), Some((20, 0)));
    }

    #[test]
    fn line_mode_parses_the_target_line() {
        let mut p = picker();
        for c in ":42".chars() {
            p.push_char(c);
        }
        assert!(p.is_line_mode());
        assert_eq!(p.line_target(), Some(42));
    }

    #[test]
    fn line_mode_with_no_digits_has_no_target() {
        let mut p = picker();
        p.push_char(':');
        assert!(p.is_line_mode());
        assert_eq!(p.line_target(), None);
    }

    #[test]
    fn selected_target_follows_navigation() {
        let mut p = picker();
        p.select_next();
        assert_eq!(p.selected_target(), Some((5, 0)));
        p.select_prev();
        assert_eq!(p.selected_target(), Some((1, 0)));
    }
}
