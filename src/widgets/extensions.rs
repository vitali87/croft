//! The Extensions side-panel: a VS Code-style list of bundled and installed
//! extensions grouped under BUILT-IN / INSTALLED headers, each row carrying a
//! coloured language/file mark, a toggle switch, and a one-line blurb. A filter
//! box at the top narrows the list as you type.
//!
//! The panel is a pure projection of [`crate::lsp::manifest::summaries`] plus
//! the per-id enabled state held in [`crate::prefs`]; the app feeds both in via
//! [`ExtensionsPanel::set_items`] whenever the view is shown or a toggle flips,
//! and pushes the active [`crate::theme::Theme`] each frame so the panel matches
//! the rest of the chrome on both the Black and Dark-Blue themes. The widget
//! never touches prefs itself — it only reports which row / switch / clear-button
//! the pointer hit, and the app does the enable/disable + persistence + gating.
//! The filter is local-only: there is no marketplace, so it narrows the
//! installed list rather than searching a remote registry.

use std::collections::BTreeSet;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};

use crate::lsp::manifest::ExtensionSummary;
use crate::theme::Theme;

const SECTION_HEADER: Color = Color::Rgb(0xcc, 0xcc, 0xcc); // caption / group headers
const NAME_FG: Color = Color::Rgb(0xff, 0xff, 0xff); // extension display name
const DESC_FG: Color = Color::Rgb(0x79, 0x82, 0x90); // blurb / dim text

/// The toggle switch is a rounded pill: Powerline half-circle caps frame a
/// coloured track, with a round knob slid to the lit (right) or dark (left)
/// side. Wider and more switch-like than a single glyph, and it renders on
/// every Nerd Font (Powerline glyphs are universal).
const SWITCH_CAP_L: char = '\u{e0b6}'; // left half-circle
const SWITCH_CAP_R: char = '\u{e0b4}'; // right half-circle
const SWITCH_KNOB: char = '\u{ebb4}'; // codicon circle-large-filled, cell-centered
const SWITCH_OFF_TRACK: Color = Color::Rgb(0x3c, 0x44, 0x4e); // grey track (off)
const SWITCH_KNOB_OFF: Color = Color::Rgb(0xcd, 0xd3, 0xdc); // dim knob (off)
/// Codicon `search` magnifier for the filter box (matches the activity bar's
/// Search icon), and the `close` glyph for the clear (✕) affordance.
const SEARCH_GLYPH: char = '\u{ea6d}';
const CLEAR_GLYPH: char = '\u{ea76}';

/// Each list item is two cells tall: mark + name + toggle on the first line,
/// blurb on the second, like VS Code's two-line extension rows.
const ROW_H: u16 = 2;
/// Cells reserved at the right edge for the toggle pill: 4 for the switch
/// (2 caps + track + knob) plus a 1-cell margin. The click zone is everything
/// from `last_switch_left` to the panel edge.
const SWITCH_ZONE: u16 = 5;
/// Cells reserved just left of the switch for a row's uninstall (trash) button,
/// drawn only on removable INSTALLED rows: the glyph plus a 1-cell gap. The
/// click zone is `[last_uninstall_left, last_switch_left)`.
const UNINSTALL_ZONE: u16 = 2;
/// Codicon `trash` (matches the SCM menu's delete icon) — the per-row uninstall
/// affordance so a mouse user has a visible control, not just the Delete key
/// (which on a Mac keyboard is Fn+Delete and easy to miss).
const TRASH_GLYPH: char = '\u{ea81}';

/// The theme-driven colours the panel paints. Resolved once per render from the
/// active [`Theme`] so the Black theme wears the brand teal where the Dark-Blue
/// theme keeps the historical VS Code blue.
struct Palette {
    brand: bool,
    accent: Color,    // magnifier glyph
    selection: Color, // selected-row fill
    search_bg: Color, // filter input fill
    switch_on: Color, // lit toggle track
}

