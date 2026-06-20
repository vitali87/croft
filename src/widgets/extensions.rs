//! The Extensions side-panel: a VS Code-style list of bundled and installed
//! extensions, each with an enable/disable toggle.
//!
//! The panel is a pure projection of [`crate::lsp::manifest::summaries`] plus
//! the per-id enabled state held in [`crate::prefs`]; the app feeds both in via
//! [`ExtensionsPanel::set_items`] whenever the view is shown or a toggle flips.
//! The widget never touches prefs itself — it only reports which row the pointer
//! hit and whether the row's action affordance was clicked, and the app does the
//! actual enable/disable + persistence + feature gating. This mirrors how
//! `SourceControlPanel` reports clicks back to `App` rather than mutating git
//! state in the render path.

use std::collections::BTreeSet;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};

use crate::lsp::manifest::ExtensionSummary;

// --- Croft-Dark palette, picked from the approved Option-A mockup. Fixed darks
// so the panel reads the same on both the Black (#000) and Dark Blue (#1e222e)
// theme backgrounds; the per-row selection/hover fills are the only painted
// backgrounds (the panel itself inherits the session bg like the other views).
const SECTION_HEADER: Color = Color::Rgb(0xcc, 0xcc, 0xcc); // "EXTENSIONS" caption
const NAME_FG: Color = Color::Rgb(0xff, 0xff, 0xff); // extension display name
const DESC_FG: Color = Color::Rgb(0x79, 0x82, 0x90); // blurb / dim text
const SELECTION_BG: Color = Color::Rgb(0x26, 0x4f, 0x78); // selected row fill
const ENABLED_FG: Color = Color::Rgb(0xa3, 0xbe, 0x8c); // "Enabled" state, green
const DISABLED_FG: Color = Color::Rgb(0x79, 0x82, 0x90); // "Disabled" state, grey
const ICON_CHIP_FG: Color = Color::Rgb(0x6c, 0x7d, 0x9c); // the small leading chip
const ACTION_BG: Color = Color::Rgb(0x4e, 0x9a, 0xff); // selected-row action pill
const ACTION_FG: Color = Color::Rgb(0xff, 0xff, 0xff);

/// Each list item is two cells tall: name + state on the first line, blurb +
/// (on the selected row) the toggle action on the second, like VS Code's
/// two-line extension rows.
const ROW_H: u16 = 2;

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
    selected: usize,
    scroll: usize,
    /// Screen y of the first rendered row and how many rows were drawn, recorded
    /// each frame so a click maps back to an item index.
    last_row_y0: u16,
    last_rows_shown: usize,
    /// The enable/disable action pill on the selected row, recorded each frame so
    /// a click can be distinguished from a plain row select.
    last_action_area: Rect,
}

impl ExtensionsPanel {
    pub fn new() -> Self {
        Self {
            focused: false,
            hover_pointer: None,
            last_area: Rect::default(),
            items: Vec::new(),
            selected: 0,
            scroll: 0,
            last_row_y0: 0,
            last_rows_shown: 0,
            last_action_area: Rect::default(),
        }
    }

