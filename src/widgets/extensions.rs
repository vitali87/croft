//! The Extensions side-panel: a VS Code-style list of bundled and installed
//! extensions, each with an enable/disable toggle switch, plus a filter box that
//! narrows the list as you type.
//!
//! The panel is a pure projection of [`crate::lsp::manifest::summaries`] plus
//! the per-id enabled state held in [`crate::prefs`]; the app feeds both in via
//! [`ExtensionsPanel::set_items`] whenever the view is shown or a toggle flips.
//! The widget never touches prefs itself — it only reports which row the pointer
//! hit and whether the row's toggle switch was clicked, and the app does the
//! actual enable/disable + persistence + feature gating. This mirrors how
//! `SourceControlPanel` reports clicks back to `App` rather than mutating git
//! state in the render path. The text filter is local-only: there is no
//! marketplace, so it narrows the installed list rather than searching a remote
//! registry.

use std::collections::BTreeSet;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};

use crate::lsp::manifest::ExtensionSummary;

// --- Croft-Dark palette, picked from the approved Option-B mockup. Fixed darks
// so the panel reads the same on both the Black (#000) and Dark Blue (#1e222e)
// theme backgrounds; the per-row selection/hover fills are the only painted
// backgrounds (the panel itself inherits the session bg like the other views).
const SECTION_HEADER: Color = Color::Rgb(0xcc, 0xcc, 0xcc); // "EXTENSIONS" caption
const NAME_FG: Color = Color::Rgb(0xff, 0xff, 0xff); // extension display name
const DESC_FG: Color = Color::Rgb(0x79, 0x82, 0x90); // blurb / dim text
const SELECTION_BG: Color = Color::Rgb(0x26, 0x4f, 0x78); // selected row fill
const ICON_CHIP_FG: Color = Color::Rgb(0x6c, 0x7d, 0x9c); // the small leading chip
const SEARCH_BG: Color = Color::Rgb(0x2a, 0x2f, 0x3a); // filter input fill
const PLACEHOLDER_FG: Color = Color::Rgb(0x6c, 0x7d, 0x9c); // filter placeholder
// Toggle switch: a right-aligned pill that reads at a glance — VS Code blue when
// on, neutral grey when off — so state needs no hover and no word label.
const SWITCH_ON_BG: Color = Color::Rgb(0x09, 0x67, 0xb8);
const SWITCH_OFF_BG: Color = Color::Rgb(0x5a, 0x64, 0x72);
const SWITCH_KNOB_ON: Color = Color::Rgb(0xff, 0xff, 0xff);
const SWITCH_KNOB_OFF: Color = Color::Rgb(0xcd, 0xd3, 0xdc);

/// Each list item is two cells tall: name + toggle on the first line, blurb on
/// the second, like VS Code's two-line extension rows.
const ROW_H: u16 = 2;
/// Width in cells of the toggle switch pill (track). The knob slides one cell
/// between off (left) and on (right); colour carries the state at a glance.
const SWITCH_W: u16 = 4;

/// One row in the Extensions list: the manifest's user-facing identity plus the
/// live enabled flag. A plain data projection the app rebuilds each time it
/// shows the panel; the widget owns none of the truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub builtin: bool,
    pub enabled: bool,
}

/// Project the manifest summaries (bundled + user, already hidden-filtered by
/// [`crate::lsp::manifest::summaries`]) onto the panel's row list, stamping each
/// with its live enabled state: everything is enabled unless its id is in the
/// disabled set. The single place the "disabled = opt-in, default enabled" rule
/// is applied for the list view.
pub fn items_from_summaries(
    summaries: Vec<ExtensionSummary>,
    disabled: &BTreeSet<String>,
) -> Vec<ExtensionItem> {
    summaries
        .into_iter()
        .map(|s| ExtensionItem {
            enabled: !disabled.contains(&s.id),
            id: s.id,
            name: s.name,
            description: s.description,
            builtin: s.builtin,
        })
        .collect()
}