impl Palette {
    fn for_theme(theme: Theme) -> Self {
        // Each theme declares its own palette; the gradient flag selects the
        // brand chrome (was `theme == Black`). Byte-identical to the old
        // hardcoded values for the two first-party themes.
        Palette {
            brand: theme.gradient(),
            accent: theme.accent(),
            selection: theme.selection(),
            search_bg: theme.search_bg(),
            switch_on: theme.button(),
        }
    }
}

/// Paint the rounded toggle pill at `(sw_x, y)` over the row background `row_bg`
/// (so the half-circle caps blend into the panel or the selection fill). On =
/// the theme's lit track with the knob to the right; off = a grey track with the
/// knob to the left. Occupies 4 cells: `[cap, track, track, cap]` with the knob
/// overlaying a track cell.
fn draw_switch(
    buf: &mut Buffer,
    sw_x: u16,
    y: u16,
    on: bool,
    row_bg: Option<Color>,
    pal: &Palette,
) {
    let track = if on { pal.switch_on } else { SWITCH_OFF_TRACK };
    let cap_style = Style::default()
        .fg(track)
        .bg(row_bg.unwrap_or(Color::Reset));
    buf.set_string(sw_x, y, SWITCH_CAP_L.to_string(), cap_style);
    for i in 1..=2 {
        buf.set_string(sw_x + i, y, " ", Style::default().bg(track));
    }
    let (knob_i, knob_fg) = if on {
        (2, NAME_FG)
    } else {
        (1, SWITCH_KNOB_OFF)
    };
    buf.set_string(
        sw_x + knob_i,
        y,
        SWITCH_KNOB.to_string(),
        Style::default().fg(knob_fg).bg(track),
    );
    buf.set_string(sw_x + 3, y, SWITCH_CAP_R.to_string(), cap_style);
}

#[cfg(test)]
fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// The coloured language / file mark for a known extension id (its real
/// ecosystem glyph in its brand colour), or `None` to fall back to a neutral
/// chip. Glyph codepoints verified against the Nerd Fonts glyphnames table.
fn chip_for(id: &str) -> Option<(char, Color)> {
    let (glyph, c) = match id {
        "pdf" => ('\u{f1c1}', (0xe5, 0x25, 0x2a)), // fa-file_pdf, red
        "csv" => ('\u{eefc}', (0x21, 0xa3, 0x66)), // fa-file_csv, green
        "vim" => ('\u{e62b}', (0x01, 0x97, 0x33)), // custom-vim, green
        "lsp-python" => ('\u{e73c}', (0x37, 0x76, 0xab)), // dev-python, blue
        "lsp-typescript" => ('\u{e8ca}', (0x31, 0x78, 0xc6)), // dev-typescript, blue
        "lsp-rust" => ('\u{e7a8}', (0xce, 0x6a, 0x3a)), // dev-rust, orange
        "lsp-go" => ('\u{e724}', (0x00, 0xad, 0xd8)), // dev-go, cyan
        "lsp-yaml" => ('\u{e8eb}', (0xcb, 0x17, 0x1e)), // dev-yaml, red
        "lsp-json" => ('\u{e80b}', (0xcb, 0xcb, 0x41)), // dev-json, gold
        "lsp-html" => ('\u{e736}', (0xe3, 0x4f, 0x26)), // dev-html5, orange
        "lsp-css" => ('\u{e749}', (0x15, 0x72, 0xb6)), // dev-css3, blue
        "lsp-bash" => ('\u{e760}', (0x4e, 0xaa, 0x25)), // dev-bash, green
        "lsp-toml" => ('\u{e615}', (0x9c, 0x42, 0x21)), // seti-config, toml brown
        "lsp-cpp" => ('\u{e7a3}', (0x00, 0x59, 0x9c)), // dev-cplusplus, C++ blue
        "dap-python" => ('\u{eb91}', (0x37, 0x76, 0xab)), // cod-debug_alt, python blue
        "dap-lldb" => ('\u{eb91}', (0xce, 0x6a, 0x3a)), // cod-debug_alt, rust orange
        "dap-js" => ('\u{eb91}', (0xf7, 0xdf, 0x1e)), // cod-debug_alt, JS yellow
        "mcp-fetch" => ('\u{eb01}', (0x4e, 0x9a, 0xff)), // cod-globe, web blue
        "themes" => ('\u{eb5c}', (0xc5, 0x86, 0xc0)), // cod-symbol_color, theme purple
        _ => return None,
    };
    Some((glyph, Color::Rgb(c.0, c.1, c.2)))
}

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
    /// A catalog entry not yet installed: shown under AVAILABLE with an "Add"
    /// affordance instead of an enable/disable toggle. `enabled` is meaningless
    /// for these (they aren't installed yet).
    pub available: bool,
    /// An INSTALLED extension croft may uninstall (a catalog entry it wrote, not
    /// a hand-dropped user manifest). Set by the app, which owns catalog
    /// knowledge; drives the per-row trash button and the Delete handler.
    pub removable: bool,
}

