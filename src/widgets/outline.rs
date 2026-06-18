//! The Explorer's OUTLINE section: a collapsible panel below the file tree
//! that lists the active editor's symbols (`textDocument/documentSymbol`), the
//! same data behind VS Code's Outline view. The LSP worker flattens the
//! server's symbol tree into a depth-tagged [`OutlineSymbol`] list; this widget
//! renders it as an indented, kind-iconed list, highlights the symbol the
//! editor caret currently sits in (follow-cursor), and reports row hit-tests so
//! a click jumps the editor to that symbol.

use std::path::PathBuf;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

use crate::icons;
use crate::lsp::manager::OutlineSymbol;
use crate::theme::Theme;
use crate::widgets::scrollbar;

const COLOR_HEADER: Color = Color::Rgb(0xE8, 0xEE, 0xF8);
const COLOR_DIM: Color = Color::Rgb(0x60, 0x68, 0x78);
const COLOR_NAME: Color = Color::Rgb(0xCC, 0xCC, 0xCC);

/// Left indent matching the tree's `Borders::ALL` inset (this panel draws only
/// a bottom separator) so symbol rows line up under the file rows above.
const CONTENT_INDENT: u16 = 1;

pub struct OutlinePanel {
    /// Section collapsed to just its header. Defaults to `true` (VS Code's
    /// SYSTEM-style chevron strip) so an empty/new workspace shows the header
    /// without stealing rows from the tree until the user expands it.
    pub collapsed: bool,
    /// The file the current symbols describe; `None` until the first reply.
    pub path: Option<PathBuf>,
    symbols: Vec<OutlineSymbol>,
    /// The symbol whose `range` encloses the editor caret — highlighted and
    /// kept scrolled into view (follow-cursor). `None` when the caret is
    /// outside every symbol or there are no symbols.
    current: Option<usize>,
    scroll: usize,
    /// `true` once a reply for the active file has arrived (even an empty one),
    /// so the panel can distinguish "still loading" from "no symbols here".
    loaded: bool,
    /// Black theme: highlight rows with the brand's dark-teal instead of the
    /// legacy blue. Mirrors `FileTree::focus_gradient`, set by the app.
    pub focus_gradient: bool,
    /// Active color theme, driving the scrollbar track/thumb colours so the
    /// outline's lane matches the file tree's directly above it. Set by the app.
    pub theme: Theme,
    /// Whether the Explorer/tree has focus, for the brighter scrollbar thumb.
    /// Mirrors `FileTree::focused`; set by the app each frame.
    pub focused: bool,
    /// Render-time pointer cell so the row under the cursor lights up, fed by
    /// the app each frame exactly like the other side panels.
    pub hover_pointer: Option<(u16, u16)>,

    // Hit-test rects, refreshed every render.
    pub last_area: Rect,
    /// The scrollbar track rect, for click-to-position; empty when the whole
    /// outline fits and no bar is drawn.
    pub last_scrollbar: Rect,
    last_header_row: u16,
    last_header_x: u16,
    last_header_w: u16,
    /// Y of the first symbol row and how many rows are visible, so a click can
    /// be mapped back to a symbol index.
    first_row_y: u16,
    visible_rows: u16,
    /// The symbol-row capacity of the body at the last render (independent of
    /// the current scroll), so wheel-scroll can clamp to the last row without
    /// knowing the layout at input time.
    viewport_rows: u16,
}

impl OutlinePanel {
    pub fn new() -> Self {
        Self {
            collapsed: true,
            path: None,
            symbols: Vec::new(),
            current: None,
            scroll: 0,
            loaded: false,
            focus_gradient: false,
            theme: Theme::default(),
            focused: false,
            hover_pointer: None,
            last_area: Rect::default(),
            last_scrollbar: Rect::default(),
            last_header_row: 0,
            last_header_x: 0,
            last_header_w: 0,
            first_row_y: 0,
            visible_rows: 0,
            viewport_rows: 0,
        }
    }

    /// Replace the outline with an authoritative language-server reply for
    /// `path` (always marks the panel loaded). Resets scroll/current when the
    /// file changed; preserves them when it is the same file being re-analysed
    /// after an edit so the view does not jump.
    pub fn set_symbols(&mut self, path: PathBuf, symbols: Vec<OutlineSymbol>) {
        self.adopt(path, symbols);
        self.loaded = true;
    }