pub struct ExtensionsPanel {
    pub focused: bool,
    pub hover_pointer: Option<(u16, u16)>,
    pub last_area: Rect,
    items: Vec<ExtensionItem>,
    /// Local filter query. Narrows the visible list by case-insensitive match on
    /// name / description / id; never searches a remote marketplace (there is
    /// none).
    filter: String,
    /// Selection and scroll index into the *visible* (filtered) list, not the
    /// full `items` — so navigation tracks what the user can actually see.
    selected: usize,
    scroll: usize,
    /// Screen y of the first rendered row and how many rows were drawn, recorded
    /// each frame so a click maps back to a visible-list index.
    last_row_y0: u16,
    last_rows_shown: usize,
    /// Left column of the toggle-switch pill, recorded each frame so a click can
    /// be told apart from a plain row select.
    last_switch_left: u16,
    /// The filter input row and its trailing clear (✕) cell, for click routing.
    last_search_area: Rect,
    last_clear_area: Rect,
}

impl ExtensionsPanel {
    pub fn new() -> Self {
        Self {
            focused: false,
            hover_pointer: None,
            last_area: Rect::default(),
            items: Vec::new(),
            filter: String::new(),
            selected: 0,
            scroll: 0,
            last_row_y0: 0,
            last_rows_shown: 0,
            last_switch_left: u16::MAX,
            last_search_area: Rect::default(),
            last_clear_area: Rect::default(),
        }
    }

    /// Replace the displayed list, preserving the selection by id where possible
    /// (so a toggle that rebuilds the list doesn't jump the cursor) and clamping
    /// it into the visible range.
    pub fn set_items(&mut self, items: Vec<ExtensionItem>) {
        let prior_id = self.selected_id().map(str::to_string);
        self.items = items;
        if let Some(id) = prior_id {
            let visible = self.visible_indices();
            if let Some(pos) = visible.iter().position(|&i| self.items[i].id == id) {
                self.selected = pos;
            }
        }
        self.clamp();
    }

    /// True when the item matches the current filter (or the filter is empty).
    fn matches(&self, item: &ExtensionItem) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        let q = self.filter.to_lowercase();
        item.name.to_lowercase().contains(&q)
            || item.description.to_lowercase().contains(&q)
            || item.id.to_lowercase().contains(&q)
    }

    /// Indices into `items` that pass the current filter, in list order.
    fn visible_indices(&self) -> Vec<usize> {
        (0..self.items.len())
            .filter(|&i| self.matches(&self.items[i]))
            .collect()
    }

    fn visible_len(&self) -> usize {
        if self.filter.is_empty() {
            self.items.len()
        } else {
            self.items.iter().filter(|i| self.matches(i)).count()
        }
    }

    /// Keep `selected` / `scroll` within the visible list after a filter or
    /// content change.
    fn clamp(&mut self) {
        let len = self.visible_len();
        if self.selected >= len {
            self.selected = len.saturating_sub(1);
        }
        if self.scroll > self.selected {
            self.scroll = self.selected;
        }
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.visible_indices()
            .get(self.selected)
            .map(|&i| self.items[i].id.as_str())
    }

    /// `idx` is a position in the *visible* list.
    pub fn select(&mut self, idx: usize) {
        if idx < self.visible_len() {
            self.selected = idx;
        }
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.visible_len() {
            self.selected += 1;
        }
    }

    pub fn scroll_down(&mut self, n: usize) {
        let max = self.visible_len().saturating_sub(1);
        self.scroll = (self.scroll + n).min(max);
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    pub fn push_filter_char(&mut self, c: char) {
        self.filter.push(c);
        self.clamp();
    }

    pub fn pop_filter_char(&mut self) {
        self.filter.pop();
        self.clamp();
    }

    /// Clear the filter, returning whether there was anything to clear (so the
    /// caller can let an already-empty `Esc` fall through to leaving the view).
    pub fn clear_filter(&mut self) -> bool {
        if self.filter.is_empty() {
            return false;
        }
        self.filter.clear();
        self.clamp();
        true
    }

    pub fn filter_is_empty(&self) -> bool {
        self.filter.is_empty()
    }

    /// Map a click at screen row `y` to the visible-list index drawn there, or
    /// `None` when the click missed the rendered rows.
    pub fn row_at(&self, y: u16) -> Option<usize> {
        if self.last_rows_shown == 0 || y < self.last_row_y0 {
            return None;
        }
        let offset = ((y - self.last_row_y0) / ROW_H) as usize;
        (offset < self.last_rows_shown).then_some(self.scroll + offset)
    }

    /// Whether the click landed on a row's toggle switch (the right-edge pill),
    /// as opposed to elsewhere on the row, which just selects it.
    pub fn click_action(&self, x: u16, y: u16) -> bool {
        self.row_at(y).is_some() && x >= self.last_switch_left
    }

    /// Whether the click landed on the filter's clear (✕) button.
    pub fn click_clear(&self, x: u16, y: u16) -> bool {
        let r = self.last_clear_area;
        r.width > 0 && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
    }
}