impl ExtensionItem {
    /// Section rank: BUILT-IN (0) before INSTALLED (1) before AVAILABLE (2).
    fn group_rank(&self) -> u8 {
        if self.available {
            2
        } else if self.builtin {
            0
        } else {
            1
        }
    }
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
            available: false,
            removable: false,
        })
        .collect()
}

/// Project catalog summaries onto AVAILABLE rows (not installed; the toggle
/// action adds them). Appended after the installed items by the app.
pub fn items_from_available(summaries: Vec<ExtensionSummary>) -> Vec<ExtensionItem> {
    summaries
        .into_iter()
        .map(|s| ExtensionItem {
            enabled: false,
            id: s.id,
            name: s.name,
            description: s.description,
            builtin: false,
            available: true,
            removable: false,
        })
        .collect()
}

pub struct ExtensionsPanel {
    pub focused: bool,
    pub hover_pointer: Option<(u16, u16)>,
    /// Active theme, pushed by the app each frame so the palette matches the
    /// rest of the chrome.
    pub theme: Theme,
    pub last_area: Rect,
    items: Vec<ExtensionItem>,
    /// Local filter query. Narrows the visible list by case-insensitive match on
    /// name / description / id; never searches a remote marketplace (there is
    /// none).
    filter: String,
    /// Selection and scroll index into the *visible* (filtered) list of items
    /// (section headers are not selectable and do not count).
    selected: usize,
    scroll: usize,
    /// Per-rendered-row hit map: `(top y of the row, visible-list index)`, so a
    /// click maps to an item even though section headers interrupt the rows.
    row_hits: Vec<(u16, usize)>,
    /// Left column of the toggle-switch zone, recorded each frame so a click can
    /// be told apart from a plain row select.
    last_switch_left: u16,
    /// Left column of the per-row uninstall (trash) button zone, just left of
    /// the switch. A click in `[last_uninstall_left, last_switch_left)` on a
    /// removable row uninstalls it.
    last_uninstall_left: u16,
    /// The filter input row and its trailing clear (✕) cell, for click routing.
    last_search_area: Rect,
    last_clear_area: Rect,
}

