//! The bottom panel's OUTPUT tab: a read-only, VS Code-style log viewer over
//! the in-process [`crate::output`] bus. A toolbar row carries a channel
//! dropdown (one channel per language server, plus Debug Adapter, Git, and
//! Server Provisioning), a minimum-level filter, an RPC-trace toggle (Zed's
//! RPC-messages equivalent), and a clear action; the rest of the area renders
//! the selected channel's tail, level-coloured, auto-following new lines until
//! the user scrolls up. The rows are a pure projection of the bus, refreshed
//! whenever its generation counter advances.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::output::{OutputLevel, OutputLine};
use crate::theme::Theme;
use crate::widgets::scrollbar;

const COLOR_DIM: Color = Color::Rgb(0x60, 0x68, 0x78);
const COLOR_MSG: Color = Color::Rgb(0xCC, 0xCC, 0xCC);
const COLOR_ERROR: Color = Color::Rgb(0xf1, 0x4c, 0x4c);
const COLOR_WARNING: Color = Color::Rgb(0xcc, 0xa7, 0x00);
const COLOR_INFO: Color = Color::Rgb(0x3b, 0x9e, 0xff);

const CHEVRON_DOWN: char = '\u{eab4}'; // codicon chevron-down
const CHEVRON_UP: char = '\u{eab7}'; // codicon chevron-up