impl Default for ExtensionsPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// Write `s` at `(x, y)` styled, clipped to the half-open column range
/// `[x, right)`, returning the column just past what was drawn.
fn put(buf: &mut Buffer, x: u16, y: u16, right: u16, s: &str, style: Style) -> u16 {
    if x >= right {
        return x;
    }
    let budget = (right - x) as usize;
    let clipped: String = s.chars().take(budget).collect();
    let drawn = clipped.chars().count() as u16;
    buf.set_string(x, y, &clipped, style);
    x + drawn
}

/// Paint the toggle switch pill at `(sw_x, y)`: a 4-cell coloured track (VS Code
/// blue when `on`, neutral grey when off) with a round knob slid right (on) or
/// left (off). Colour is the at-a-glance signal; the knob position reinforces it.
fn draw_switch(buf: &mut Buffer, sw_x: u16, y: u16, on: bool) {
    let track = if on { SWITCH_ON_BG } else { SWITCH_OFF_BG };
    let spaces: String = " ".repeat(SWITCH_W as usize);
    buf.set_string(sw_x, y, &spaces, Style::default().bg(track));
    let knob_off = if on { SWITCH_W - 2 } else { 1 };
    let knob_fg = if on { SWITCH_KNOB_ON } else { SWITCH_KNOB_OFF };
    buf.set_string(
        sw_x + knob_off,
        y,
        "\u{25cf}",
        Style::default().fg(knob_fg).bg(track),
    );
}