impl ExtensionsPanel {
    pub fn new() -> Self {
        Self {
            focused: false,
            hover_pointer: None,
            theme: Theme::default(),
            last_area: Rect::default(),
            items: Vec::new(),
            filter: String::new(),
            selected: 0,
            scroll: 0,
            row_hits: Vec::new(),
            last_switch_left: u16::MAX,
            last_uninstall_left: u16::MAX,
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

    /// Indices into `items` that pass the current filter, built-in group first
    /// (BUILT-IN before INSTALLED), preserving order within each group.
    fn visible_indices(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.items.len())
            .filter(|&i| self.matches(&self.items[i]))
            .collect();
        v.sort_by_key(|&i| self.items[i].group_rank()); // BUILT-IN, INSTALLED, AVAILABLE
        v
    }

    fn visible_len(&self) -> usize {
        self.items.iter().filter(|i| self.matches(i)).count()
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

    /// Whether the selected row is an AVAILABLE catalog entry — so the app
    /// treats its toggle as "add" (install) rather than enable/disable.
    pub fn selected_available(&self) -> bool {
        self.visible_indices()
            .get(self.selected)
            .map(|&i| self.items[i].available)
            .unwrap_or(false)
    }

    /// Whether the selected row is an INSTALLED extension (not built-in, not an
    /// AVAILABLE catalog row) — the only rows eligible for uninstall. The app
    /// further restricts removal to catalog-sourced ids so it never deletes a
    /// hand-dropped user manifest.
    pub fn selected_removable(&self) -> bool {
        self.visible_indices()
            .get(self.selected)
            .map(|&i| !self.items[i].builtin && !self.items[i].available)
            .unwrap_or(false)
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
        self.row_hits
            .iter()
            .find(|(ry, _)| y >= *ry && y < ry + ROW_H)
            .map(|(_, idx)| *idx)
    }

    /// Whether the click landed on a row's toggle switch (the right-edge zone),
    /// as opposed to elsewhere on the row, which just selects it.
    pub fn click_action(&self, x: u16, y: u16) -> bool {
        self.row_at(y).is_some() && x >= self.last_switch_left
    }

    /// The item drawn at visible-list index `vis_idx` (as returned by
    /// [`Self::row_at`]), for hover-tooltip resolution. The app reads its flags
    /// to pick the right tooltip text without the panel needing catalog
    /// knowledge.
    pub fn item_at(&self, vis_idx: usize) -> Option<&ExtensionItem> {
        self.visible_indices().get(vis_idx).map(|&i| &self.items[i])
    }

    /// Whether the click landed on a removable row's trash button (the small
    /// zone just left of the switch). Only true for rows flagged `removable`,
    /// so a click on a built-in or AVAILABLE row never reads as uninstall.
    pub fn click_uninstall(&self, x: u16, y: u16) -> bool {
        let Some(idx) = self.row_at(y) else {
            return false;
        };
        let removable = self.item_at(idx).map(|it| it.removable).unwrap_or(false);
        removable && x >= self.last_uninstall_left && x < self.last_switch_left
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

impl Widget for &mut ExtensionsPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.last_area = area;
        self.row_hits.clear();
        self.last_switch_left = u16::MAX;
        self.last_search_area = Rect::default();
        self.last_clear_area = Rect::default();
        if area.width == 0 || area.height == 0 {
            return;
        }
        let right = area.x + area.width;
        let bottom = area.y + area.height;
        let pal = Palette::for_theme(self.theme);

        // Header caption.
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
        if search_y < bottom {
            let box_rect = Rect {
                x: area.x + 1,
                y: search_y,
                width: area.width.saturating_sub(2),
                height: 1,
            };
            self.last_search_area = box_rect;
            buf.set_style(box_rect, Style::default().bg(pal.search_bg));
            let x = put(
                buf,
                box_rect.x + 1,
                search_y,
                right,
                &format!("{SEARCH_GLYPH} "),
                Style::default().fg(pal.accent).bg(pal.search_bg),
            );
            let text_right = box_rect.x + box_rect.width;
            if self.filter.is_empty() {
                put(
                    buf,
                    x,
                    search_y,
                    text_right,
                    "Search built-in & installed\u{2026}",
                    Style::default().fg(DESC_FG).bg(pal.search_bg),
                );
            } else {
                let clear_x = text_right.saturating_sub(2);
                put(
                    buf,
                    x,
                    search_y,
                    clear_x,
                    &self.filter,
                    Style::default().fg(NAME_FG).bg(pal.search_bg),
                );
                buf.set_string(
                    clear_x,
                    search_y,
                    CLEAR_GLYPH.to_string(),
                    Style::default().fg(DESC_FG).bg(pal.search_bg),
                );
                self.last_clear_area = Rect {
                    x: clear_x,
                    y: search_y,
                    width: 1,
                    height: 1,
                };
            }
        }

        // Content begins below the search box, leaving a blank gutter row.
        let mut y = area.y + 3;
        if y >= bottom {
            return;
        }
        let sw_x = right.saturating_sub(SWITCH_ZONE);
        self.last_switch_left = sw_x;
        // The trash button sits just left of the switch; same column on every
        // removable row, so record it once. The click zone is bounded by the
        // switch on its right.
        let uninstall_x = sw_x.saturating_sub(UNINSTALL_ZONE);
        self.last_uninstall_left = uninstall_x;

        let visible = self.visible_indices();
        let mut prev_group: Option<u8> = None;
        for (vis_idx, &item_idx) in visible.iter().enumerate().skip(self.scroll) {
            let item = &self.items[item_idx];

            // Section header whenever the group changes (or on the first shown
            // row). One row tall, dim small-caps, like VS Code's BUILT-IN list.
            if prev_group != Some(item.group_rank()) {
                if y + 1 >= bottom {
                    break;
                }
                let label = match item.group_rank() {
                    0 => "BUILT-IN",
                    1 => "INSTALLED",
                    _ => "AVAILABLE",
                };
                put(
                    buf,
                    area.x + 1,
                    y,
                    right,
                    label,
                    Style::default().fg(DESC_FG).add_modifier(Modifier::BOLD),
                );
                prev_group = Some(item.group_rank());
                y += 1;
            }
            if y + ROW_H > bottom {
                break;
            }

            let is_selected = vis_idx == self.selected;
            let row_rect = Rect {
                x: area.x,
                y,
                width: area.width,
                height: ROW_H,
            };
            let row_bg = if is_selected {
                Some(pal.selection)
            } else {
                crate::widgets::hover::row_hover_bg(row_rect, self.hover_pointer, pal.brand)
            };
            if let Some(bg) = row_bg {
                buf.set_style(row_rect, Style::default().bg(bg));
            }

            let name_fg = if item.enabled { NAME_FG } else { DESC_FG };
            let mut name_style = Style::default().fg(name_fg).add_modifier(Modifier::BOLD);
            let mut desc_style = Style::default().fg(DESC_FG);
            if let Some(bg) = row_bg {
                name_style = name_style.bg(bg);
                desc_style = desc_style.bg(bg);
            }

            // Line 1: coloured language/file mark + name, clipped before the
            // toggle (or before the trash button on a removable row), then the
            // right-aligned controls.
            let name_right = if item.removable {
                uninstall_x.saturating_sub(1)
            } else {
                sw_x.saturating_sub(1)
            };
            let (chip, chip_color) = chip_for(&item.id).unwrap_or(('\u{25c6}', ICON_CHIP_FALLBACK));
            let mut chip_style = Style::default().fg(chip_color);
            if let Some(bg) = row_bg {
                chip_style = chip_style.bg(bg);
            }
            let x = put(
                buf,
                area.x + 1,
                y,
                name_right,
                &format!("{chip} "),
                chip_style,
            );
            put(buf, x, y, name_right, &item.name, name_style);

            // Installed rows get an enable/disable switch; AVAILABLE catalog
            // rows get a "+ Add" affordance in the same hit zone (toggling it
            // installs the entry rather than enabling it).
            if item.available {
                let add_style = Style::default()
                    .fg(pal.accent)
                    .bg(row_bg.unwrap_or(Color::Reset))
                    .add_modifier(Modifier::BOLD);
                put(buf, sw_x, y, right, "+Add", add_style);
            } else {
                draw_switch(buf, sw_x, y, item.enabled, row_bg, &pal);
            }

            // Removable (catalog) rows get a visible trash button just left of
            // the switch, so a mouse user has an uninstall control without the
            // Delete key (Fn+Delete on a Mac, easy to miss).
            if item.removable {
                let trash_style = Style::default()
                    .fg(DESC_FG)
                    .bg(row_bg.unwrap_or(Color::Reset));
                buf.set_string(uninstall_x, y, TRASH_GLYPH.to_string(), trash_style);
            }

            // Line 2: the blurb.
            let line2_y = y + 1;
            put(
                buf,
                area.x + 3,
                line2_y,
                right,
                &item.description,
                desc_style,
            );

            self.row_hits.push((y, vis_idx));
            y += ROW_H;
        }
    }
}

/// Neutral chip colour for an extension with no known language/file mark.
const ICON_CHIP_FALLBACK: Color = Color::Rgb(0x6c, 0x7d, 0x9c);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_catalog_entries_sort_last_and_toggle_means_add() {
        let mut panel = ExtensionsPanel::new();
        let mut items = items(); // three BUILT-IN rows
        items.extend(items_from_available(vec![ExtensionSummary {
            id: "mcp-time".into(),
            name: "Time".into(),
            description: "Current time".into(),
            builtin: false,
        }]));
        panel.set_items(items);
        // The available entry sorts after every installed/built-in row.
        let order = panel.visible_indices();
        let last = *order.last().unwrap();
        assert!(panel.items[last].available, "AVAILABLE row must sort last");
        // Selecting it reports `available`, so the app treats its toggle as Add.
        for _ in 0..10 {
            panel.move_down();
        }
        assert_eq!(panel.selected_id(), Some("mcp-time"));
        assert!(
            panel.selected_available(),
            "the catalog row must report as available so its toggle installs"
        );
    }

    #[test]
    fn removable_row_shows_a_trash_button_and_only_its_zone_uninstalls() {
        let mut panel = ExtensionsPanel::new();
        let mut its = items();
        its.push(ExtensionItem {
            id: "mcp-fetch".into(),
            name: "Web Fetch".into(),
            description: "Fetch a URL".into(),
            builtin: false,
            enabled: true,
            available: false,
            removable: true,
        });
        panel.set_items(its);
        let (_buf, text) = render(&mut panel, 40, 24);
        assert!(
            text.contains(TRASH_GLYPH),
            "a removable row must paint a trash button"
        );

        let row_y = |id: &str| {
            (0..24u16).find(|&y| {
                panel
                    .row_at(y)
                    .and_then(|idx| panel.item_at(idx))
                    .map(|it| it.id == id)
                    .unwrap_or(false)
            })
        };
        let y = row_y("mcp-fetch").expect("removable row rendered");
        let trash_x = panel.last_uninstall_left;
        let switch_x = panel.last_switch_left;
        assert!(
            panel.click_uninstall(trash_x, y),
            "the trash zone uninstalls the removable row"
        );
        assert!(
            !panel.click_uninstall(switch_x, y),
            "the switch is not the trash zone"
        );
        // The same column on a built-in row never reads as uninstall.
        let by = row_y("pdf").expect("built-in row rendered");
        assert!(
            !panel.click_uninstall(trash_x, by),
            "built-in rows are not removable"
        );
    }

    fn items() -> Vec<ExtensionItem> {
        vec![
            ExtensionItem {
                id: "pdf".into(),
                name: "PDF Viewer".into(),
                description: "Render PDF files inline".into(),
                builtin: true,
                enabled: true,
                available: false,
                removable: false,
            },
            ExtensionItem {
                id: "csv".into(),
                name: "CSV Viewer".into(),
                description: "Tabular view of delimited files".into(),
                builtin: true,
                enabled: false,
                available: false,
                removable: false,
            },
            ExtensionItem {
                id: "vim".into(),
                name: "Vim".into(),
                description: "Modal editing".into(),
                builtin: true,
                enabled: true,
                available: false,
                removable: false,
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
    fn lists_names_blurbs_a_filter_box_and_a_builtin_header() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        let (_buf, dump) = render(&mut panel, 44, 16);
        assert!(dump.contains("EXTENSIONS"), "title:\n{dump}");
        assert!(
            dump.contains("Search built-in"),
            "filter placeholder:\n{dump}"
        );
        assert!(dump.contains("BUILT-IN"), "section header:\n{dump}");
        assert!(dump.contains("PDF Viewer"), "first name:\n{dump}");
        assert!(dump.contains("CSV Viewer"), "second name:\n{dump}");
    }

    #[test]
    fn enabled_state_renders_as_a_pill_switch_not_a_word_label() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        panel.theme = Theme::BLACK;
        let (buf, dump) = render(&mut panel, 44, 16);
        assert!(
            !dump.contains("Enabled"),
            "no 'Enabled' word label:\n{dump}"
        );
        assert!(
            !dump.contains("Disabled"),
            "no 'Disabled' word label:\n{dump}"
        );
        let sw_x = panel.last_switch_left;
        // Row 0 (PDF, enabled): the pill's track is the lit (brand-teal) colour.
        let y0 = panel.row_hits[0].0;
        assert_eq!(
            buf[(sw_x + 1, y0)].style().bg,
            Some(rgb(crate::gradient::PRIMARY_BTN_BG)),
            "an enabled row's pill track is the lit colour"
        );
        assert_eq!(
            buf[(sw_x, y0)].symbol().chars().next(),
            Some(SWITCH_CAP_L),
            "the pill has a rounded left cap"
        );
        // Row 1 (CSV, disabled): the pill's track is the grey off colour.
        let y1 = panel.row_hits[1].0;
        assert_eq!(
            buf[(sw_x + 1, y1)].style().bg,
            Some(SWITCH_OFF_TRACK),
            "a disabled row's pill track is grey"
        );
    }

    #[test]
    fn the_panel_palette_follows_the_active_theme() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        panel.select(0);
        // Black theme: selected row wears the brand teal fill.
        panel.theme = Theme::BLACK;
        let (buf, _) = render(&mut panel, 44, 16);
        let y0 = panel.row_hits[0].0;
        assert_eq!(
            buf[(2, y0)].style().bg,
            Some(rgb(crate::gradient::POPUP_SEL_BG)),
            "Black theme selection is the brand teal"
        );
        // Dark-Blue theme: selected row wears the historical blue fill.
        panel.theme = Theme::DARK_BLUE;
        let (buf, _) = render(&mut panel, 44, 16);
        let y0 = panel.row_hits[0].0;
        assert_eq!(
            buf[(2, y0)].style().bg,
            Some(Color::Rgb(0x09, 0x4d, 0x77)),
            "Dark-Blue theme selection is the historical blue"
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
            "selection tracks the filter"
        );
        assert!(panel.clear_filter());
        assert_eq!(panel.visible_len(), 3);
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
    fn row_at_maps_screen_y_to_visible_item_below_the_section_header() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        let _ = render(&mut panel, 44, 16);
        let y0 = panel.row_hits[0].0;
        assert_eq!(panel.row_at(y0), Some(0), "first cell of row 0");
        assert_eq!(panel.row_at(y0 + 1), Some(0), "second cell still row 0");
        assert_eq!(panel.row_at(y0 + 2), Some(1), "next item");
        // The BUILT-IN header sits one row above the first item and is inert.
        assert_eq!(
            panel.row_at(y0 - 1),
            None,
            "the section header is not a row"
        );
    }

    #[test]
    fn clicking_a_rows_switch_zone_is_an_action_not_a_plain_select() {
        let mut panel = ExtensionsPanel::new();
        panel.set_items(items());
        let _ = render(&mut panel, 44, 16);
        let y0 = panel.row_hits[0].0;
        let sw_x = panel.last_switch_left;
        assert!(
            panel.click_action(sw_x, y0),
            "a click on the toggle toggles it"
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
        let _ = render(&mut panel, 44, 16);
        assert_eq!(
            panel.last_clear_area,
            Rect::default(),
            "no clear while empty"
        );
        panel.push_filter_char('p');
        let _ = render(&mut panel, 44, 16);
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