fn rect_has(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0 && r.height > 0 && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

fn level_color(level: OutputLevel, theme: crate::theme::Theme) -> Color {
    match level {
        OutputLevel::Error => theme.ui(COLOR_ERROR),
        OutputLevel::Warn => theme.ui(COLOR_WARNING),
        OutputLevel::Info => theme.ui(COLOR_INFO),
        OutputLevel::Debug | OutputLevel::Trace => theme.ui(COLOR_DIM),
    }
}

pub struct OutputPanel {
    /// Channel names, mirrored from the bus (sorted) on each generation change.
    channels: Vec<String>,
    /// Index into `channels` of the selected channel.
    selected: usize,
    /// Lines of the selected channel, snapshotted from the bus.
    lines: Vec<OutputLine>,
    /// Last bus generation we synced against, so an unchanged bus is a no-op.
    last_gen: u64,
    /// Minimum level shown; lines below it are filtered out (VS Code per-channel
    /// log level / Neovim `set_log_level`). Trace shows everything.
    min_level: OutputLevel,
    scroll: usize,
    /// Pin the viewport to the newest line until the user scrolls up.
    follow_tail: bool,
    dropdown_open: bool,
    /// First channel row visible in the open dropdown (#45): with more
    /// channels than the body is tall, the wheel (and PageUp/Down) moves
    /// this window instead of the log, so every channel stays reachable.
    /// Reset to the selection on every open; clamped at render.
    dropdown_scroll: usize,
    /// The open dropdown's scrollbar track, for click-to-jump. Empty when
    /// the list fits.
    dropdown_scrollbar: Rect,

    pub focus_gradient: bool,
    pub theme: Theme,
    pub focused: bool,
    pub hover_pointer: Option<(u16, u16)>,

    pub last_area: Rect,
    pub last_scrollbar: Rect,

    // Toolbar hit rects, recomputed each render.
    dropdown_rect: Rect,
    level_rect: Rect,
    rpc_rect: Rect,
    clear_rect: Rect,
    /// When the dropdown is open, the rect + channel index of each list row.
    dropdown_items: Vec<(Rect, usize)>,

    first_row_y: u16,
    visible_rows: u16,
    viewport_rows: u16,
}

impl OutputPanel {
    pub fn new() -> Self {
        Self {
            channels: Vec::new(),
            selected: 0,
            lines: Vec::new(),
            last_gen: 0,
            min_level: OutputLevel::Trace,
            scroll: 0,
            follow_tail: true,
            dropdown_open: false,
            dropdown_scroll: 0,
            dropdown_scrollbar: Rect::default(),
            focus_gradient: false,
            theme: Theme::default(),
            focused: false,
            hover_pointer: None,
            last_area: Rect::default(),
            last_scrollbar: Rect::default(),
            dropdown_rect: Rect::default(),
            level_rect: Rect::default(),
            rpc_rect: Rect::default(),
            clear_rect: Rect::default(),
            dropdown_items: Vec::new(),
            first_row_y: 0,
            visible_rows: 0,
            viewport_rows: 0,
        }
    }

    /// Pull the latest channel set and selected channel's lines from the bus,
    /// but only when its generation advanced (one relaxed atomic load otherwise).
    /// Preserves the selected channel across list changes by name. Returns
    /// whether anything changed, so the caller forces a redraw only on a real
    /// update.
    pub fn sync(&mut self) -> bool {
        let cur_gen = crate::output::generation();
        if cur_gen == self.last_gen && !self.channels.is_empty() {
            return false;
        }
        self.last_gen = cur_gen;
        let prev = self.selected_name();
        let names = crate::output::channel_names();
        if names == self.channels && prev.is_some() {
            // Only the line contents of an existing channel changed.
            self.refresh_lines();
            return true;
        }
        self.channels = names;
        self.selected = prev
            .and_then(|name| self.channels.iter().position(|c| *c == name))
            .unwrap_or(0);
        if self.selected >= self.channels.len() {
            self.selected = self.channels.len().saturating_sub(1);
        }
        self.refresh_lines();
        true
    }

    fn refresh_lines(&mut self) {
        self.lines = self
            .selected_name()
            .and_then(|c| crate::output::snapshot(&c))
            .unwrap_or_default();
    }

    pub fn selected_name(&self) -> Option<String> {
        self.channels.get(self.selected).cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    pub fn dropdown_open(&self) -> bool {
        self.dropdown_open
    }

    pub fn min_level(&self) -> OutputLevel {
        self.min_level
    }

    /// Select a channel by name, if present. Returns whether it was found.
    pub fn select_by_name(&mut self, name: &str) -> bool {
        if let Some(idx) = self.channels.iter().position(|c| c == name) {
            self.select(idx);
            true
        } else {
            false
        }
    }

    /// Select a channel by index, closing the dropdown and re-following the tail.
    pub fn select(&mut self, idx: usize) {
        if idx < self.channels.len() {
            self.selected = idx;
            self.dropdown_open = false;
            self.follow_tail = true;
            self.scroll = 0;
            self.refresh_lines();
        }
    }

    /// Lines passing the current minimum-level filter, in arrival order.
    fn filtered(&self) -> Vec<&OutputLine> {
        self.lines
            .iter()
            .filter(|l| l.level >= self.min_level)
            .collect()
    }

    fn cycle_level(&mut self) {
        self.min_level = match self.min_level {
            OutputLevel::Trace => OutputLevel::Debug,
            OutputLevel::Debug => OutputLevel::Info,
            OutputLevel::Info => OutputLevel::Warn,
            OutputLevel::Warn => OutputLevel::Error,
            OutputLevel::Error => OutputLevel::Trace,
        };
    }

    pub fn scroll_up(&mut self, n: usize) {
        // An open channel list owns the wheel: scrolling must move the
        // list's window, never the log underneath it (#45).
        if self.dropdown_open {
            self.dropdown_scroll = self.dropdown_scroll.saturating_sub(n);
            return;
        }
        self.scroll = self.scroll.saturating_sub(n);
        self.follow_tail = false;
    }

    pub fn scroll_down(&mut self, n: usize) {
        if self.dropdown_open {
            // Loose clamp; the render clamps exactly against the body
            // height it is given.
            self.dropdown_scroll =
                (self.dropdown_scroll + n).min(self.channels.len().saturating_sub(1));
            return;
        }
        self.scroll = (self.scroll + n).min(self.max_scroll());
        if self.scroll >= self.max_scroll() {
            self.follow_tail = true;
        }
    }

    fn max_scroll(&self) -> usize {
        self.filtered()
            .len()
            .saturating_sub(self.viewport_rows as usize)
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let Some(metrics) = scrollbar::vertical_metrics(
            self.last_scrollbar,
            self.filtered().len(),
            self.viewport_rows as usize,
            self.scroll,
        ) else {
            return false;
        };
        let target = scrollbar::scroll_for_y(metrics, y);
        let moved = target != self.scroll;
        self.scroll = target;
        self.follow_tail = target >= self.max_scroll();
        moved
    }

    /// Handle a click at `(x, y)`. Returns true if it landed on the toolbar or
    /// the open dropdown (and was consumed); false if the caller should treat it
    /// as a click in the log body (e.g. to focus the pane).
    pub fn click(&mut self, x: u16, y: u16) -> bool {
        if self.dropdown_open {
            if let Some(&(_, idx)) = self.dropdown_items.iter().find(|(r, _)| rect_has(*r, x, y)) {
                self.select(idx);
                return true;
            }
            // A click on the list's scrollbar track jumps the window.
            if rect_has(self.dropdown_scrollbar, x, y) {
                if let Some(metrics) = scrollbar::vertical_metrics(
                    self.dropdown_scrollbar,
                    self.channels.len(),
                    self.dropdown_scrollbar.height as usize,
                    self.dropdown_scroll,
                ) {
                    self.dropdown_scroll = scrollbar::scroll_for_y(metrics, y);
                }
                return true;
            }
            // A click anywhere else closes the dropdown without selecting.
            self.dropdown_open = false;
            return true;
        }
        if rect_has(self.dropdown_rect, x, y) {
            self.dropdown_open = !self.dropdown_open;
            if self.dropdown_open {
                // Open with the active channel in view however deep it
                // sits; the render clamps to the last full window.
                self.dropdown_scroll = self.selected;
            }
            return true;
        }
        if rect_has(self.clear_rect, x, y) {
            if let Some(c) = self.selected_name() {
                crate::output::clear(&c);
            }
            return true;
        }
        if rect_has(self.level_rect, x, y) {
            self.cycle_level();
            return true;
        }
        if rect_has(self.rpc_rect, x, y) {
            crate::output::set_trace_enabled(!crate::output::trace_enabled());
            return true;
        }
        false
    }
}

impl Default for OutputPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &mut OutputPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.last_area = area;
        self.last_scrollbar = Rect::default();
        self.dropdown_rect = Rect::default();
        self.dropdown_scrollbar = Rect::default();
        self.level_rect = Rect::default();
        self.rpc_rect = Rect::default();
        self.clear_rect = Rect::default();
        self.dropdown_items.clear();
        self.visible_rows = 0;
        if area.width == 0 || area.height == 0 {
            return;
        }
        let accent = if self.focus_gradient {
            crate::gradient::rgb_color(crate::gradient::PANEL_TITLE_FG)
        } else {
            self.theme.ui(Color::White)
        };
        buf.set_style(area, Style::default().bg(self.theme.editor_bg()));

        // Toolbar row: dropdown on the left, actions on the right.
        let toolbar = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        self.render_toolbar(toolbar, buf, accent);

        let body = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: area.height.saturating_sub(1),
        };
        if body.height == 0 {
            return;
        }

        if self.channels.is_empty() {
            Paragraph::new(Line::from(Span::styled(
                "No output yet. Channels appear as language servers and tasks log.",
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            )))
            .render(body, buf);
            return;
        }

        // Indices (owned, no borrow of `self`) of the lines passing the filter,
        // so the field writes below don't conflict with a live borrow.
        let filtered: Vec<usize> = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.level >= self.min_level)
            .map(|(i, _)| i)
            .collect();
        self.first_row_y = body.y;
        self.viewport_rows = body.height;
        let total = filtered.len();
        if self.follow_tail {
            self.scroll = total.saturating_sub(body.height as usize);
        }
        self.scroll = self.scroll.min(total.saturating_sub(body.height as usize));

        let bar = scrollbar::vertical_metrics(
            Rect {
                x: body.x + body.width.saturating_sub(1),
                y: body.y,
                width: 1,
                height: body.height,
            },
            total,
            body.height as usize,
            self.scroll,
        );
        let content_w = body.width.saturating_sub(u16::from(bar.is_some()));

        let visible = (body.height as usize).min(total.saturating_sub(self.scroll));
        self.visible_rows = visible as u16;
        for row in 0..visible {
            let line = &self.lines[filtered[self.scroll + row]];
            let y = body.y + row as u16;
            let row_rect = Rect {
                x: body.x,
                y,
                width: content_w,
                height: 1,
            };
            let spans = vec![
                Span::styled(
                    format!("{} ", line.level.label()),
                    Style::default().fg(level_color(line.level, self.theme)),
                ),
                Span::styled(
                    line.text.replace('\n', " "),
                    Style::default().fg(self.theme.ui(COLOR_MSG)),
                ),
            ];
            Paragraph::new(Line::from(spans)).render(row_rect, buf);
        }

        if let Some(metrics) = bar {
            self.last_scrollbar = metrics.area;
            scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
        }

        // The dropdown overlays the body, so paint it last.
        if self.dropdown_open {
            self.render_dropdown(body, buf, accent);
        }
    }
}