impl Widget for &mut ExtensionsPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.last_area = area;
        self.last_rows_shown = 0;
        self.last_switch_left = u16::MAX;
        self.last_search_area = Rect::default();
        self.last_clear_area = Rect::default();
        if area.width == 0 || area.height == 0 {
            return;
        }
        let right = area.x + area.width;
        let brand = false; // row-hover tint variant; theme-driven, defaults to Dark.

        // Header caption, VS Code-style small caps.
        put(
            buf,
            area.x + 1,
            area.y,
            right,
            "EXTENSIONS",
            Style::default()
                .fg(SECTION_HEADER)
                .add_modifier(Modifier::BOLD),
        );

        // Filter input row: a filled box with a magnifier, the query (or a dim
        // placeholder), and a trailing clear (✕) when non-empty.
        let search_y = area.y + 1;
        if search_y < area.y + area.height {
            let box_rect = Rect {
                x: area.x + 1,
                y: search_y,
                width: area.width.saturating_sub(2),
                height: 1,
            };
            self.last_search_area = box_rect;
            buf.set_style(box_rect, Style::default().bg(SEARCH_BG));
            let mut x = put(
                buf,
                box_rect.x + 1,
                search_y,
                right,
                "\u{2315} ",
                Style::default().fg(PLACEHOLDER_FG).bg(SEARCH_BG),
            );
            let text_right = box_rect.x + box_rect.width;
            if self.filter.is_empty() {
                put(
                    buf,
                    x,
                    search_y,
                    text_right,
                    "Search built-in & installed\u{2026}",
                    Style::default().fg(PLACEHOLDER_FG).bg(SEARCH_BG),
                );
            } else {
                // Leave room for the trailing clear glyph.
                let clear_x = text_right.saturating_sub(2);
                x = put(
                    buf,
                    x,
                    search_y,
                    clear_x,
                    &self.filter,
                    Style::default().fg(NAME_FG).bg(SEARCH_BG),
                );
                let _ = x;
                buf.set_string(
                    clear_x,
                    search_y,
                    "\u{2715}",
                    Style::default().fg(PLACEHOLDER_FG).bg(SEARCH_BG),
                );
                self.last_clear_area = Rect {
                    x: clear_x,
                    y: search_y,
                    width: 1,
                    height: 1,
                };
            }
        }

        // List starts below the search box, leaving one blank row as a gutter.
        let list_y0 = area.y + 3;
        if list_y0 >= area.y + area.height {
            return;
        }
        self.last_row_y0 = list_y0;
        let rows_avail = ((area.y + area.height - list_y0) / ROW_H) as usize;
        let sw_x = right.saturating_sub(SWITCH_W + 1);
        self.last_switch_left = sw_x;

        let visible = self.visible_indices();
        let mut shown = 0usize;
        for (offset, &item_idx) in visible
            .iter()
            .skip(self.scroll)
            .take(rows_avail)
            .enumerate()
        {
            let item = &self.items[item_idx];
            let vis_idx = self.scroll + offset;
            let row_y = list_y0 + (offset as u16) * ROW_H;
            let is_selected = vis_idx == self.selected;
            let row_rect = Rect {
                x: area.x,
                y: row_y,
                width: area.width,
                height: ROW_H,
            };
            // Selection fill wins; otherwise a hovered (unselected) row lifts to
            // the shared list-hover tint.
            let row_bg = if is_selected {
                Some(SELECTION_BG)
            } else {
                crate::widgets::hover::row_hover_bg(row_rect, self.hover_pointer, brand)
            };
            if let Some(bg) = row_bg {
                buf.set_style(row_rect, Style::default().bg(bg));
            }

            // Disabled extensions dim to grey so the list shows state at a glance.
            let name_fg = if item.enabled { NAME_FG } else { DESC_FG };
            let mut name_style = Style::default().fg(name_fg).add_modifier(Modifier::BOLD);
            let mut desc_style = Style::default().fg(DESC_FG);
            let mut chip_style = Style::default().fg(ICON_CHIP_FG);
            if let Some(bg) = row_bg {
                name_style = name_style.bg(bg);
                desc_style = desc_style.bg(bg);
                chip_style = chip_style.bg(bg);
            }

            // Line 1: leading chip + name, clipped before the toggle switch.
            let name_right = sw_x.saturating_sub(1);
            let x = put(buf, area.x + 1, row_y, name_right, "\u{25c6} ", chip_style);
            put(buf, x, row_y, name_right, &item.name, name_style);
            // The toggle switch, right-aligned on the name line.
            draw_switch(buf, sw_x, row_y, item.enabled);

            // Line 2: the blurb.
            let line2_y = row_y + 1;
            if line2_y < area.y + area.height {
                if let Some(bg) = row_bg {
                    buf.set_style(
                        Rect {
                            x: area.x,
                            y: line2_y,
                            width: area.width,
                            height: 1,
                        },
                        Style::default().bg(bg),
                    );
                }
                put(
                    buf,
                    area.x + 3,
                    line2_y,
                    right,
                    &item.description,
                    desc_style,
                );
            }
            shown += 1;
        }
        self.last_rows_shown = shown;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<ExtensionItem> {
        vec![
            ExtensionItem {
                id: "pdf".into(),
                name: "PDF Viewer".into(),
                description: "Render PDF files inline".into(),
                builtin: true,
                enabled: true,
            },
            ExtensionItem {
                id: "csv".into(),
                name: "CSV Viewer".into(),
                description: "Tabular view of delimited files".into(),
                builtin: true,
                enabled: false,
            },
            ExtensionItem {
                id: "vim".into(),
                name: "Vim".into(),
                description: "Modal editing".into(),
                builtin: true,
                enabled: true,
            },
        ]
    }

    fn render(panel: &mut ExtensionsPanel, w: u16, h: u16) -> (Buffer, String) {
        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(panel, area, &mut buf);
        let mut dump = String::new();
        for y in 0..h {
            for x in 0..w {
                dump.push_str(buf[(x, y)].symbol());
            }
            dump.push('\n');
        }
        (buf, dump)
    }

    #[test]
    fn lists_names_and_blurbs_with_a_filter_box() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        let (_buf, dump) = render(&mut panel, 44, 14);
        assert!(dump.contains("EXTENSIONS"), "header:\n{dump}");
        assert!(
            dump.contains("Search built-in"),
            "filter placeholder:\n{dump}"
        );
        assert!(dump.contains("PDF Viewer"), "first name:\n{dump}");
        assert!(dump.contains("CSV Viewer"), "second name:\n{dump}");
    }

    #[test]
    fn enabled_state_renders_as_a_colored_switch_not_a_word_label() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        let (buf, dump) = render(&mut panel, 44, 14);
        // The clunky word labels are gone.
        assert!(
            !dump.contains("Enabled"),
            "no 'Enabled' word label:\n{dump}"
        );
        assert!(
            !dump.contains("Disabled"),
            "no 'Disabled' word label:\n{dump}"
        );
        // The first row (PDF, enabled) paints its switch track in the ON blue.
        let y0 = panel.last_row_y0;
        let sw_x = panel.last_switch_left;
        assert_eq!(
            buf[(sw_x, y0)].style().bg,
            Some(SWITCH_ON_BG),
            "an enabled row's switch track is VS Code blue"
        );
        // The second row (CSV, disabled) paints its switch in the OFF grey.
        let y1 = y0 + ROW_H;
        assert_eq!(
            buf[(sw_x, y1)].style().bg,
            Some(SWITCH_OFF_BG),
            "a disabled row's switch track is grey"
        );
    }

    #[test]
    fn typing_into_the_filter_narrows_the_visible_list() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        for c in "csv".chars() {
            panel.push_filter_char(c);
        }
        assert_eq!(panel.visible_len(), 1, "only the csv extension matches");
        assert_eq!(
            panel.selected_id(),
            Some("csv"),
            "selection tracks the filtered list"
        );
        // Clearing restores the full list and reports it had something to clear.
        assert!(panel.clear_filter());
        assert_eq!(panel.visible_len(), 3);
        // An already-empty clear is a no-op the caller can fall through.
        assert!(!panel.clear_filter());
    }

    #[test]
    fn filter_matches_id_and_description_case_insensitively() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        for c in "MODAL".chars() {
            panel.push_filter_char(c);
        }
        assert_eq!(
            panel.selected_id(),
            Some("vim"),
            "matches the blurb 'Modal editing'"
        );
    }

    #[test]
    fn row_at_maps_screen_y_to_visible_item_through_the_two_cell_row_height() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        let _ = render(&mut panel, 44, 14);
        let y0 = panel.last_row_y0;
        assert_eq!(panel.row_at(y0), Some(0), "first cell of row 0");
        assert_eq!(panel.row_at(y0 + 1), Some(0), "second cell still row 0");
        assert_eq!(panel.row_at(y0 + 2), Some(1), "next item");
        assert_eq!(panel.row_at(y0.saturating_sub(1)), None, "above the list");
    }

    #[test]
    fn clicking_a_rows_switch_column_is_an_action_not_a_plain_select() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        let _ = render(&mut panel, 44, 14);
        let y0 = panel.last_row_y0;
        let sw_x = panel.last_switch_left;
        assert!(
            panel.click_action(sw_x, y0),
            "a click in the switch column on a row toggles it"
        );
        assert!(
            !panel.click_action(1, y0),
            "a click at the row's left edge selects, it is not the toggle"
        );
    }

    #[test]
    fn the_clear_button_appears_only_with_a_filter_and_hit_tests() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        let _ = render(&mut panel, 44, 14);
        assert_eq!(
            panel.last_clear_area,
            Rect::default(),
            "no clear button while the filter is empty"
        );
        panel.push_filter_char('p');
        let _ = render(&mut panel, 44, 14);
        let c = panel.last_clear_area;
        assert!(c.width > 0, "a non-empty filter lays out a clear button");
        assert!(panel.click_clear(c.x, c.y), "the clear button hit-tests");
    }

    #[test]
    fn set_items_keeps_the_selection_on_the_same_id_across_a_rebuild() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        panel.select(1); // "csv"
        let mut next = items();
        next[1].enabled = true; // a toggle rebuilds the list
        panel.set_items(next);
        assert_eq!(panel.selected_id(), Some("csv"));
    }

    #[test]
    fn move_down_stops_at_the_last_item_and_move_up_at_the_first() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        panel.move_up();
        assert_eq!(panel.selected_id(), Some("pdf"));
        panel.move_down();
        panel.move_down();
        assert_eq!(panel.selected_id(), Some("vim"));
        panel.move_down();
        assert_eq!(panel.selected_id(), Some("vim"), "clamped at the end");
    }
}