    /// Paint a provisional outline computed locally from the buffer's syntax
    /// tree, before any language-server reply (tree-sitter-first, the way Zed
    /// and aerial.nvim source their outlines). `lsp_pending` is `true` when a
    /// server tracks this file and will refine the outline shortly; if so and
    /// the syntax pass found nothing, the panel keeps showing "Loading…"
    /// instead of a premature "No symbols". When no server will reply, an empty
    /// pass settles to "No symbols" exactly as before.
    pub fn set_syntax_symbols(
        &mut self,
        path: PathBuf,
        symbols: Vec<OutlineSymbol>,
        lsp_pending: bool,
    ) {
        let settled = !symbols.is_empty() || !lsp_pending;
        self.adopt(path, symbols);
        self.loaded = settled;
    }

    /// Shared body of [`set_symbols`]/[`set_syntax_symbols`]: swap in the new
    /// symbols and reset the view only when the file actually changed.
    fn adopt(&mut self, path: PathBuf, symbols: Vec<OutlineSymbol>) {
        let same_file = self.path.as_ref() == Some(&path);
        self.path = Some(path);
        self.symbols = symbols;
        if !same_file {
            self.scroll = 0;
            self.current = None;
        } else if self.current.is_some_and(|i| i >= self.symbols.len()) {
            self.current = None;
        }
    }

    /// Forget the outline (e.g. the editor returned to the welcome screen).
    pub fn clear(&mut self) {
        self.path = None;
        self.symbols.clear();
        self.current = None;
        self.scroll = 0;
        self.loaded = false;
    }

    pub fn toggle_collapse(&mut self) {
        self.collapsed = !self.collapsed;
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    /// Whether a reply (even an empty one) has been applied for the current
    /// file, so the panel can show "No symbols" instead of "Loading…".
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// Highlight (and scroll to) the innermost symbol whose `range` encloses the
    /// editor caret on `line` (zero-based). The innermost match wins, so the
    /// method inside a class highlights rather than the class. Returns whether
    /// the highlighted symbol changed, so the app can request a redraw.
    pub fn follow_caret(&mut self, line: u32) -> bool {
        let mut best: Option<usize> = None;
        let mut best_span = u32::MAX;
        for (i, s) in self.symbols.iter().enumerate() {
            if line >= s.range_start_line && line <= s.range_end_line {
                let span = s.range_end_line - s.range_start_line;
                if span <= best_span {
                    best_span = span;
                    best = Some(i);
                }
            }
        }
        let changed = best != self.current;
        self.current = best;
        // Only pull the view to the caret's symbol when that symbol actually
        // changes; otherwise follow-cursor (which runs every frame) would fight
        // a manual wheel-scroll, yanking the view back each tick.
        if changed && let Some(i) = best {
            self.ensure_visible(i);
        }
        changed
    }

    fn ensure_visible(&mut self, idx: usize) {
        if idx < self.scroll {
            self.scroll = idx;
        } else if self.visible_rows > 0 && idx >= self.scroll + self.visible_rows as usize {
            self.scroll = idx + 1 - self.visible_rows as usize;
        }
    }

    /// Scroll the symbol list by `n` rows (mouse wheel), clamped so the last
    /// row can reach the top of the viewport but no further. A no-op while the
    /// whole outline already fits.
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.max_scroll());
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    fn max_scroll(&self) -> usize {
        self.symbols
            .len()
            .saturating_sub(self.viewport_rows as usize)
    }