    /// Replace the displayed list, preserving the selection by id where possible
    /// (so a toggle that rebuilds the list doesn't jump the cursor) and clamping
    /// it into range.
    pub fn set_items(&mut self, items: Vec<ExtensionItem>) {
        let prior_id = self.items.get(self.selected).map(|i| i.id.clone());
        self.items = items;
        if let Some(id) = prior_id
            && let Some(idx) = self.items.iter().position(|i| i.id == id)
        {
            self.selected = idx;
        }
        if self.selected >= self.items.len() {
            self.selected = self.items.len().saturating_sub(1);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.items.get(self.selected).map(|i| i.id.as_str())
    }

    pub fn select(&mut self, idx: usize) {
        if idx < self.items.len() {
            self.selected = idx;
        }
    }

    pub fn is_selected(&self, idx: usize) -> bool {
        idx == self.selected
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.items.len() {
            self.selected += 1;
        }
    }

    pub fn scroll_down(&mut self, n: usize) {
        let max = self.items.len().saturating_sub(1);
        self.scroll = (self.scroll + n).min(max);
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// Map a click at screen row `y` to the item index drawn there, or `None`
    /// when the click missed the rendered rows.
    pub fn row_at(&self, y: u16) -> Option<usize> {
        if self.last_rows_shown == 0 || y < self.last_row_y0 {
            return None;
        }
        let offset = ((y - self.last_row_y0) / ROW_H) as usize;
        (offset < self.last_rows_shown).then_some(self.scroll + offset)
    }

    /// Whether the click landed on the selected row's enable/disable action
    /// pill (as opposed to elsewhere on the row, which just selects it).
    pub fn click_action(&self, x: u16, y: u16) -> bool {
        let r = self.last_action_area;
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

impl Widget for &mut ExtensionsPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.last_area = area;
        self.last_rows_shown = 0;
        self.last_action_area = Rect::default();
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

        let list_y0 = area.y + 2;
        if list_y0 >= area.y + area.height {
            return;
        }
        self.last_row_y0 = list_y0;
        let rows_avail = ((area.y + area.height - list_y0) / ROW_H) as usize;

        let mut shown = 0usize;
        for (offset, item) in self
            .items
            .iter()
            .skip(self.scroll)
            .take(rows_avail)
            .enumerate()
        {
            let idx = self.scroll + offset;
            let row_y = list_y0 + (offset as u16) * ROW_H;
            let is_selected = idx == self.selected;
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
            let name_fg = if item.enabled { NAME_FG } else { DISABLED_FG };
            let mut name_style = Style::default().fg(name_fg).add_modifier(Modifier::BOLD);
            let mut desc_style = Style::default().fg(DESC_FG);
            let mut chip_style = Style::default().fg(ICON_CHIP_FG);
            if let Some(bg) = row_bg {
                name_style = name_style.bg(bg);
                desc_style = desc_style.bg(bg);
                chip_style = chip_style.bg(bg);
            }

            // Right-aligned state label: green "Enabled" / grey "Disabled".
            let (state_txt, state_fg) = if item.enabled {
                ("Enabled", ENABLED_FG)
            } else {
                ("Disabled", DISABLED_FG)
            };
            let state_w = state_txt.chars().count() as u16;
            let state_x = right.saturating_sub(state_w + 1);
            let mut state_style = Style::default().fg(state_fg);
            if let Some(bg) = row_bg {
                state_style = state_style.bg(bg);
            }
            put(buf, state_x, row_y, right, state_txt, state_style);

            // Line 1: leading chip + name, clipped before the state label.
            let mut x = area.x + 1;
            x = put(
                buf,
                x,
                row_y,
                state_x.saturating_sub(1),
                "\u{25c6} ",
                chip_style,
            );
            put(
                buf,
                x,
                row_y,
                state_x.saturating_sub(1),
                &item.name,
                name_style,
            );

            // Line 2: blurb, and on the selected row the enable/disable action.
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
                let action_left = if is_selected {
                    // Pill: " Disable " on an enabled row, " Enable " on a disabled one.
                    let label = if item.enabled {
                        " Disable "
                    } else {
                        " Enable "
                    };
                    let aw = label.chars().count() as u16;
                    let ax = right.saturating_sub(aw + 1);
                    let action_rect = Rect {
                        x: ax,
                        y: line2_y,
                        width: aw,
                        height: 1,
                    };
                    self.last_action_area = action_rect;
                    put(
                        buf,
                        ax,
                        line2_y,
                        right,
                        label,
                        Style::default()
                            .fg(ACTION_FG)
                            .bg(ACTION_BG)
                            .add_modifier(Modifier::BOLD),
                    );
                    ax.saturating_sub(1)
                } else {
                    right
                };
                put(
                    buf,
                    area.x + 3,
                    line2_y,
                    action_left,
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
    fn lists_names_blurbs_and_per_row_state_labels() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        let (_buf, dump) = render(&mut panel, 44, 12);
        assert!(dump.contains("EXTENSIONS"), "header:\n{dump}");
        assert!(dump.contains("PDF Viewer"), "first name:\n{dump}");
        assert!(dump.contains("CSV Viewer"), "second name:\n{dump}");
        assert!(dump.contains("Enabled"), "enabled state:\n{dump}");
        assert!(dump.contains("Disabled"), "disabled state:\n{dump}");
    }

    #[test]
    fn row_at_maps_screen_y_to_item_through_the_two_cell_row_height() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        let _ = render(&mut panel, 44, 12);
        let y0 = panel.last_row_y0;
        assert_eq!(panel.row_at(y0), Some(0), "first cell of row 0");
        assert_eq!(panel.row_at(y0 + 1), Some(0), "second cell still row 0");
        assert_eq!(panel.row_at(y0 + 2), Some(1), "next item");
        assert_eq!(panel.row_at(y0.saturating_sub(1)), None, "above the list");
    }

    #[test]
    fn only_the_selected_row_exposes_a_clickable_action_pill() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        panel.select(1);
        let _ = render(&mut panel, 44, 12);
        let a = panel.last_action_area;
        assert!(a.width > 0, "selected row must lay out an action pill");
        assert!(
            panel.click_action(a.x, a.y),
            "click inside the pill is an action"
        );
        assert!(
            !panel.click_action(0, a.y),
            "a click at the row's left edge selects, it is not the action"
        );
    }

    #[test]
    fn set_items_keeps_the_selection_on_the_same_id_across_a_rebuild() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        panel.select(1); // "csv"
        // Rebuild with csv now enabled (a toggle); selection should stay on csv.
        let mut next = items();
        next[1].enabled = true;
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
        assert_eq!(panel.selected_id(), Some("csv"));
        panel.move_down();
        assert_eq!(panel.selected_id(), Some("csv"), "clamped at the end");
    }
}