impl OutputPanel {
    fn render_toolbar(&mut self, area: Rect, buf: &mut Buffer, accent: Color) {
        let chevron = if self.dropdown_open {
            CHEVRON_UP
        } else {
            CHEVRON_DOWN
        };
        let name = self.selected_name().unwrap_or_else(|| "Output".to_string());
        let label = format!("{chevron} {name} ");
        let w = label.chars().count() as u16;
        self.dropdown_rect = Rect {
            x: area.x + 1,
            y: area.y,
            width: w.min(area.width.saturating_sub(1)),
            height: 1,
        };
        buf.set_stringn(
            self.dropdown_rect.x,
            area.y,
            &label,
            self.dropdown_rect.width as usize,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        );

        // Right-aligned action tokens: clear | rpc | level.
        let rpc_on = crate::output::trace_enabled();
        let clear_tok = " clear ".to_string();
        let rpc_tok = if rpc_on { " ●rpc " } else { " rpc " }.to_string();
        let level_tok = format!(" ≥{} ", self.min_level.label());
        let mut x = area.x + area.width;
        for (tok, slot) in [(clear_tok, 0u8), (rpc_tok, 1), (level_tok, 2)] {
            let tw = tok.chars().count() as u16;
            if x.saturating_sub(area.x) < tw + self.dropdown_rect.width + 1 {
                break;
            }
            x -= tw;
            let rect = Rect {
                x,
                y: area.y,
                width: tw,
                height: 1,
            };
            let hovered =
                crate::widgets::hover::row_hover_bg(rect, self.hover_pointer, self.theme).is_some();
            let on = slot == 1 && rpc_on;
            let fg = if on || hovered {
                accent
            } else {
                self.theme.ui(COLOR_DIM)
            };
            buf.set_stringn(rect.x, area.y, &tok, tw as usize, Style::default().fg(fg));
            match slot {
                0 => self.clear_rect = rect,
                1 => self.rpc_rect = rect,
                _ => self.level_rect = rect,
            }
        }
    }