    /// Jump the scroll so the thumb centers on track row `y` (click/drag on the
    /// scrollbar lane). No-op when no bar is drawn. Returns whether it moved.
    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let Some(metrics) = scrollbar::vertical_metrics(
            self.last_scrollbar,
            self.symbols.len(),
            self.viewport_rows as usize,
            self.scroll,
        ) else {
            return false;
        };
        let target = scrollbar::scroll_for_y(metrics, y);
        let moved = target != self.scroll;
        self.scroll = target;
        moved
    }

    /// Height the section wants given the rows available to it. The widget
    /// draws a `Borders::BOTTOM` separator, so every layout reserves one row
    /// beyond the visible content for it (mirroring `SystemPanel`). Collapsed
    /// is the header plus that separator; expanded adds one row per symbol (or a
    /// single "no symbols" row), capped at **half** the shared region so a long
    /// outline can squeeze the file tree to a 50/50 split but never past it.
    /// The outline then scrolls within its half.
    pub fn desired_height(&self, available: u16) -> u16 {
        const BORDER: u16 = 1;
        if available == 0 {
            return 0;
        }
        let header = 1u16;
        let floor = header + BORDER;
        if self.collapsed {
            return floor.min(available);
        }
        let content = if self.symbols.is_empty() {
            1
        } else {
            self.symbols.len() as u16
        };
        let half = (available / 2).max(floor);
        (header + content + BORDER).min(half)
    }

    pub fn hit_header(&self, x: u16, y: u16) -> bool {
        y == self.last_header_row
            && x >= self.last_header_x
            && x < self.last_header_x.saturating_add(self.last_header_w)
    }

    /// Map a click row to the symbol index it lands on, if any.
    pub fn row_at(&self, y: u16) -> Option<usize> {
        if self.collapsed || self.visible_rows == 0 || y < self.first_row_y {
            return None;
        }
        let offset = (y - self.first_row_y) as usize;
        if offset >= self.visible_rows as usize {
            return None;
        }
        let idx = self.scroll + offset;
        (idx < self.symbols.len()).then_some(idx)
    }

    /// The caret target (file + zero-based line/character) for the symbol at
    /// `idx`, for jumping the editor on click.
    pub fn jump_target(&self, idx: usize) -> Option<(PathBuf, u32, u32)> {
        let s = self.symbols.get(idx)?;
        let path = self.path.clone()?;
        Some((path, s.line, s.character))
    }
}

impl Default for OutlinePanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &mut OutlinePanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(COLOR_DIM));
        let inner = block.inner(area);
        block.render(area, buf);
        self.last_area = area;
        self.last_scrollbar = Rect::default();
        self.visible_rows = 0;
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let inner = Rect {
            x: inner.x + CONTENT_INDENT.min(inner.width),
            width: inner.width.saturating_sub(CONTENT_INDENT),
            ..inner
        };

        // Header: chevron + OUTLINE, matching the SYSTEM panel's styling.
        let chevron = if self.collapsed {
            icons::CHEVRON_CLOSED
        } else {
            icons::CHEVRON_OPEN
        };
        let header_y = inner.y;
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{chevron} "), Style::default().fg(COLOR_DIM)),
            Span::styled(
                "OUTLINE",
                Style::default()
                    .fg(COLOR_HEADER)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .render(
            Rect {
                x: inner.x,
                y: header_y,
                width: inner.width,
                height: 1,
            },
            buf,
        );
        self.last_header_row = header_y;
        self.last_header_x = inner.x;
        self.last_header_w = inner.width;

        if self.collapsed || inner.height < 2 {
            return;
        }

        let body_y = header_y + 1;
        let body_h = inner.height - 1;
        self.first_row_y = body_y;
        self.viewport_rows = body_h;

        // Empty / loading message in place of rows. "Loading…" is only honest
        // while an active file is still awaiting its first symbols; with no
        // file open (welcome screen) there is nothing to load, so show
        // "No symbols" instead of spinning forever.
        if self.symbols.is_empty() {
            let msg = if self.path.is_some() && !self.loaded {
                "Loading…"
            } else {
                "No symbols"
            };
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(COLOR_DIM),
            )))
            .render(
                Rect {
                    x: inner.x,
                    y: body_y,
                    width: inner.width,
                    height: 1,
                },
                buf,
            );
            return;
        }

        // Clamp before measuring so the thumb and the drawn rows agree, then
        // size the scrollbar over the body's rightmost column. When it shows,
        // the rows give up that column so text never paints under the lane.
        self.scroll = self
            .scroll
            .min(self.symbols.len().saturating_sub(body_h as usize));
        let bar = scrollbar::vertical_metrics(
            Rect {
                x: inner.x + inner.width.saturating_sub(1),
                y: body_y,
                width: 1,
                height: body_h,
            },
            self.symbols.len(),
            body_h as usize,
            self.scroll,
        );
        let content_w = inner.width.saturating_sub(u16::from(bar.is_some()));

        let visible = (body_h as usize).min(self.symbols.len().saturating_sub(self.scroll));
        self.visible_rows = visible as u16;
        let brand = self.focus_gradient;
        let sel_bg = if brand {
            crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG)
        } else {
            Color::Rgb(0x09, 0x4d, 0x77)
        };

        for row in 0..visible {
            let idx = self.scroll + row;
            let sym = &self.symbols[idx];
            let y = body_y + row as u16;
            let row_rect = Rect {
                x: inner.x,
                y,
                width: content_w,
                height: 1,
            };

            // Indent two cells per depth, but never push the icon off-screen.
            let max_depth = (content_w / 2).saturating_sub(2) as usize;
            let depth = (sym.depth as usize).min(max_depth);
            let icon = icons::for_outline_kind(sym.kind);
            let mut spans: Vec<Span> = Vec::with_capacity(4);
            spans.push(Span::raw("  ".repeat(depth)));
            spans.push(Span::styled(
                format!("{} ", icon.glyph),
                Style::default().fg(icon.color),
            ));
            spans.push(Span::styled(
                sym.name.clone(),
                Style::default().fg(COLOR_NAME),
            ));
            if let Some(detail) = &sym.detail
                && !detail.is_empty()
            {
                spans.push(Span::styled(
                    format!(" {detail}"),
                    Style::default().fg(COLOR_DIM),
                ));
            }

            let style = if self.current == Some(idx) {
                Style::default().bg(sel_bg)
            } else if let Some(bg) =
                crate::widgets::hover::row_hover_bg(row_rect, self.hover_pointer, brand)
            {
                Style::default().bg(bg)
            } else {
                Style::default()
            };
            buf.set_style(row_rect, style);
            Paragraph::new(Line::from(spans)).render(row_rect, buf);
        }

        if let Some(metrics) = bar {
            self.last_scrollbar = metrics.area;
            scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::manager::OutlineKind;

    fn sym(name: &str, kind: OutlineKind, depth: u16, start: u32, end: u32) -> OutlineSymbol {
        OutlineSymbol {
            name: name.to_string(),
            detail: None,
            kind,
            depth,
            line: start,
            character: 0,
            range_start_line: start,
            range_end_line: end,
        }
    }

    #[test]
    fn collapsed_is_header_plus_separator_expanded_grows() {
        let mut p = OutlinePanel::new();
        p.set_symbols(
            "a.rs".into(),
            vec![sym("f", OutlineKind::Function, 0, 1, 3)],
        );
        assert_eq!(
            p.desired_height(40),
            2,
            "collapsed shows the header plus the bottom separator row"
        );
        p.toggle_collapse();
        assert_eq!(
            p.desired_height(40),
            3,
            "header + one symbol row + separator"
        );
    }

    #[test]
    fn expanded_caps_at_half_the_region() {
        let mut p = OutlinePanel::new();
        let syms: Vec<_> = (0..50)
            .map(|i| sym(&format!("f{i}"), OutlineKind::Function, 0, i, i))
            .collect();
        p.set_symbols("a.rs".into(), syms);
        p.toggle_collapse();
        // A 50-symbol outline wants far more than 20 rows, but must never take
        // more than half the shared region, leaving the other half to the tree.
        assert_eq!(p.desired_height(20), 10);
    }

    #[test]
    fn wheel_scroll_clamps_to_the_last_row() {
        let mut p = OutlinePanel::new();
        let syms: Vec<_> = (0..50)
            .map(|i| sym(&format!("f{i}"), OutlineKind::Function, 0, i, i))
            .collect();
        p.set_symbols("a.rs".into(), syms);
        p.toggle_collapse();
        // Render at 20 rows tall (cap 10 → 9 symbol rows after header), so the
        // viewport holds 9 of the 50 symbols.
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        p.scroll_up(3);
        assert_eq!(p.scroll, 0, "cannot scroll above the first symbol");
        p.scroll_down(1000);
        assert_eq!(
            p.scroll,
            50 - p.viewport_rows as usize,
            "scrolling stops with the last symbol at the viewport top"
        );
    }

    #[test]
    fn empty_expanded_shows_one_message_row() {
        let mut p = OutlinePanel::new();
        p.set_symbols("a.rs".into(), vec![]);
        p.toggle_collapse();
        assert_eq!(
            p.desired_height(40),
            3,
            "header + one 'No symbols' row + separator"
        );
    }

    #[test]
    fn follow_caret_picks_innermost_symbol() {
        let mut p = OutlinePanel::new();
        p.set_symbols(
            "a.rs".into(),
            vec![
                sym("Outer", OutlineKind::Class, 0, 0, 20),
                sym("inner", OutlineKind::Method, 1, 5, 10),
            ],
        );
        assert!(p.follow_caret(7));
        assert_eq!(p.current, Some(1), "innermost (method) wins over the class");
        assert!(p.follow_caret(2));
        assert_eq!(p.current, Some(0), "only the class encloses line 2");
        assert!(!p.follow_caret(2), "no change returns false");
    }

    #[test]
    fn switching_files_resets_scroll_and_current() {
        let mut p = OutlinePanel::new();
        p.set_symbols(
            "a.rs".into(),
            vec![sym("f", OutlineKind::Function, 0, 0, 5)],
        );
        p.follow_caret(2);
        p.scroll = 3;
        p.set_symbols(
            "b.rs".into(),
            vec![sym("g", OutlineKind::Function, 0, 0, 5)],
        );
        assert_eq!(p.scroll, 0);
        assert_eq!(p.current, None);
    }

    #[test]
    fn row_at_maps_clicks_to_symbols() {
        let mut p = OutlinePanel::new();
        p.set_symbols(
            "a.rs".into(),
            vec![
                sym("a", OutlineKind::Function, 0, 0, 1),
                sym("b", OutlineKind::Function, 0, 2, 3),
            ],
        );
        p.toggle_collapse();
        let area = Rect {
            x: 0,
            y: 0,
            width: 30,
            height: 6,
        };
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        // header at y=0, first symbol row at y=1.
        assert_eq!(p.row_at(0), None, "header row is not a symbol");
        assert_eq!(p.row_at(1), Some(0));
        assert_eq!(p.row_at(2), Some(1));
        assert_eq!(p.row_at(3), None, "below the last symbol");
    }

    fn rendered_text(p: &mut OutlinePanel, width: u16, height: u16) -> String {
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn no_open_file_shows_no_symbols_not_loading() {
        // On the welcome screen (no active editor) there is nothing to load, so
        // the expanded panel must read "No symbols", never spin on "Loading…".
        let mut p = OutlinePanel::new();
        p.toggle_collapse();
        let text = rendered_text(&mut p, 24, 6);
        assert!(text.contains("No symbols"), "got:\n{text}");
        assert!(!text.contains("Loading"), "got:\n{text}");
    }

    #[test]
    fn loading_shows_only_while_an_active_file_awaits_symbols() {
        // A served file whose symbols have not arrived yet is the one case that
        // legitimately reads "Loading…".
        let mut p = OutlinePanel::new();
        p.set_syntax_symbols("a.rs".into(), vec![], true);
        p.toggle_collapse();
        let text = rendered_text(&mut p, 24, 6);
        assert!(text.contains("Loading"), "got:\n{text}");
    }

    fn many(n: u32) -> Vec<OutlineSymbol> {
        (0..n)
            .map(|i| sym(&format!("f{i}"), OutlineKind::Function, 0, i, i))
            .collect()
    }

    #[test]
    fn scrollbar_appears_only_when_the_outline_overflows() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 24,
            height: 6,
        };
        // Body holds 4 rows; 3 symbols fit, so no bar is drawn.
        let mut p = OutlinePanel::new();
        p.set_symbols("a.rs".into(), many(3));
        p.toggle_collapse();
        (&mut p).render(area, &mut Buffer::empty(area));
        assert_eq!(
            p.last_scrollbar,
            Rect::default(),
            "no scrollbar while the whole outline fits"
        );

        // 50 symbols overflow the 4-row body, so the bar shows on the rightmost
        // column of the body.
        let mut p = OutlinePanel::new();
        p.set_symbols("a.rs".into(), many(50));
        p.toggle_collapse();
        (&mut p).render(area, &mut Buffer::empty(area));
        assert_ne!(p.last_scrollbar, Rect::default(), "scrollbar on overflow");
        assert_eq!(p.last_scrollbar.width, 1);
        assert_eq!(
            p.last_scrollbar.x,
            area.x + area.width - 1,
            "bar hugs the right edge"
        );
    }

    #[test]
    fn clicking_the_scrollbar_track_jumps_the_scroll() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 24,
            height: 12,
        };
        let mut p = OutlinePanel::new();
        p.set_symbols("a.rs".into(), many(100));
        p.toggle_collapse();
        (&mut p).render(area, &mut Buffer::empty(area));
        let bar = p.last_scrollbar;
        assert!(bar.height > 0, "bar must have rendered");
        p.scroll_to_bar_y(bar.y + bar.height - 1);
        assert!(p.scroll > 0, "clicking low on the track scrolls down");
        p.scroll_to_bar_y(bar.y);
        assert_eq!(p.scroll, 0, "clicking the top returns to the first symbol");
    }

    #[test]
    fn jump_target_carries_selection_position() {
        let mut p = OutlinePanel::new();
        let mut s = sym("f", OutlineKind::Function, 0, 12, 30);
        s.character = 4;
        p.set_symbols("a.rs".into(), vec![s]);
        assert_eq!(p.jump_target(0), Some(("a.rs".into(), 12, 4)));
        assert_eq!(p.jump_target(9), None);
    }
}