    fn render_dropdown(&mut self, body: Rect, buf: &mut Buffer, accent: Color) {
        let n = self.channels.len() as u16;
        let h = n.min(body.height);
        // One extra column carries the scrollbar when the list overflows,
        // so the longest channel name is never overpainted by the track.
        let overflow = n > h;
        let w = self
            .channels
            .iter()
            .map(|c| c.chars().count() as u16 + 2)
            .max()
            .unwrap_or(8)
            .saturating_add(overflow as u16)
            .min(body.width);
        let area = Rect {
            x: body.x + 1,
            y: body.y,
            width: w,
            height: h,
        };
        // Clamp the window so the last page is always full; the wheel
        // clamps loosely and this is the exact bound (#45).
        self.dropdown_scroll = self
            .dropdown_scroll
            .min((n as usize).saturating_sub(h as usize));
        buf.set_style(area, Style::default().bg(self.theme.editor_bg()));
        crate::gradient::paint_gradient_box(
            buf,
            Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: area.height,
            },
        );
        if overflow {
            let track = Rect {
                x: area.x + area.width - 1,
                y: area.y,
                width: 1,
                height: area.height,
            };
            self.dropdown_scrollbar = track;
            if let Some(metrics) =
                scrollbar::vertical_metrics(track, n as usize, h as usize, self.dropdown_scroll)
            {
                scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
            }
        }
        for (row, name) in self
            .channels
            .iter()
            .enumerate()
            .skip(self.dropdown_scroll)
            .take(h as usize)
        {
            let y = area.y + (row - self.dropdown_scroll) as u16;
            let r = Rect {
                x: area.x,
                y,
                // The scrollbar column is its own hit target, not the row's.
                width: area.width - overflow as u16,
                height: 1,
            };
            self.dropdown_items.push((r, row));
            let selected = row == self.selected;
            let hovered =
                crate::widgets::hover::row_hover_bg(r, self.hover_pointer, self.theme).is_some();
            if hovered || selected {
                buf.set_style(
                    r,
                    Style::default().bg(crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG)),
                );
            }
            let fg = if selected {
                accent
            } else {
                self.theme.ui(COLOR_MSG)
            };
            buf.set_stringn(
                area.x + 1,
                y,
                name,
                r.width.saturating_sub(1) as usize,
                Style::default().fg(fg),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(p: &mut OutputPanel, w: u16, h: u16) -> String {
        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);
        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    /// #45: with more channels than the panel body is tall, the overflow
    /// rows were neither drawn nor registered as hit targets — unreachable
    /// by any gesture, with nothing indicating truncation. The wheel over
    /// the open dropdown must scroll the LIST (not the log) and give every
    /// channel a clickable rect.
    #[test]
    fn dropdown_overflow_channels_are_reachable_by_scrolling() {
        let mut p = OutputPanel::new();
        p.channels = (0..12).map(|i| format!("chan-{i:02}")).collect();
        p.dropdown_open = true;
        // 7 rows total: 1 toolbar + 6 body — six of twelve channels fit.
        let _ = render(&mut p, 40, 7);
        let visible: Vec<usize> = p.dropdown_items.iter().map(|&(_, i)| i).collect();
        assert!(
            !visible.contains(&11),
            "staging: the last channel starts off-screen; got {visible:?}"
        );
        p.scroll_down(6);
        let _ = render(&mut p, 40, 7);
        let visible: Vec<usize> = p.dropdown_items.iter().map(|&(_, i)| i).collect();
        assert!(
            visible.contains(&11),
            "scrolling must bring the last channel into reach; got {visible:?}"
        );
        let &(r, _) = p.dropdown_items.iter().find(|&&(_, i)| i == 11).unwrap();
        assert!(p.click(r.x, r.y), "the row consumes the click");
        assert_eq!(
            p.selected, 11,
            "the click lands on the scrolled row's channel"
        );
        // Selecting closed the dropdown; the log wheel works again.
        assert!(!p.dropdown_open);
    }

    /// Opening the dropdown starts the window AT the current selection, so
    /// the active channel is immediately visible however deep it sits.
    #[test]
    fn dropdown_opens_with_the_selection_in_view() {
        let mut p = OutputPanel::new();
        p.channels = (0..12).map(|i| format!("chan-{i:02}")).collect();
        p.selected = 10;
        let _ = render(&mut p, 40, 7);
        // Toggle open through the real gesture: a click on the picker.
        let r = p.dropdown_rect;
        assert!(p.click(r.x, r.y), "the picker consumes the click");
        assert!(p.dropdown_open);
        let _ = render(&mut p, 40, 7);
        let visible: Vec<usize> = p.dropdown_items.iter().map(|&(_, i)| i).collect();
        assert!(
            visible.contains(&10),
            "the selected channel must be in the opening window; got {visible:?}"
        );
    }

    #[test]
    fn empty_shows_a_hint() {
        let mut p = OutputPanel::new();
        let text = render(&mut p, 70, 4);
        assert!(text.contains("No output yet"), "empty state: {text:?}");
    }

    #[test]
    fn sync_picks_up_a_new_channel_and_renders_its_lines() {
        let ch = "widget-test-render";
        crate::output::push(ch, OutputLevel::Error, "kaboom here");
        let mut p = OutputPanel::new();
        assert!(p.sync(), "sync sees the new generation");
        // Select our channel by name (other tests may have added channels).
        let idx = p
            .channels
            .iter()
            .position(|c| c == ch)
            .expect("channel present");
        p.select(idx);
        let text = render(&mut p, 80, 6);
        assert!(text.contains("kaboom here"), "renders the line: {text:?}");
        assert!(text.contains("error"), "renders the level tag: {text:?}");
    }

    #[test]
    fn level_filter_hides_lower_levels() {
        let ch = "widget-test-filter";
        crate::output::push(ch, OutputLevel::Debug, "noisy-debug-line");
        crate::output::push(ch, OutputLevel::Error, "the-error-line");
        let mut p = OutputPanel::new();
        p.sync();
        let idx = p.channels.iter().position(|c| c == ch).unwrap();
        p.select(idx);
        // Default Trace shows both.
        assert!(render(&mut p, 80, 6).contains("noisy-debug-line"));
        // Cycle Trace -> Debug -> Info: now Debug is filtered out, Error stays.
        p.cycle_level();
        p.cycle_level();
        let text = render(&mut p, 80, 6);
        assert!(
            !text.contains("noisy-debug-line"),
            "debug filtered: {text:?}"
        );
        assert!(text.contains("the-error-line"), "error kept: {text:?}");
    }

    #[test]
    fn clicking_the_dropdown_toggles_it() {
        let ch = "widget-test-dropdown";
        crate::output::push(ch, OutputLevel::Info, "x");
        let mut p = OutputPanel::new();
        p.sync();
        render(&mut p, 80, 6);
        assert!(!p.dropdown_open());
        let r = p.dropdown_rect;
        assert!(r.width > 0);
        assert!(p.click(r.x, r.y), "dropdown click is consumed");
        assert!(p.dropdown_open(), "dropdown opens");
    }

    #[test]
    fn clicking_rpc_toggles_trace() {
        let ch = "widget-test-rpc";
        crate::output::push(ch, OutputLevel::Info, "x");
        crate::output::set_trace_enabled(false);
        let mut p = OutputPanel::new();
        p.sync();
        render(&mut p, 80, 6);
        let r = p.rpc_rect;
        assert!(r.width > 0, "rpc token rendered");
        p.click(r.x, r.y);
        assert!(crate::output::trace_enabled(), "rpc click enables trace");
        crate::output::set_trace_enabled(false);
    }
}
