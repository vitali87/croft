//! Screen layout and Customize Layout, the activity bar and its badges, the Manage
//! menu, side-bar auto-hide, splitter drags and screencast mode.

use super::*;

/// The clickable icons on the activity bar, used to track which one the
/// pointer is hovering. Mirrors `SidebarView` plus the bottom-anchored
/// sponsor heart and settings gear, which are not sidebar views but are
/// still hoverable buttons.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActivityIcon {
    Explorer,
    Search,
    SourceControl,
    Remote,
    RunDebug,
    Extensions,
    Testing,
    CodeQL,
    Sponsor,
    Settings,
}

impl ActivityIcon {
    /// The side bar view this icon opens; `None` for the sponsor heart,
    /// which opens a web page, and the settings gear, which opens a menu.
    pub(super) fn view(self) -> Option<SidebarView> {
        match self {
            ActivityIcon::Explorer => Some(SidebarView::Explorer),
            ActivityIcon::Search => Some(SidebarView::Search),
            ActivityIcon::SourceControl => Some(SidebarView::SourceControl),
            ActivityIcon::Remote => Some(SidebarView::Remote),
            ActivityIcon::RunDebug => Some(SidebarView::RunDebug),
            ActivityIcon::Extensions => Some(SidebarView::Extensions),
            ActivityIcon::Testing => Some(SidebarView::Testing),
            ActivityIcon::CodeQL => Some(SidebarView::CodeQL),
            ActivityIcon::Sponsor | ActivityIcon::Settings => None,
        }
    }
}

/// Where the activity bar's sponsor heart leads (#1940).
pub(super) const SPONSOR_URL: &str = "https://github.com/sponsors/vitali87";

pub(super) const ACTIVITY_BAR_WIDTH: u16 = 4;

pub(super) const ACTIVITY_ICON_HEIGHT: u16 = 2;

pub(super) const ACTIVITY_ICON_GAP: u16 = 0;

/// Fixed width of the secondary side bar (hosts the active file's Outline),
/// on the edge opposite the primary side bar. VS Code's auxiliary bar.
pub(super) const SECONDARY_SIDEBAR_WIDTH: u16 = 30;

/// Cell box for each Customize Layout toolbar icon: 2 cells wide × 1 tall —
/// exactly HALF an activity-bar icon's `ACTIVITY_BAR_WIDTH`×`ACTIVITY_ICON_HEIGHT`
/// (4×2) box, so the toolbar reads as a smaller chrome control than the far-left
/// Explorer / Search icons, as intended. With the usual ~1:2 cell aspect this is
/// a crisp ~square ≈20×20px image. Baked OPAQUE on the editor background so it
/// paints in FRONT of the tab-strip / welcome top line (the line sits behind
/// it), and stays within the single top row (no overlap with editor content).
pub(super) const LAYOUT_ICON_CELLS_W: u16 = 2;

pub(super) const LAYOUT_ICON_CELLS_H: u16 = 1;

/// Cells the three layout icons take at the right end of the top row: three
/// icons, a one-column gap after the first two and a one-column right pad.
pub(super) const LAYOUT_ICONS_W: u16 = (LAYOUT_ICON_CELLS_W + 1) * 3;

/// Which edge the primary side bar (and the activity bar that flanks it) dock
/// to. VS Code's "Primary Side Bar Position". Persisted via `crate::prefs`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SideBarPosition {
    #[default]
    Left,
    Right,
}

/// Horizontal span of the bottom panel (the terminal stack). Mirrors VS
/// Code's "Panel Alignment": `Center` (default) spans the editor column;
/// `Left`/`Right` hug that half of the editor column; `Justify` stretches the
/// panel across the full content width, under the side bar. Persisted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PanelAlignment {
    Left,
    #[default]
    Center,
    Right,
    Justify,
}

impl PanelAlignment {
    /// Every alignment with its picker id and label, in the Customize
    /// Layout popup's order (View: Set Panel Alignment…, #852).
    pub(super) const OPTIONS: [(PanelAlignment, &'static str, &'static str); 4] = [
        (PanelAlignment::Left, "left", "Left"),
        (PanelAlignment::Center, "center", "Center"),
        (PanelAlignment::Right, "right", "Right"),
        (PanelAlignment::Justify, "justify", "Justify"),
    ];
}

/// Where the quick input (command palette / Go to File) anchors vertically.
/// VS Code's "Quick Input Position". Persisted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum QuickInputPosition {
    #[default]
    Top,
    Center,
}

impl QuickInputPosition {
    /// Both positions with their picker id and label, in the Customize
    /// Layout popup's order (View: Set Quick Input Position…, #852).
    pub(super) const OPTIONS: [(QuickInputPosition, &'static str, &'static str); 2] = [
        (QuickInputPosition::Top, "top", "Top"),
        (QuickInputPosition::Center, "center", "Center"),
    ];
}

/// The editor rect for Centered Layout (#1585): `width` columns in the
/// middle of `area`, or all of `area` when it is no wider than that.
pub(super) fn centered_editor_rect(area: Rect, width: u16) -> Rect {
    if width == 0 || area.width <= width {
        return area;
    }
    Rect {
        x: area.x + (area.width - width) / 2,
        width,
        ..area
    }
}

/// The words the Zen hint shows, with the platform's command key (#1585).
pub(super) fn zen_hint_text() -> &'static str {
    if cfg!(target_os = "macos") {
        "Zen Mode: Cmd+K Z or Esc Esc to exit"
    } else {
        "Zen Mode: Ctrl+K Z or Esc Esc to exit"
    }
}

/// Pure horizontal layout of the workspace chrome within `main`. Column order
/// follows the primary side bar's position (the activity bar always flanks the
/// primary side bar on its outer edge, like VS Code):
///   `Left`:  `[activity][primary][right......][secondary]`
///   `Right`: `[secondary][right......][primary][activity]`
/// The `right` column is guaranteed at least `RIGHT_PANE_MIN` cells; the
/// primary side bar is shrunk to make room when the frame is narrow.
pub(super) fn compute_chrome_layout(
    main: Rect,
    activity_visible: bool,
    primary_visible: bool,
    secondary_visible: bool,
    side_bar_position: SideBarPosition,
    sidebar_w: u16,
    secondary_w: u16,
) -> ChromeLayout {
    let activity_w = if activity_visible {
        ACTIVITY_BAR_WIDTH
    } else {
        0
    };
    // A secondary side bar the window can't seat beside the editor's minimum
    // is dropped: reserving it anyway placed it past the frame's right edge,
    // where its scrollbar wrote outside the buffer (#1132).
    let fits = main.width
        >= activity_w
            .saturating_add(secondary_w)
            .saturating_add(RIGHT_PANE_MIN);
    let secondary_w = if secondary_visible && fits {
        secondary_w
    } else {
        0
    };
    // Reserve activity + secondary first, then fit the primary side bar in
    // what's left while keeping RIGHT_PANE_MIN for the editor column.
    let reserved = activity_w.saturating_add(secondary_w);
    let after_reserved = main.width.saturating_sub(reserved);
    let max_primary = after_reserved.saturating_sub(RIGHT_PANE_MIN);
    let primary_w = if primary_visible {
        sidebar_w.min(max_primary)
    } else {
        0
    };
    let right_w = after_reserved.saturating_sub(primary_w);

    let row = |x: u16, w: u16| Rect {
        x,
        y: main.y,
        width: w,
        height: main.height,
    };
    let mut x = main.x;
    let (activity, primary_side, right, secondary_side, splitter);
    match side_bar_position {
        SideBarPosition::Left => {
            activity = (activity_w > 0).then(|| row(x, activity_w));
            x += activity_w;
            primary_side = (primary_w > 0).then(|| row(x, primary_w));
            x += primary_w;
            let right_rect = row(x, right_w);
            // Seam = editor's left edge.
            splitter = (primary_w > 0).then_some(right_rect.x);
            right = right_rect;
            x += right_w;
            secondary_side = (secondary_w > 0).then(|| row(x, secondary_w));
        }
        SideBarPosition::Right => {
            secondary_side = (secondary_w > 0).then(|| row(x, secondary_w));
            x += secondary_w;
            let right_rect = row(x, right_w);
            right = right_rect;
            x += right_w;
            // Seam = sidebar's left edge (= editor's right edge).
            splitter = (primary_w > 0).then_some(x);
            primary_side = (primary_w > 0).then(|| row(x, primary_w));
            x += primary_w;
            activity = (activity_w > 0).then(|| row(x, activity_w));
        }
    }
    ChromeLayout {
        activity,
        primary_side,
        secondary_side,
        right,
        sidebar_splitter_x: splitter,
    }
}

/// Pure horizontal placement of the bottom panel within its `band`. `Center`
/// and `Justify` fill the band; `Left`/`Right` hug that half (clamped to a
/// usable minimum). The leftover strip is painted with the editor background
/// by the caller so it never ghosts.
pub(super) fn panel_band_rect(band: Rect, alignment: PanelAlignment) -> Rect {
    match alignment {
        PanelAlignment::Center | PanelAlignment::Justify => band,
        PanelAlignment::Left => Rect {
            width: (band.width / 2).max(RIGHT_PANE_MIN).min(band.width),
            ..band
        },
        PanelAlignment::Right => {
            let w = (band.width / 2).max(RIGHT_PANE_MIN).min(band.width);
            Rect {
                x: band.x + band.width - w,
                width: w,
                ..band
            }
        }
    }
}

pub(super) fn activity_icon_glyph_x(bar: Rect) -> u16 {
    bar.x + bar.width / 2
}

pub(super) fn activity_explorer_y(bar: Rect) -> u16 {
    bar.y + 1
}

pub(super) fn activity_search_y(bar: Rect) -> u16 {
    activity_explorer_y(bar) + ACTIVITY_ICON_HEIGHT + ACTIVITY_ICON_GAP
}

pub(super) fn activity_source_control_y(bar: Rect) -> u16 {
    activity_search_y(bar) + ACTIVITY_ICON_HEIGHT + ACTIVITY_ICON_GAP
}

pub(super) fn activity_run_debug_y(bar: Rect) -> u16 {
    activity_source_control_y(bar) + ACTIVITY_ICON_HEIGHT + ACTIVITY_ICON_GAP
}

pub(super) fn activity_remote_y(bar: Rect) -> u16 {
    activity_run_debug_y(bar) + ACTIVITY_ICON_HEIGHT + ACTIVITY_ICON_GAP
}

pub(super) fn activity_extensions_y(bar: Rect) -> u16 {
    activity_remote_y(bar) + ACTIVITY_ICON_HEIGHT + ACTIVITY_ICON_GAP
}

pub(super) fn activity_testing_y(bar: Rect) -> u16 {
    activity_extensions_y(bar) + ACTIVITY_ICON_HEIGHT + ACTIVITY_ICON_GAP
}

pub(super) fn activity_explorer_block(bar: Rect) -> Rect {
    Rect {
        x: bar.x,
        y: activity_explorer_y(bar),
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
    }
}

pub(super) fn activity_search_block(bar: Rect) -> Rect {
    Rect {
        x: bar.x,
        y: activity_search_y(bar),
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
    }
}

pub(super) fn activity_source_control_block(bar: Rect) -> Rect {
    Rect {
        x: bar.x,
        y: activity_source_control_y(bar),
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
    }
}

pub(super) fn activity_remote_block(bar: Rect) -> Rect {
    Rect {
        x: bar.x,
        y: activity_remote_y(bar),
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
    }
}

pub(super) fn activity_run_debug_block(bar: Rect) -> Rect {
    Rect {
        x: bar.x,
        y: activity_run_debug_y(bar),
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
    }
}

pub(super) fn activity_extensions_block(bar: Rect) -> Rect {
    Rect {
        x: bar.x,
        y: activity_extensions_y(bar),
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
    }
}

pub(super) fn activity_testing_block(bar: Rect) -> Rect {
    Rect {
        x: bar.x,
        y: activity_testing_y(bar),
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
    }
}

/// The QL icon (#578), last of the view icons so disabling CodeQL moves no
/// other icon.
pub(super) fn activity_codeql_y(bar: Rect) -> u16 {
    activity_testing_y(bar) + ACTIVITY_ICON_HEIGHT + ACTIVITY_ICON_GAP
}

pub(super) fn activity_codeql_block(bar: Rect) -> Rect {
    Rect {
        x: bar.x,
        y: activity_codeql_y(bar),
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
    }
}

/// The settings gear, anchored to the BOTTOM of the activity bar (VS Code's
/// "Manage" gear), asymmetric to the view icons stacked from the top. Returns
/// an empty rect when the bar is too short to fit the gear below the last view
/// icon, so the caller skips rendering and hit-testing it rather than letting
/// it collide with Run-and-Debug on a tiny terminal.
///
/// Reserves a 1-cell bottom inset (`ACTIVITY_ICON_HEIGHT + 1`) so the gap below
/// the gear matches the 1-cell top inset the view icons get from
/// `activity_explorer_y`'s `bar.y + 1`. Without it the gear hugged the row
/// directly above the status bar, reading as lopsidedly low.
pub(super) const ACTIVITY_BOTTOM_INSET: u16 = 1;

pub(super) fn activity_settings_block(bar: Rect, codeql_shown: bool) -> Rect {
    let y = bar
        .y
        .saturating_add(bar.height)
        .saturating_sub(ACTIVITY_ICON_HEIGHT + ACTIVITY_BOTTOM_INSET);
    if y < activity_last_view_bottom(bar, codeql_shown) {
        return Rect::default();
    }
    Rect {
        x: bar.x,
        y,
        width: bar.width,
        height: ACTIVITY_ICON_HEIGHT,
    }
}

/// The row just below the last view icon, which the bottom-anchored icons
/// must not reach. The last view icon is CodeQL's only once it is added
/// (#1842).
pub(super) fn activity_last_view_bottom(bar: Rect, codeql_shown: bool) -> u16 {
    let last_view_y = if codeql_shown {
        activity_codeql_y(bar)
    } else {
        activity_testing_y(bar)
    };
    last_view_y + ACTIVITY_ICON_HEIGHT
}

/// The sponsor heart (#1940), stacked directly above the gear as VS Code
/// stacks Accounts above Manage. Empty when the gear is, or when the heart
/// would overlap the last view icon, so a shrinking bar drops the heart a
/// slot before it drops the gear.
pub(super) fn activity_sponsor_block(bar: Rect, codeql_shown: bool) -> Rect {
    let gear = activity_settings_block(bar, codeql_shown);
    let y = gear
        .y
        .saturating_sub(ACTIVITY_ICON_HEIGHT + ACTIVITY_ICON_GAP);
    if gear.width == 0 || y < activity_last_view_bottom(bar, codeql_shown) {
        return Rect::default();
    }
    Rect { y, ..gear }
}

/// A tinted badge's fill and ink on a `surfaces` theme's chrome: the hue
/// laid over the chrome at a fifth of its strength (a little more under the
/// pointer), and the hue lifted toward the foreground for the label, so a
/// count badge reads in its own colour without a hard block of it.
pub(super) fn chip_tint(
    t: crate::widgets::surface::Tones,
    hue: (u8, u8, u8),
    hovered: bool,
) -> (Color, Color) {
    let strength = if hovered { 0.32 } else { 0.22 };
    (
        rgb_color(crate::gradient::lerp_rgb(t.chrome, hue, strength)),
        rgb_color(crate::gradient::lerp_rgb(hue, t.lift, 0.35)),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SplitterDrag {
    Sidebar,
    Terminal,
    /// Dragging a seam between two editor groups in the grid. Carries the seam
    /// snapshot taken at mouse-down (stable for the drag's duration), so the
    /// drag adjusts exactly that boundary in a grid of any depth.
    EditorSeam(editor_layout::Seam),
}

/// Minimum width in cells of either editor column while split, so a drag
/// can't collapse a pane to nothing.
pub(super) const EDITOR_SPLIT_MIN: u16 = 10;

pub(super) const SIDEBAR_WIDTH_DEFAULT: u16 = 32;

pub(super) const SIDEBAR_WIDTH_MIN: u16 = 12;

pub(super) const TERMINAL_HEIGHT_MIN: u16 = 3;

pub(super) const EDITOR_HEIGHT_MIN: u16 = 3;

/// Shortest frame that still shows the on-screen keyboard band; below this
/// the workspace would be unusably squeezed, so the band collapses instead.
pub(super) const OSK_MIN_FRAME_HEIGHT: u16 = 16;

pub(super) const RIGHT_PANE_MIN: u16 = 20;

/// Two Escapes in Zen Mode this close together leave it (#1585), the
/// double-press window croft's clicks use too.
pub(super) const ZEN_ESC_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

/// How long the "how to leave Zen" hint shows after entering it (#1585).
pub(super) const ZEN_HINT_FOR: std::time::Duration = std::time::Duration::from_secs(4);

impl App {
    /// Rebuild the cached source-control change-count badge after the change
    /// set, theme, or cell size shifts, and mark the activity overlay dirty so
    /// the next post-draw flush re-emits. On the glyph-fallback path (no inline
    /// images) it only tracks the count for the dirty check; that path draws a
    /// text badge directly in `render_activity_bar`.
    pub(super) fn refresh_scm_change_badge(&mut self) {
        // Across EVERY folder's repository (#161): work pending in a
        // background folder must never be invisible. Single-folder, the
        // sum is exactly the active repo's porcelain count.
        let count: usize = std::iter::once(&self.git)
            .chain(self.git_extra.iter().map(|(_, w)| w))
            .map(|w| w.status().changed_count)
            .sum();
        if !self.overlays.activity.has_images() {
            self.scm_change_badge = None;
            if count != self.scm_change_badge_count {
                self.scm_change_badge_count = count;
                self.overlays.activity.mark_dirty();
            }
            return;
        }
        if count == 0 {
            if self.scm_change_badge.is_some() {
                self.scm_change_badge = None;
                self.overlays.activity.mark_dirty();
            }
            self.scm_change_badge_count = 0;
            return;
        }
        self.scm_change_badge = self.build_count_badge_image(
            count,
            crate::iterm2_inline::KITTY_ID_SCM_BADGE,
            self.theme.accent_rgb(),
        );
        self.scm_change_badge_count = count;
        self.overlays.activity.mark_dirty();
    }

    /// Build the inline-image for an activity-bar count badge (accent pill +
    /// count) at the given Kitty image id. `None` when image rendering is off or
    /// the cell size isn't known yet. Shared by the SCM, Explorer, and Remote
    /// badges so the compose/encode/tmux-wrap chain lives in one place.
    pub(super) fn build_count_badge_image(
        &self,
        count: usize,
        kitty_id: u32,
        color: (u8, u8, u8),
    ) -> Option<String> {
        let (cw, ch) = self.cell_pixel?;
        let png = crate::iterm2_inline::compose_count_badge(
            count,
            cw,
            ch,
            color,
            self.chrome_bg_pixel(),
        )?;
        let raw = crate::iterm2_inline::build_inline_image_z(
            self.inline_protocol,
            &png,
            crate::iterm2_inline::count_badge_cells_w(count),
            1,
            false,
            kitty_id,
            1,
        )?;
        Some(self.wrap_for_terminal(raw))
    }

    /// Rebuild the cached Explorer unsaved-file badge when the count, theme, or
    /// cell size shifts, and mark the activity overlay dirty so the next flush
    /// re-emits. Cheap to call every frame: it recomputes the count and bails
    /// unless it changed. The glyph-fallback path draws a text badge in
    /// `render_activity_bar` instead.
    pub fn refresh_explorer_unsaved_badge(&mut self) {
        let count = self.unsaved_count();
        if !self.overlays.activity.has_images() {
            self.explorer_unsaved_badge = None;
            if count != self.explorer_unsaved_count {
                self.explorer_unsaved_count = count;
                self.overlays.activity.mark_dirty();
            }
            return;
        }
        if count == self.explorer_unsaved_count {
            return;
        }
        if count == 0 {
            self.explorer_unsaved_badge = None;
            self.explorer_unsaved_count = 0;
            self.overlays.activity.mark_dirty();
            return;
        }
        self.explorer_unsaved_badge = self.build_count_badge_image(
            count,
            crate::iterm2_inline::KITTY_ID_EXPLORER_BADGE,
            self.theme.accent_rgb(),
        );
        self.explorer_unsaved_count = count;
        self.overlays.activity.mark_dirty();
    }

    /// Rebuild the cached Remote forwarded-ports badge when the count, theme, or
    /// cell size shifts. Count-gated like the Explorer badge so it's a cheap
    /// per-frame no-op; the glyph-fallback path draws a text badge instead.
    pub fn refresh_remote_ports_badge(&mut self) {
        let count = self.ports.forwarded_count();
        if !self.overlays.activity.has_images() {
            self.remote_ports_badge = None;
            if count != self.remote_ports_count {
                self.remote_ports_count = count;
                self.overlays.activity.mark_dirty();
            }
            return;
        }
        if count == self.remote_ports_count {
            return;
        }
        if count == 0 {
            self.remote_ports_badge = None;
            self.remote_ports_count = 0;
            self.overlays.activity.mark_dirty();
            return;
        }
        self.remote_ports_badge = self.build_count_badge_image(
            count,
            crate::iterm2_inline::KITTY_ID_REMOTE_BADGE,
            self.theme.accent_rgb(),
        );
        self.remote_ports_count = count;
        self.overlays.activity.mark_dirty();
    }

    /// Rebuild the cached Testing failing-count badge (red pill) when the count,
    /// theme, or cell size shifts. Count-gated per-frame no-op; the glyph path
    /// draws a text badge instead.
    pub fn refresh_testing_badge(&mut self) {
        let count = self.testing.failed_count();
        if !self.overlays.activity.has_images() {
            self.testing_badge = None;
            if count != self.testing_badge_count {
                self.testing_badge_count = count;
                self.overlays.activity.mark_dirty();
            }
            return;
        }
        if count == self.testing_badge_count {
            return;
        }
        if count == 0 {
            self.testing_badge = None;
            self.testing_badge_count = 0;
            self.overlays.activity.mark_dirty();
            return;
        }
        self.testing_badge = self.build_count_badge_image(
            count,
            crate::iterm2_inline::KITTY_ID_TESTING_BADGE,
            self.theme.git_deleted_rgb(),
        );
        self.testing_badge_count = count;
        self.overlays.activity.mark_dirty();
    }

    /// Rebuild the cached PROBLEMS-tab circle badge (orange, like the activity
    /// bar badges) when the count, theme, or cell size shifts. The badge is an
    /// inline-image circle emitted post-frame by `flush_problems_badge_overlay`;
    /// glyph-fallback terminals keep the powerline-cap pill in `paint_panel_tabs`.
    /// Count-gated, so a frame with no churn is a cheap no-op.
    pub fn refresh_problems_badge(&mut self) {
        let count = self.problems.total_count();
        if !self.overlays.activity.has_images() {
            self.problems_badge_count = count;
            return;
        }
        if count == self.problems_badge_count {
            return;
        }
        self.problems_badge_count = count;
        if count == 0 {
            // Clear the screen only if a badge was actually drawn: at start-up
            // and on every graphics re-init the count goes from unknown to 0,
            // and on iTerm2 a needless clear is a whole-screen repaint.
            self.overlays.problems_badge.clear_image();
            self.clear_problems_badge_if_emitted();
            return;
        }
        if let Some(img) = self.build_count_badge_image(
            count,
            crate::iterm2_inline::KITTY_ID_PROBLEMS_BADGE,
            crate::gradient::GRAD_TR,
        ) {
            self.overlays.problems_badge.set_image(img);
        }
    }

    /// Map a pointer cell to the activity-bar icon under it, if any. Uses the
    /// same per-icon rectangles the click handler hit-tests, so hover and
    /// click target exactly the same cells.
    pub(super) fn activity_icon_at(&self, col: u16, row: u16) -> Option<ActivityIcon> {
        let a = &self.sidebar_areas;
        if rect_contains(a.explorer_icon, col, row) {
            Some(ActivityIcon::Explorer)
        } else if rect_contains(a.search_icon, col, row) {
            Some(ActivityIcon::Search)
        } else if rect_contains(a.source_control_icon, col, row) {
            Some(ActivityIcon::SourceControl)
        } else if rect_contains(a.remote_icon, col, row) {
            Some(ActivityIcon::Remote)
        } else if rect_contains(a.run_debug_icon, col, row) {
            Some(ActivityIcon::RunDebug)
        } else if rect_contains(a.extensions_icon, col, row) {
            Some(ActivityIcon::Extensions)
        } else if rect_contains(a.testing_icon, col, row) {
            Some(ActivityIcon::Testing)
        } else if rect_contains(a.codeql_icon, col, row) {
            Some(ActivityIcon::CodeQL)
        } else if rect_contains(a.sponsor_icon, col, row) {
            Some(ActivityIcon::Sponsor)
        } else if rect_contains(a.settings_icon, col, row) {
            Some(ActivityIcon::Settings)
        } else {
            None
        }
    }

    /// Anchor a `ContextMenu` at the activity-bar gear. `menu_rect` clamps it
    /// onto the screen, so a bottom-anchored gear pops the menu upward — the
    /// same direction VS Code's "Manage" menu opens.
    pub(super) fn settings_menu_origin(&self) -> (u16, u16) {
        let gear = self.sidebar_areas.settings_icon;
        (gear.x + gear.width, gear.y)
    }

    /// Open the activity-bar gear's settings menu (VS Code's "Manage" menu).
    /// Currently one entry — Color Theme — which opens the theme picker; more
    /// settings entries slot in here later.
    pub(super) fn open_settings_menu(&mut self) {
        let origin = self.settings_menu_origin();
        self.context_menu = Some(ContextMenu::flat(
            origin,
            vec![
                (String::from("Color Theme"), MenuAction::OpenThemePicker),
                (
                    String::from("Customize Layout"),
                    MenuAction::OpenCustomizeLayout,
                ),
            ],
            self.workspace_root().to_path_buf(),
        ));
        // The gear renders "active" while its menu is open; re-emit it so the
        // pressed-state PNG replaces the idle one.
        self.overlays.activity.mark_dirty();
    }

    /// The activity-bar heart and "Help: Sponsor croft" (#1940): open
    /// croft's GitHub Sponsors page in the user's browser.
    pub(super) fn open_sponsor_page(&mut self) {
        self.open_url_for_user(SPONSOR_URL);
    }

    /// Anchor for the Customize Layout popup: just under the Customize Layout
    /// (⛶) title-bar icon when it has been laid out, else the activity-bar gear
    /// (the gear-menu entry path).
    pub(super) fn customize_layout_origin(&self) -> (u16, u16) {
        let icon = self.layout_icon_areas.customize;
        if icon.width > 0 {
            // Pull left so the wider popup doesn't spill off the right edge;
            // `menu_rect` clamps anything that still would.
            (icon.x.saturating_sub(18), icon.y + 1)
        } else {
            self.settings_menu_origin()
        }
    }

    /// Build the Customize Layout popup rows (VS Code's title-bar layout
    /// controls): visibility checkboxes, then radio groups for Side Bar
    /// Position, Panel Alignment, and Quick Input Position, then Zen Mode.
    /// A leading `✔` marks an enabled toggle / the active radio option.
    pub(super) fn customize_layout_items(&self) -> Vec<MenuEntry> {
        let check = |on: bool| if on { "\u{2713} " } else { "  " };
        vec![
            MenuEntry::item(
                format!("{}Activity Bar", check(self.activity_bar_visible)),
                MenuAction::ToggleActivityBar,
            ),
            MenuEntry::item(
                format!("{}Primary Side Bar", check(self.show_tree)),
                MenuAction::ToggleSideBar,
            ),
            MenuEntry::item(
                format!(
                    "{}Secondary Side Bar",
                    check(self.secondary_side_bar_visible)
                ),
                MenuAction::ToggleSecondarySideBar,
            ),
            MenuEntry::item(
                format!("{}Panel", check(self.show_terminal)),
                MenuAction::TogglePanel,
            ),
            MenuEntry::item(
                format!("{}Status Bar", check(self.status_bar_visible)),
                MenuAction::ToggleStatusBar,
            ),
            MenuEntry::item(
                format!("{}Minimap", check(self.minimap_visible)),
                MenuAction::ToggleMinimap,
            ),
            MenuEntry::item(
                format!("{}Auto-Hide Side Bar", check(self.sidebar_auto_hide)),
                MenuAction::ToggleAutoHideSideBar,
            ),
            MenuEntry::Separator,
            MenuEntry::header("Primary Side Bar Position"),
            MenuEntry::item(
                format!(
                    "{}Left",
                    check(self.side_bar_position == SideBarPosition::Left)
                ),
                MenuAction::SetSideBarPosition(SideBarPosition::Left),
            ),
            MenuEntry::item(
                format!(
                    "{}Right",
                    check(self.side_bar_position == SideBarPosition::Right)
                ),
                MenuAction::SetSideBarPosition(SideBarPosition::Right),
            ),
            MenuEntry::Separator,
            MenuEntry::header("Panel Alignment"),
            MenuEntry::item(
                format!(
                    "{}Left",
                    check(self.panel_alignment == PanelAlignment::Left)
                ),
                MenuAction::SetPanelAlignment(PanelAlignment::Left),
            ),
            MenuEntry::item(
                format!(
                    "{}Center",
                    check(self.panel_alignment == PanelAlignment::Center)
                ),
                MenuAction::SetPanelAlignment(PanelAlignment::Center),
            ),
            MenuEntry::item(
                format!(
                    "{}Right",
                    check(self.panel_alignment == PanelAlignment::Right)
                ),
                MenuAction::SetPanelAlignment(PanelAlignment::Right),
            ),
            MenuEntry::item(
                format!(
                    "{}Justify",
                    check(self.panel_alignment == PanelAlignment::Justify)
                ),
                MenuAction::SetPanelAlignment(PanelAlignment::Justify),
            ),
            MenuEntry::Separator,
            MenuEntry::header("Quick Input Position"),
            MenuEntry::item(
                format!(
                    "{}Top",
                    check(self.quick_input_position == QuickInputPosition::Top)
                ),
                MenuAction::SetQuickInputPosition(QuickInputPosition::Top),
            ),
            MenuEntry::item(
                format!(
                    "{}Center",
                    check(self.quick_input_position == QuickInputPosition::Center)
                ),
                MenuAction::SetQuickInputPosition(QuickInputPosition::Center),
            ),
            MenuEntry::Separator,
            MenuEntry::item(
                format!("{}Zen Mode", check(self.zen_mode)),
                MenuAction::ToggleZenMode,
            ),
            MenuEntry::item(
                format!("{}Centered Layout", check(self.centered_layout_active())),
                MenuAction::ToggleCenteredLayout,
            ),
        ]
    }

    /// Reopen the Customize Layout popup with the cursor on the row carrying
    /// `action`. Looking the row up beats hardcoding an index: the indices
    /// were already stale for every row below the first separator (a row
    /// insert shifts them and nothing catches it), so this makes that whole
    /// class of bug unrepresentable.
    pub(super) fn open_customize_layout_menu_on(&mut self, action: &MenuAction) {
        let idx = self
            .customize_layout_items()
            .iter()
            .position(|e| matches!(e, MenuEntry::Item { action: a, .. } if a == action))
            .unwrap_or(0);
        self.open_customize_layout_menu_at(idx);
    }

    /// Open (or re-open) the Customize Layout popup with the cursor on row
    /// `selected`. Each toggle re-opens it so the popup stays put while the
    /// user flips several controls, exactly like VS Code's dropdown.
    pub(super) fn open_customize_layout_menu_at(&mut self, selected: usize) {
        let origin = self.customize_layout_origin();
        self.context_menu = Some(ContextMenu {
            crumb: None,
            scroll: Default::default(),
            submenu_scroll: Default::default(),
            origin,
            items: self.customize_layout_items(),
            selected,
            open_submenu: None,
            submenu_selected: 0,
            target_dir: self.workspace_root().to_path_buf(),
        });
        self.overlays.activity.mark_dirty();
    }

    /// Open the Customize Layout popup fresh (cursor on the first row).
    pub(super) fn open_customize_layout_menu(&mut self) {
        self.open_customize_layout_menu_at(0);
    }

    /// Persist the current Customize Layout chrome to `~/.config/croft`,
    /// saying so when that fails. Skipped under test (hermetic suite, like
    /// `apply_theme`).
    pub(super) fn persist_layout(&mut self) {
        if cfg!(test) {
            return;
        }
        let saved = crate::prefs::save_layout(crate::prefs::LayoutPrefs {
            activity_bar: self.activity_bar_visible,
            status_bar: self.status_bar_visible,
            minimap: self.minimap_visible,
            side_bar_position: self.side_bar_position,
            secondary_side_bar: self.secondary_side_bar_visible,
            panel_alignment: self.panel_alignment,
            quick_input_position: self.quick_input_position,
            centered_layout: self.centered_layout,
            zen_center_layout: self.zen_center_layout,
            centered_layout_width: self.centered_layout_width,
        });
        self.report_unsaved_setting(saved);
    }

    /// Show or hide the activity bar. The Customize Layout row and the
    /// palette's View: Toggle Activity Bar Visibility (#852) both land here.
    pub(super) fn toggle_activity_bar(&mut self) {
        self.activity_bar_visible = !self.activity_bar_visible;
        // The bar is a structural auto-hide suppression, and unlike Zen
        // it flips here on its own. A pending collapse armed against the
        // old chrome would otherwise sit armed while `allowed()` declines
        // for it, then fire on the first idle tick after the bar comes
        // back, with no focus move of the user's own.
        //
        // Cancel the collapse. And when the bar goes AWAY, drop the pin
        // too — for the reason `toggle_side_bar` already refuses to bank
        // one in that state: no collapse can fire while the bar is
        // hidden, so a pin held across that period is unconsumable, and
        // it silently eats the first real collapse after the bar
        // returns. Those two sites disagreed about the same question
        // until this line; the bank-time answer is the right one.
        //
        // Revealing the bar leaves any pin alone: a pin banked while the
        // bar is visible belongs to a reveal that can still be honoured.
        self.sidebar_dwell.disarm();
        if !self.activity_bar_visible {
            self.sidebar_pinned_open = false;
        }
        self.persist_layout();
        self.after_chrome_visibility_change();
    }

    /// Show or hide the status bar: the Customize Layout row and View:
    /// Toggle Status Bar Visibility (#852).
    pub(super) fn toggle_status_bar(&mut self) {
        self.status_bar_visible = !self.status_bar_visible;
        self.persist_layout();
        self.after_chrome_visibility_change();
    }

    /// Dock the primary side bar (and the activity bar with it) to `pos`:
    /// the Customize Layout radio rows and View: Toggle Primary Side Bar
    /// Position (#852).
    pub(super) fn set_side_bar_position(&mut self, pos: SideBarPosition) {
        self.side_bar_position = pos;
        self.persist_layout();
        self.after_chrome_visibility_change();
    }

    /// The bottom panel's alignment: the Customize Layout radio rows and
    /// View: Set Panel Alignment… (#852).
    pub(super) fn set_panel_alignment(&mut self, al: PanelAlignment) {
        self.panel_alignment = al;
        self.persist_layout();
    }

    /// Where the quick input anchors: the Customize Layout radio rows and
    /// View: Set Quick Input Position… (#852).
    pub(super) fn set_quick_input_position(&mut self, pos: QuickInputPosition) {
        self.quick_input_position = pos;
        self.persist_layout();
    }

    /// View: Set Panel Alignment… (#852): the popup's Panel Alignment radio
    /// group as a keyboard picker, opened on the current alignment.
    pub(super) fn open_panel_alignment_picker(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let rows = PanelAlignment::OPTIONS
            .iter()
            .map(|(_, id, label)| ListRow {
                id: (*id).to_string(),
                label: (*label).to_string(),
            })
            .collect();
        let mut picker = ListPicker::new(ListPurpose::PanelAlignment, "Panel Alignment", rows);
        picker.selected = PanelAlignment::OPTIONS
            .iter()
            .position(|(al, ..)| *al == self.panel_alignment)
            .unwrap_or(0);
        self.open_list_picker(picker, "No panel alignments");
    }

    /// View: Set Quick Input Position… (#852): the popup's Quick Input
    /// Position radio group as a keyboard picker, opened on the current one.
    pub(super) fn open_quick_input_position_picker(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let rows = QuickInputPosition::OPTIONS
            .iter()
            .map(|(_, id, label)| ListRow {
                id: (*id).to_string(),
                label: (*label).to_string(),
            })
            .collect();
        let mut picker = ListPicker::new(
            ListPurpose::QuickInputPosition,
            "Quick Input Position",
            rows,
        );
        picker.selected = QuickInputPosition::OPTIONS
            .iter()
            .position(|(pos, ..)| *pos == self.quick_input_position)
            .unwrap_or(0);
        self.open_list_picker(picker, "No quick input positions");
    }

    /// Hide the activity bar's inline images when the bar is collapsed so the
    /// OSC-1337 icons don't ghost over the editor; arm a re-emit when shown.
    pub(super) fn after_chrome_visibility_change(&mut self) {
        self.overlays.activity.mark_dirty();
        if self.overlays.activity.has_images() {
            self.overlays.activity.request_clear();
        }
        // The welcome logo re-bakes against the new editor rect on next render.
        self.overlays.welcome.mark_dirty();
    }

    /// Show or hide the primary side bar. Every route that flips it by hand —
    /// the Cmd+B keybinding, the palette command, the Customize Layout row and
    /// the layout-icon click — goes through here, because each one is a
    /// deliberate sidebar action and so must take the auto-hide pin (#260).
    /// Four hand-rolled copies is exactly how three of them ended up without
    /// it: the pin lived only on the palette route, so the real Cmd+B revealed
    /// the panel and the next click into the editor collapsed it again.
    pub(super) fn toggle_side_bar(&mut self) {
        let was_visible = self.show_tree;
        self.show_tree = !self.show_tree;
        // A reveal the user asked for outlives the focus move that follows it.
        // Taken only when auto-hide is live: a pin set while the feature is
        // off would sit unconsumed and silently eat the first collapse after
        // they turn it on.
        // Structural suppression (Zen Mode, or a hidden activity bar) means no
        // collapse can fire for as long as it lasts, so a pin taken now is the
        // "sits unconsumed and silently eats a later collapse" case this line
        // already guards against for `!sidebar_auto_hide` — same bug, same
        // answer: refuse to bank it rather than hold it indefinitely.
        self.sidebar_pinned_open =
            self.show_tree && self.sidebar_auto_hide && self.activity_bar_visible && !self.zen_mode;
        // A deliberate toggle supersedes any pending automatic collapse: the
        // dwell was armed against a sidebar state the user has since changed by
        // hand. Leaving it armed lets a later idle tick fire against the NEW
        // state — spending the pin this reveal just banked on a collapse the
        // user never saw, which is #294 reached by a different door.
        self.sidebar_dwell.disarm();
        // Hiding the sidebar while Run-Debug was on screen: arm the same
        // OSC-1337 image-cell evict gate that fires on a sidebar-view change.
        // Without this the bug+play icon ghosts on top of the editor /
        // terminal that fills the space the panel just vacated.
        if was_visible
            && !self.show_tree
            && self.sidebar_view == SidebarView::RunDebug
            && self.overlays.run_debug.was_emitted()
        {
            self.overlays.run_debug.request_clear();
        }
    }

    /// Toggle the secondary side bar (the Outline of the active file), on the
    /// edge opposite the primary side bar. Mirrors ⌥⌘B.
    pub(super) fn toggle_secondary_side_bar(&mut self) {
        self.secondary_side_bar_visible = !self.secondary_side_bar_visible;
        self.after_chrome_visibility_change();
        self.status = String::from(if self.secondary_side_bar_visible {
            "Secondary side bar shown"
        } else {
            "Secondary side bar hidden"
        });
        self.persist_layout();
    }

    /// Toggle Zen Mode: hide the activity bar, both side bars, the panel, and
    /// the status bar; toggling off restores the pre-Zen snapshot. Mirrors VS
    /// Code's ⌘K Z.
    pub(super) fn toggle_zen_mode(&mut self) {
        if let Some(prev) = self.pre_zen.take() {
            self.activity_bar_visible = prev.activity_bar;
            self.status_bar_visible = prev.status_bar;
            self.show_tree = prev.side_bar;
            self.secondary_side_bar_visible = prev.secondary_side_bar;
            self.show_terminal = prev.panel;
            self.zen_mode = false;
            self.zen_esc_at = None;
            self.zen_hint_until = None;
            self.status = String::from("Zen Mode off");
        } else {
            self.pre_zen = Some(PreZenLayout {
                activity_bar: self.activity_bar_visible,
                status_bar: self.status_bar_visible,
                side_bar: self.show_tree,
                secondary_side_bar: self.secondary_side_bar_visible,
                panel: self.show_terminal,
            });
            self.activity_bar_visible = false;
            self.status_bar_visible = false;
            self.show_tree = false;
            // Zen is structural, not a transient suppression: it can last the
            // session, and a dwell left armed would fire on the frame Zen
            // exits — a collapse the user long since stopped associating with
            // the focus move that armed it.
            //
            // The pin goes too, for the reason `toggle_side_bar` refuses to
            // bank one here: Zen sets both structural conditions, so no
            // collapse can fire while it lasts and a pin held across it is
            // unconsumable — it would silently eat the first real collapse
            // after Zen exits. Main cleared this incidentally, by taking the
            // pin on every focus move; consuming at fire time instead is
            // correct but leaves this boundary to say so explicitly.
            self.sidebar_dwell.disarm();
            self.sidebar_pinned_open = false;
            self.secondary_side_bar_visible = false;
            self.show_terminal = false;
            self.zen_mode = true;
            self.zen_centered = self.zen_center_layout;
            self.zen_esc_at = None;
            self.zen_hint_until = Some(std::time::Instant::now() + ZEN_HINT_FOR);
            self.status = String::from("Zen Mode on");
        }
        self.after_chrome_visibility_change();
    }

    /// Whether the editor is centred this frame (#1585): Toggle Centered
    /// Layout, or a Zen session that started centred.
    pub(super) fn centered_layout_active(&self) -> bool {
        self.centered_layout || (self.zen_mode && self.zen_centered)
    }

    /// View: Toggle Centered Layout (#1585). Off turns off both the setting
    /// and a centred Zen session, so the toggle always flips what is on
    /// screen; the setting persists, Zen's own centring does not.
    pub(super) fn toggle_centered_layout(&mut self) {
        if self.centered_layout_active() {
            self.centered_layout = false;
            self.zen_centered = false;
        } else {
            self.centered_layout = true;
        }
        self.status = String::from(if self.centered_layout_active() {
            "Centered Layout on"
        } else {
            "Centered Layout off"
        });
        self.persist_layout();
    }

    /// One Esc reached Zen Mode's editor and closed or cleared nothing
    /// (#1585): the second inside [`ZEN_ESC_WINDOW`] leaves Zen, as VS
    /// Code's Esc Esc does.
    pub(super) fn note_idle_zen_esc(&mut self, now: std::time::Instant) {
        match self.zen_esc_at.take() {
            Some(at) if now.saturating_duration_since(at) <= ZEN_ESC_WINDOW => {
                self.toggle_zen_mode();
            }
            _ => self.zen_esc_at = Some(now),
        }
    }

    /// What an Esc in the editor can close or clear (#1585), compared
    /// across one Esc to tell an Esc that did something from one that did
    /// nothing. Only an Esc that did nothing counts toward Esc Esc.
    pub(super) fn esc_fingerprint(&self) -> impl PartialEq + use<> {
        (
            (
                self.focus,
                self.zen_mode,
                self.modal_overlay_open(),
                self.context_menu.is_some(),
                self.prompt.is_some(),
                self.peek_popup.is_some(),
                self.hover_popup.is_some(),
                self.completion_popup.is_some(),
                self.signature_help_popup.is_some(),
            ),
            (
                self.editor.selection,
                self.editor.has_multi_cursor(),
                self.editor.snippet_active(),
                self.editor
                    .markdown_preview
                    .as_ref()
                    .map(|md| md.has_selection()),
                self.status.clone(),
            ),
            (
                self.editor.comment_focus.is_some(),
                self.vim.enabled,
                self.vim.mode(),
                self.vim.has_pending(),
            ),
        )
    }

    /// Drop the Zen hint once its time is up; true when it just went, so
    /// the frame without it is drawn (#1585).
    pub(super) fn expire_zen_hint(&mut self, now: std::time::Instant) -> bool {
        if self.zen_hint_until.is_some_and(|until| now >= until) {
            self.zen_hint_until = None;
            return true;
        }
        false
    }

    /// Developer: Toggle Screencast Mode (VS Code's; Shift+F8 here): the
    /// keys pressed show on screen for a recording until it is toggled off.
    pub(super) fn toggle_screencast_mode(&mut self) {
        self.status = String::from(if self.screencast.toggle() {
            "Screencast Mode on: the keys you press show at the bottom (Shift+F8 to stop)"
        } else {
            "Screencast Mode off"
        });
    }

    /// Whether a character typed now may be a secret, which Screencast Mode
    /// then leaves off the screen: a password or passphrase field, or the
    /// shell, which reads a sudo password with echo off. What the shell
    /// does echo is on screen already.
    pub(super) fn screencast_hides_typing(&self) -> bool {
        self.input_prompt.as_ref().is_some_and(|p| p.masked)
            || self
                .connect_dialog
                .as_ref()
                .is_some_and(|d| d.input_is_secret())
            || (self.focus == Pane::Terminal && !self.modal_overlay_open())
    }

    /// Screencast Mode's keys, over everything else, in the workspace above
    /// the status bar and the on-screen keyboard, and above the Zen hint
    /// while that shows; clear of the caret being typed at.
    pub(super) fn render_screencast(&self, frame: &mut ratatui::Frame, main_area: Rect) {
        if !self.screencast.is_on() {
            return;
        }
        let mut area = main_area;
        if self.zen_mode
            && self
                .zen_hint_until
                .is_some_and(|until| std::time::Instant::now() < until)
        {
            area.height = area.height.saturating_sub(4);
        }
        let caret = match self.focus {
            Pane::Editor if self.editor.diff.is_some() => self.editor.diff_caret_screen_pos(),
            Pane::Editor => self.editor.cursor_screen_pos(),
            Pane::Terminal if self.show_terminal => self
                .terminals
                .get(self.active_terminal)
                .and_then(|t| t.screen_cursor()),
            _ => None,
        };
        self.screencast
            .render(frame.buffer_mut(), area, self.theme, caret);
    }

    /// Paint the three layout-control icons (Toggle Primary Side Bar, Toggle
    /// Panel, Customize Layout) right-aligned on the single-row `row` — the top
    /// of the editor tab strip, or the welcome screen's top edge. croft's
    /// analog of VS Code's title-bar layout controls (placement A). Records the
    /// hit rects in `self.layout_icon_areas`.
    pub(super) fn paint_layout_icons(&mut self, frame: &mut ratatui::Frame, row: Rect) {
        self.layout_icon_areas = LayoutIconAreas::default();
        let w = LAYOUT_ICON_CELLS_W;
        let stride = w + 1; // one-column gap between icons
        // Three icons + a one-column right pad.
        let needed = LAYOUT_ICONS_W;
        if row.height == 0 || row.width < needed {
            return;
        }
        let y = row.y;
        let right = row.x + row.width;
        let cust_x = right.saturating_sub(w + 1); // 1-col right pad
        let panel_x = cust_x.saturating_sub(stride);
        let side_x = panel_x.saturating_sub(stride);
        if side_x < row.x {
            return;
        }
        let rect = |x: u16| Rect {
            x,
            y,
            width: w,
            height: LAYOUT_ICON_CELLS_H,
        };
        self.layout_icon_areas = LayoutIconAreas {
            toggle_side_bar: rect(side_x),
            toggle_panel: rect(panel_x),
            customize: rect(cust_x),
        };
        // On an image-capable terminal the post-draw flush paints crisp 2× PNG
        // icons at these cells (see `pending_activity_image_overlays`); the text
        // path below is only the fallback for terminals without inline images.
        if self.overlays.activity.has_images() {
            return;
        }
        let pointer = self.pointer_cell;
        // The codicon shape carries the on/off state (filled vs. hollow), like
        // VS Code; colour brightens to the accent on hover. Glyphs sit centred
        // in the 4-wide slot.
        let base = self.theme.ui(Color::Rgb(0xcc, 0xcc, 0xcc));
        let accent = self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff));
        let surfaces = self.theme.surfaces().is_some();
        let theme = self.theme;
        let mut paint = |x: u16, glyph: char| {
            let hovered = pointer.is_some_and(|(px, py)| py == y && px >= x && px < x + w);
            // On a `surfaces` theme these are header actions like the side
            // bar's: muted at rest, and under the pointer the strong ink on
            // a faint chip across the whole two-cell slot, never the accent.
            let style = if surfaces {
                crate::widgets::header_pill::action_style(theme, hovered)
            } else {
                Style::default().fg(if hovered { accent } else { base })
            };
            let buf = frame.buffer_mut();
            if surfaces && hovered {
                let chip = Rect::new(x, y, w, 1).intersection(buf.area);
                buf.set_style(chip, style);
            }
            buf.set_string(x + w / 2, y, glyph.to_string(), style);
        };
        let side_glyph = if self.show_tree {
            crate::icons::LAYOUT_SIDEBAR_ON
        } else {
            crate::icons::LAYOUT_SIDEBAR_OFF
        };
        let panel_glyph = if self.show_terminal {
            crate::icons::LAYOUT_PANEL_ON
        } else {
            crate::icons::LAYOUT_PANEL_OFF
        };
        paint(side_x, side_glyph);
        paint(panel_x, panel_glyph);
        paint(cust_x, crate::icons::LAYOUT_CUSTOMIZE);
    }

    pub(super) fn render_activity_bar(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        // Keep the per-frame activity badges current before the post-draw flush
        // emits them. Both are count-gated, so a frame with no churn is a cheap
        // no-op. (The SCM badge refreshes off the git worker instead.)
        self.refresh_explorer_unsaved_badge();
        self.refresh_remote_ports_badge();
        self.refresh_testing_badge();
        self.refresh_problems_badge();
        self.overlays.activity.note_side_bar_shown(self.show_tree);
        // In images mode the activity bar inherits the iTerm session bg
        // (forced to sRGB(EDITOR_BG_RGB) via SetColors), matching the rest
        // of the panes. The glyph-fallback path keeps a solid bg for
        // terminals that can't render OSC-1337.
        // Croft Loch's bar is chrome, and its baked icons carry the same
        // tone (see `chrome_bg_pixel`).
        let bar_bg = self
            .theme
            .surfaces()
            .map_or(self.theme.editor_bg(), |s| s.chrome());
        let bg = if self.overlays.activity.has_images() {
            Style::default().bg(Color::Reset)
        } else {
            Style::default().bg(bar_bg)
        };
        // In images mode the icon PNG owns the entire activity-bar block —
        // background, codicon, and active pill are baked in. Rendering a bg
        // block here would force a per-cell diff every frame, which in turn
        // would force us to re-emit the OSC-1337 images on every draw. Both
        // are visible to the user. So in images mode we leave the cells
        // untouched and let the post-draw OSC writer paint them once.
        if !self.overlays.activity.has_images() {
            frame.render_widget(ratatui::widgets::Block::default().style(bg), area);
        }
        let active_bar = self.theme.activity_icon_pill();
        let bg_color = bg.bg.unwrap_or(Color::Reset);
        let explorer_block = activity_explorer_block(area);
        let search_block = activity_search_block(area);
        let source_control_block = activity_source_control_block(area);
        let remote_block = activity_remote_block(area);
        let run_debug_block = activity_run_debug_block(area);
        let extensions_block = activity_extensions_block(area);
        let testing_block = activity_testing_block(area);
        let codeql_block = if self.is_extension_enabled("codeql") {
            activity_codeql_block(area)
        } else {
            Rect::default()
        };
        let settings_block = activity_settings_block(area, codeql_block.width > 0);
        let sponsor_block = activity_sponsor_block(area, codeql_block.width > 0);
        // The baked icons cover their own rows in the chrome tone; Croft
        // Loch paints only the rows between them, so no icon cell is ever
        // rewritten under its image.
        if self.overlays.activity.has_images()
            && let Some(s) = self.theme.surfaces()
        {
            let icons = [
                explorer_block,
                search_block,
                source_control_block,
                remote_block,
                run_debug_block,
                extensions_block,
                testing_block,
                codeql_block,
                sponsor_block,
                settings_block,
            ];
            for y in area.top()..area.bottom() {
                if !icons.iter().any(|b| y >= b.top() && y < b.bottom()) {
                    frame.render_widget(
                        ratatui::widgets::Block::default().style(Style::default().bg(s.chrome())),
                        Rect {
                            x: area.x,
                            y,
                            width: area.width,
                            height: 1,
                        },
                    );
                }
            }
        }
        let explorer_active = self.activity_view_active(SidebarView::Explorer);
        let search_active = self.activity_view_active(SidebarView::Search);
        let source_control_active = self.activity_view_active(SidebarView::SourceControl);
        let remote_active = self.activity_view_active(SidebarView::Remote);
        let run_debug_active = self.activity_view_active(SidebarView::RunDebug);
        let extensions_active = self.activity_view_active(SidebarView::Extensions);
        let testing_active = self.activity_view_active(SidebarView::Testing);
        let codeql_active = self.activity_view_active(SidebarView::CodeQL);
        let settings_active = self.settings_menu_active();
        // Hover brightens the glyph (like active) but draws no selection pill,
        // mirroring the image path's hovered variant. Selected wins over hover.
        let hov = self.hovered_activity_icon;
        let explorer_hovered = hov == Some(ActivityIcon::Explorer);
        let search_hovered = hov == Some(ActivityIcon::Search);
        let source_control_hovered = hov == Some(ActivityIcon::SourceControl);
        let remote_hovered = hov == Some(ActivityIcon::Remote);
        let run_debug_hovered = hov == Some(ActivityIcon::RunDebug);
        let extensions_hovered = hov == Some(ActivityIcon::Extensions);
        let testing_hovered = hov == Some(ActivityIcon::Testing);
        let codeql_hovered = hov == Some(ActivityIcon::CodeQL);
        let sponsor_hovered = hov == Some(ActivityIcon::Sponsor);
        let settings_hovered = hov == Some(ActivityIcon::Settings);

        // Same palette the baked icons use, so an image-less terminal renders
        // the bar in the same colors as an image-capable one instead of a
        // second, slightly-off set of greys.
        let active_color = self.theme.activity_icon_active();
        let hover_color = self.theme.activity_icon_hover();
        let inactive_color = self.theme.activity_icon_inactive();
        // Codicons have no bold weight; on Croft Loch bold would only set
        // off some terminals' bold-as-bright, so its glyphs stay regular.
        let tones = crate::widgets::surface::tones(self.theme);
        let glyph_mods = if tones.is_some() {
            Modifier::empty()
        } else {
            Modifier::BOLD
        };
        let glyph_x = activity_icon_glyph_x(area);
        let render_glyph = |frame: &mut ratatui::Frame,
                            block: Rect,
                            glyph: char,
                            is_active: bool,
                            is_hovered: bool| {
            let mid = block.y + block.height.saturating_sub(1) / 2;
            if is_active {
                let pill = Rect {
                    x: block.x,
                    y: mid,
                    width: 1,
                    height: 1,
                };
                frame.render_widget(
                    ratatui::widgets::Paragraph::new("▎")
                        .style(Style::default().fg(active_bar).bg(bg_color)),
                    pill,
                );
            }
            let cell = Rect {
                x: glyph_x,
                y: mid,
                width: 1,
                height: 1,
            };
            // Selected wins over hover; hover is its own step (body text on
            // Croft Loch, the active ink elsewhere).
            let color = if is_active {
                active_color
            } else if is_hovered {
                hover_color
            } else {
                inactive_color
            };
            frame.render_widget(
                ratatui::widgets::Paragraph::new(glyph.to_string()).style(
                    Style::default()
                        .fg(color)
                        .bg(bg_color)
                        .add_modifier(glyph_mods),
                ),
                cell,
            );
        };
        // Text analogue of the baked count badge for image-less terminals: an
        // accent cell with the count at the icon's bottom-right corner. Croft
        // Loch tints it instead, the accent laid into the chrome with the
        // digits in the accent lifted toward the foreground, so the count
        // reads without a hard teal block beside a muted icon.
        let badge_style = match tones {
            Some(t) => {
                let (fill, ink) = chip_tint(t, t.accent, false);
                Style::default().fg(ink).bg(fill)
            }
            None => Style::default()
                .fg(Color::White)
                .bg(self.theme.accent())
                .add_modifier(Modifier::BOLD),
        };
        let render_count_badge = |frame: &mut ratatui::Frame, block: Rect, count: usize| {
            if count == 0 {
                return;
            }
            let label = if count > 9 {
                String::from("9+")
            } else {
                count.to_string()
            };
            let w = label.chars().count() as u16;
            frame.render_widget(
                ratatui::widgets::Paragraph::new(label).style(badge_style),
                Rect {
                    x: block.x + block.width.saturating_sub(w),
                    y: block.y + block.height.saturating_sub(1),
                    width: w,
                    height: 1,
                },
            );
        };

        if !self.overlays.activity.has_images() {
            // Glyph fallback path: render the codicon and a separate active
            // pill on the leftmost column. iTerm2's image path bakes the
            // pill into the PNG itself, so this branch is only used on
            // terminals that can't render OSC-1337.
            render_glyph(
                frame,
                explorer_block,
                crate::icons::ACTIVITY_EXPLORER,
                explorer_active,
                explorer_hovered,
            );
            render_count_badge(frame, explorer_block, self.unsaved_count());
            render_glyph(
                frame,
                search_block,
                crate::icons::ACTIVITY_SEARCH,
                search_active,
                search_hovered,
            );
            render_glyph(
                frame,
                source_control_block,
                crate::icons::ACTIVITY_SOURCE_CONTROL,
                source_control_active,
                source_control_hovered,
            );
            let scm_count = if self.scm_worker().status().in_repo {
                self.source_control.changes_count()
            } else {
                0
            };
            render_count_badge(frame, source_control_block, scm_count);
            render_glyph(
                frame,
                remote_block,
                crate::icons::ACTIVITY_REMOTE,
                remote_active,
                remote_hovered,
            );
            render_count_badge(frame, remote_block, self.ports.forwarded_count());
            render_glyph(
                frame,
                run_debug_block,
                crate::icons::ACTIVITY_RUN_DEBUG,
                run_debug_active,
                run_debug_hovered,
            );
            render_glyph(
                frame,
                extensions_block,
                crate::icons::ACTIVITY_EXTENSIONS,
                extensions_active,
                extensions_hovered,
            );
            render_glyph(
                frame,
                testing_block,
                crate::icons::ACTIVITY_TESTING,
                testing_active,
                testing_hovered,
            );
            render_count_badge(frame, testing_block, self.testing.failed_count());
            if codeql_block.width > 0 {
                render_glyph(
                    frame,
                    codeql_block,
                    crate::icons::ACTIVITY_CODEQL,
                    codeql_active,
                    codeql_hovered,
                );
            }
            if sponsor_block.width > 0 {
                // The heart's own pink, not the bar's greys (see the bake).
                let color = if sponsor_hovered {
                    self.theme.activity_sponsor_hover()
                } else {
                    self.theme.activity_sponsor()
                };
                frame.render_widget(
                    ratatui::widgets::Paragraph::new(crate::icons::ACTIVITY_SPONSOR.to_string())
                        .style(
                            Style::default()
                                .fg(color)
                                .bg(bg_color)
                                .add_modifier(glyph_mods),
                        ),
                    Rect {
                        x: glyph_x,
                        y: sponsor_block.y + sponsor_block.height.saturating_sub(1) / 2,
                        width: 1,
                        height: 1,
                    },
                );
            }
            if settings_block.width > 0 {
                render_glyph(
                    frame,
                    settings_block,
                    crate::icons::ACTIVITY_SETTINGS,
                    settings_active,
                    settings_hovered,
                );
            }
        }

        self.sidebar_areas.explorer_icon = explorer_block;
        self.sidebar_areas.search_icon = search_block;
        self.sidebar_areas.source_control_icon = source_control_block;
        self.sidebar_areas.remote_icon = remote_block;
        self.sidebar_areas.run_debug_icon = run_debug_block;
        self.sidebar_areas.extensions_icon = extensions_block;
        self.sidebar_areas.testing_icon = testing_block;
        self.sidebar_areas.codeql_icon = codeql_block;
        self.sidebar_areas.sponsor_icon = sponsor_block;
        self.sidebar_areas.settings_icon = settings_block;
    }

    pub(super) fn set_sidebar_view(&mut self, view: SidebarView) {
        // Choosing a sidebar view is a deliberate sidebar action, so it must
        // reveal under auto-hide (#260). No pin is needed: every arm below
        // focuses Pane::Tree, and auto-hide only collapses on a move to the
        // editor or a terminal, so the reveal is not at risk from its own
        // focus move.
        if self.sidebar_view != view {
            self.overlays.activity.mark_dirty();
            // Leaving Search while a scan is in flight: the worker cancels
            // the live walker threads instead of letting them keep
            // saturating every core until the scan finishes. The user's hits
            // stay populated for when they come back, when the search runs
            // again to finish (#2608).
            if self.sidebar_view == SidebarView::Search && view != SidebarView::Search {
                self.cancel_search_run();
            }
            // Leaving Run-Debug after the headline icon was on screen:
            // arm a one-shot `terminal.clear()` so iTerm2 evicts the
            // cached OSC-1337 image cells. Plain SGR overwrites do not
            // reliably evict those cells, but `\x1b[2J` (which
            // `terminal.clear()` emits) does — same gate the welcome
            // wordmark and editor preview pipelines use. The post-draw
            // flush function is silent on the non-Run-Debug path, so
            // nothing stomps the freshly-drawn next sidebar.
            if self.sidebar_view == SidebarView::RunDebug
                && view != SidebarView::RunDebug
                && self.overlays.run_debug.was_emitted()
            {
                self.overlays.run_debug.request_clear();
            }
            // Same eviction trick for the Source Control no-repo hero
            // PNG: leaving Source Control after the image was painted
            // would otherwise leave the cached PNG under whatever
            // sidebar replaces it.
            if self.sidebar_view == SidebarView::SourceControl && view != SidebarView::SourceControl
            {
                self.overlays.hero.request_clear_if_displayed();
            }
            // The Source Control commit dropdown is anchored to that view;
            // close it on any view change so it can't linger over the next
            // sidebar.
            self.commit_menu_open = false;
        }
        self.sidebar_view = view;
        if self.sidebar_view == SidebarView::Remote && self.remote.refresh_if_config_changed() {
            self.status = String::from("Reloaded SSH remotes");
        }
        if !self.show_tree {
            self.show_tree = true; // ensure the side panel is open when switching
        }
        match view {
            SidebarView::Explorer => self.focus_pane(Pane::Tree),
            SidebarView::Search => {
                self.focus_pane(Pane::Tree); // tree pane = side panel; dispatch by view
                self.search.select_all_query();
                // A run stopped on leaving Search finishes now.
                self.resume_cancelled_search();
            }
            SidebarView::SourceControl => {
                self.refresh_source_control();
                // Opening the view lands in the message box, so typing a
                // message never stages or discards from the list (#871).
                self.source_control.list_focused = false;
                self.focus_pane(Pane::Tree);
            }
            SidebarView::Remote => self.focus_pane(Pane::Tree),
            SidebarView::RunDebug => {
                self.refresh_run_debug();
                self.focus_pane(Pane::Tree);
            }
            SidebarView::Extensions => {
                self.refresh_extensions();
                self.focus_pane(Pane::Tree);
            }
            SidebarView::Testing => self.focus_pane(Pane::Tree),
            SidebarView::CodeQL => {
                self.codeql.theme = self.theme;
                self.focus_pane(Pane::Tree);
            }
        }
    }

    /// A click on an activity-bar view icon (#1845), with VS Code's default
    /// `workbench.activityBar.iconClickBehavior` of `toggle`: the icon of the
    /// view the side bar is already showing collapses it, and any other click
    /// opens that view, expanding a collapsed side bar. The collapse goes
    /// through `toggle_side_bar` like every other deliberate hide, then hands
    /// focus on, as VS Code does, so keys never land in a pane no longer on
    /// screen: to the editor, or to a maximized terminal, which the editor
    /// would otherwise un-maximize by taking focus.
    pub(super) fn click_activity_view(&mut self, view: SidebarView) {
        if self.activity_view_active(view) {
            self.toggle_side_bar();
            self.after_chrome_visibility_change();
            if self.focus == Pane::Tree {
                self.focus_pane(if self.terminal_maximized {
                    Pane::Terminal
                } else {
                    Pane::Editor
                });
            }
            return;
        }
        match view {
            SidebarView::Testing => self.open_testing_view(),
            SidebarView::CodeQL => self.open_codeql_view(),
            _ => self.set_sidebar_view(view),
        }
    }

    /// Whether `view`'s activity icon reads as selected: it is the side bar's
    /// view and the side bar is open. A collapsed side bar selects nothing,
    /// as in VS Code, so the pill never claims a view that is not on screen.
    pub(super) fn activity_view_active(&self, view: SidebarView) -> bool {
        self.show_tree && self.sidebar_view == view
    }

    /// Run `f` with auto-hide suppressed, for focus moves the user did not
    /// ask for. `focus_pane` cannot tell intent apart on its own: an async
    /// MCP result, a launch-time file open, and Enter on a sidebar row all
    /// look identical to it, and collapsing on those is the flapping #260
    /// set out to prevent.
    pub(super) fn without_auto_hide<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = std::mem::replace(&mut self.sidebar_auto_hide_suspended, true);
        let out = f(self);
        self.sidebar_auto_hide_suspended = prev;
        out
    }

    /// Whether auto-hide may collapse the sidebar RIGHT NOW. Every reason to
    /// hold it open lives here rather than being scattered through the call
    /// sites, so the interaction rules are one readable list and a new
    /// suppression case has exactly one place to go.
    /// Whether a STRUCTURAL suppression is in force: one that no passage of
    /// time lifts, only a deliberate chrome change by the user.
    ///
    /// Split out from [`Self::sidebar_auto_hide_allowed`] because the tick has
    /// to treat the two kinds differently and the combined predicate cannot
    /// say which it declined for. A transient suppression (seam drag, open
    /// palette, modal) ends in moments, so the dwell stays armed and retries.
    /// A structural one can last the session, and a dwell left armed across it
    /// fires on the frame it lifts — a collapse for a focus move the user made
    /// an hour ago. Cancelling at the ARM site instead would make the retry
    /// unreachable, which is the recovery path for the transient case.
    pub(super) fn sidebar_structurally_suppressed(&self) -> bool {
        !self.activity_bar_visible || self.zen_mode
    }

    /// Whether auto-hide may collapse the sidebar RIGHT NOW. Every reason to
    /// hold it open lives here rather than being scattered through the call
    /// sites, so the interaction rules are one readable list and a new
    /// suppression case has exactly one place to go.
    pub(super) fn sidebar_auto_hide_allowed(&self) -> bool {
        if !self.sidebar_auto_hide || !self.show_tree {
            return false;
        }
        // A focus move nobody asked for is not a reason to hide chrome.
        if self.sidebar_auto_hide_suspended {
            return false;
        }
        // NOTE: the one-shot reveal pin is deliberately NOT checked here. It is
        // the caller's concern (`tick_sidebar_auto_hide_at`), because this
        // predicate must stay answerable while a pin is banked: the tick asks
        // "would this collapse otherwise fire?" precisely so it can decide
        // whether spending the pin is warranted. Checking the pin here made
        // that question unanswerable and the pin unspendable.
        // Mid-drag on the sidebar seam: the pointer is over the editor by
        // definition, and collapsing under the cursor is the flapping the
        // issue calls out.
        if self.splitter_drag.is_some() {
            return false;
        }
        // Anything modal may be anchored over sidebar space, and several are
        // sidebar-targeting flows (branch picker, connect dialog) that are
        // their own fields rather than `Prompt`s — checking only `prompt`
        // would have missed exactly those.
        if self.prompt.is_some()
            || self.context_menu.is_some()
            || self.connect_dialog.is_some()
            || self.modal_overlay_open()
        {
            return false;
        }
        // An Explorer drag is a drag over the sidebar the user is dragging
        // FROM; `splitter_drag` does not cover it.
        if self.tree_drag.is_some() {
            return false;
        }
        // With no activity bar there is no on-screen way back to a collapsed
        // sidebar (hover-to-reveal is not implemented yet), so a mouse-only
        // user would be stranded.
        if !self.activity_bar_visible {
            return false;
        }
        // Zen Mode already owns chrome visibility; it wins, as it does today.
        if self.zen_mode {
            return false;
        }
        true
    }

    /// Flip auto-hide and persist it. Both routes (palette command and the
    /// Customize Layout row) funnel here so they cannot drift in what they
    /// set, what they report, or whether the choice survives a restart.
    pub(super) fn toggle_sidebar_auto_hide(&mut self) {
        self.sidebar_auto_hide = !self.sidebar_auto_hide;
        // Turning it ON with the sidebar open takes effect at the next focus
        // move rather than retroactively yanking it away here — which means
        // dropping any collapse already pending. A dwell armed before the
        // setting was touched belongs to the old state: leaving it armed lets
        // an idle tick collapse the sidebar after the user turns the feature
        // off and on again, with no focus move of their own in between.
        //
        // The pin goes for the same reason, and the fourth boundary to say so.
        // `toggle_side_bar` refuses to bank one while the feature is off,
        // because "a pin set while the feature is off would sit unconsumed and
        // silently eat the first collapse after they turn it on" — and a pin
        // banked BEFORE the user disables auto-hide reaches that same state by
        // the other route. Main cleared it incidentally by taking the pin on
        // every focus move; consuming at fire time is correct but leaves each
        // boundary to say so.
        self.sidebar_dwell.disarm();
        self.sidebar_pinned_open = false;
        self.status = format!(
            "Auto-hide side bar {}",
            if self.sidebar_auto_hide { "on" } else { "off" }
        );
        // A read-only config must not break the in-session toggle; a failed
        // write is only said. Skipped under test, mirroring the write-skip
        // in `apply_theme` — `save_*` resolves the developer's real
        // ~/.config/croft/config.json, and a test has no business rewriting
        // it (nor leaving the next test to read what it wrote).
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_sidebar_auto_hide(
                self.sidebar_auto_hide,
            ));
        }
    }

    /// Arm the grace delay before collapsing. Called on focus moves INTO the
    /// editor or a terminal.
    ///
    /// #294 collapsed right here, which meant a click *passing through* the
    /// panel on its way to the editor took the panel out from under the
    /// pointer. The collapse now waits `AUTO_HIDE_DWELL` (#302) and fires from
    /// `tick_sidebar_auto_hide`, so a click that lands back on the sidebar
    /// inside the window cancels it.
    ///
    /// Note what is deliberately NOT done here: the one-shot pin is not
    /// consumed. Arming is no longer the same event as collapsing, and eating
    /// the exemption on an attempt the dwell then cancels would silently strip
    /// a deliberate reveal of the protection it was granted. It is consumed
    /// where the collapse actually happens.
    pub(super) fn maybe_auto_hide_sidebar(&mut self) {
        // Cheap pre-check only. The real decision is re-made when the timer
        // fires, because every suppression here can change during the window.
        //
        // `sidebar_auto_hide_suspended` is the exception and MUST be read here:
        // it is scoped to a `without_auto_hide` closure and restored the moment
        // that closure returns, so by tick time it is always false. Deferring
        // it would silently un-suppress every programmatic focus move — the
        // async MCP result, the launch-time file open, Enter on a sidebar row —
        // which is the #260 flapping this flag exists to prevent. Arming is
        // what has to be refused; there is nothing left to consult later.
        //
        // The STRUCTURAL suppressions (Zen Mode, hidden activity bar) are NOT
        // checked here, and that is deliberate. Refusing to arm under them
        // looks right and strands the feature: the tick stays armed through a
        // decline and fires once the suppression lifts, so that retry IS the
        // recovery path, and nothing that fails to arm can ever use it. Zen
        // survives such a refusal only because Zen exit restores the pre-Zen
        // snapshot and closes the sidebar, forcing a fresh reveal; a hidden
        // activity bar restores nothing, focus is already on the editor, and
        // production will not call `focus_pane` again for a pane that already
        // has focus — so auto-hide would stay dead for the rest of the session
        // with nothing on screen to explain it.
        //
        // The structural/transient distinction is drawn at FIRE time instead,
        // where the state is actually known and no future suppression needs its
        // own boundary hook. See `tick_sidebar_auto_hide_at`.
        if self.sidebar_auto_hide && self.show_tree && !self.sidebar_auto_hide_suspended {
            self.sidebar_dwell.arm(std::time::Instant::now());
        }
    }

    /// Fire a pending auto-hide collapse once its grace delay elapses.
    ///
    /// Returns `true` when the sidebar collapsed, so the frame redraws.
    pub fn tick_sidebar_auto_hide(&mut self) -> bool {
        self.tick_sidebar_auto_hide_at(std::time::Instant::now())
    }

    /// `tick_sidebar_auto_hide` with the clock injected, so the grace delay is
    /// testable without sleeping — the same posture as `HoverDwell::due`.
    pub(crate) fn tick_sidebar_auto_hide_at(&mut self, now: std::time::Instant) -> bool {
        if !self.sidebar_dwell.armed() {
            return false;
        }
        // Focus came back to the sidebar, or it is already closed: the pending
        // collapse is moot. Leave the pin alone — it was never spent.
        if self.focus == Pane::Tree || !self.show_tree {
            self.sidebar_dwell.disarm();
            return false;
        }
        if !self.sidebar_dwell.due(now, AUTO_HIDE_DWELL) {
            return false;
        }
        // The window has elapsed and focus stayed away. Before spending
        // anything, ask whether a collapse would fire at all: a tick stopped by
        // a seam drag, an open palette or a modal is not the collapse the pin
        // was banked for, and burning it there loses an exemption the user
        // never saw honoured. Every earlier decline already leaves the pin
        // alone; this makes the late one behave the same way.
        //
        // Stay ARMED on that decline, and disarm only once a collapse is
        // actually due. These suppressions are transient — a drag ends, a
        // palette closes — and nothing re-arms the dwell but a fresh focus
        // move into the editor or terminal. Disarming here would drop the
        // pending collapse for good, so a user whose palette happened to be
        // open during the grace window would keep a sidebar that never
        // auto-hides again. Retrying costs nothing: `main_loop` ticks on an
        // 8ms cadence, so the collapse lands as soon as the suppression lifts.
        // A STRUCTURAL suppression DEFERS and re-stamps. Cancelling here was
        // the round-two defect: `main_loop` ticks every 8ms unconditionally, so
        // a cancel-on-suppressed-tick fires 8ms after arming and is
        // indistinguishable from refusing to arm at all — which strands the
        // feature, because nothing re-arms once a hidden activity bar returns
        // (focus never moved, and production will not re-focus a pane that
        // already has focus).
        //
        // Re-stamping is what makes deferral safe. The complaint against
        // deferral was that Zen can last an hour and the collapse would land on
        // the frame it exits, for a focus move the user has long forgotten.
        // Anchoring to the moment the suppression LIFTS answers that exactly:
        // the user then gets the same AUTO_HIDE_DWELL grace they would get from
        // a fresh focus move, measured from a moment they can actually perceive
        // — the chrome coming back.
        if self.sidebar_structurally_suppressed() {
            self.sidebar_dwell.restamp(now);
            return false;
        }
        if !self.sidebar_auto_hide_allowed() {
            return false;
        }
        self.sidebar_dwell.disarm();
        // A real collapse is now due, so this IS the moment the one-shot
        // exemption is spent — whether or not it is what stops us.
        //
        // Semantics: the pin is spent by the next collapse that would genuinely
        // have fired, not by the next tick — so a tick stopped by a TRANSIENT
        // suppression (seam drag, palette, modal) spends nothing and the pin
        // survives to do its job a moment later.
        //
        // Structural suppressions are decided HERE, above, not at the sites
        // that enter them. Entry hooks cannot see the case where the dwell is
        // armed while the suppression is ALREADY in force — Cmd+B works inside
        // Zen, so no transition occurs and no hook fires. This tick is asked to
        // distinguish a suppression that lasts a moment from one that lasts the
        // session precisely because it is the one place that always runs.
        let exempt = std::mem::take(&mut self.sidebar_pinned_open);
        if !exempt {
            self.show_tree = false;
            return true;
        }
        false
    }

    /// Zen Mode hides the status bar, so for a few seconds after entering
    /// it a box at the bottom says how to leave (#1585).
    pub(super) fn render_zen_hint(&self, frame: &mut ratatui::Frame) {
        if !self.zen_mode
            || !self
                .zen_hint_until
                .is_some_and(|until| std::time::Instant::now() < until)
        {
            return;
        }
        let text = zen_hint_text();
        let area = frame.area();
        let width = text.chars().count() as u16 + 4;
        let height: u16 = 3;
        if area.width < width || area.height < height + 1 {
            return;
        }
        let rect = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + area.height - height - 1,
            width,
            height,
        };
        let accent = self.theme.accent();
        // A `surfaces` theme floats the hint as a raised card in body text,
        // the accent only on its edge, rather than accent text in an accent
        // box on the canvas.
        let tones = crate::widgets::surface::tones(self.theme);
        let (bg, fg) = match tones {
            Some(t) => (t.raised(), t.text()),
            None => (self.theme.editor_bg(), accent),
        };
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(accent))
            .style(Style::default().bg(bg));
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(
            ratatui::widgets::Paragraph::new(ratatui::text::Span::styled(
                text,
                Style::default().fg(fg),
            ))
            .alignment(ratatui::layout::Alignment::Center)
            .block(block),
            rect,
        );
        if tones.is_some() {
            crate::widgets::surface::paint_overlay_frame(frame.buffer_mut(), rect, self.theme);
        }
    }

    /// Resize the sidebar / terminal pane while a splitter drag is in
    /// progress. The pointer's screen coordinate maps directly to the new
    /// edge: dragging horizontally sets the sidebar width to (column −
    /// activity-bar width); dragging vertically sets the terminal height
    /// to (right-pane bottom − row).
    pub(super) fn handle_splitter_drag(&mut self, kind: SplitterDrag, column: u16, row: u16) {
        match kind {
            SplitterDrag::Sidebar => {
                let activity_w = if self.activity_bar_visible {
                    ACTIVITY_BAR_WIDTH
                } else {
                    0
                };
                let secondary_w = if self.secondary_side_bar_visible {
                    SECONDARY_SIDEBAR_WIDTH
                } else {
                    0
                };
                // Reconstruct the content width from the live regions rather
                // than the frame, so the drag is correct even before a real
                // render has stamped `last_frame_area`.
                let frame_x = self.last_frame_area.x;
                let total = activity_w + secondary_w + self.sidebar_width + self.last_content_width;
                let new_w = match self.side_bar_position {
                    // Left: sidebar starts after the activity bar; the seam is
                    // its right edge, so width = column - activity offset.
                    SideBarPosition::Left => {
                        column.saturating_sub(frame_x).saturating_sub(activity_w)
                    }
                    // Right: sidebar ends at the content's right edge (minus the
                    // activity bar); dragging the seam left grows it.
                    SideBarPosition::Right => (frame_x + total)
                        .saturating_sub(activity_w)
                        .saturating_sub(column),
                };
                let max_sidebar = total
                    .saturating_sub(activity_w)
                    .saturating_sub(secondary_w)
                    .saturating_sub(RIGHT_PANE_MIN);
                self.sidebar_width =
                    new_w.clamp(SIDEBAR_WIDTH_MIN, max_sidebar.max(SIDEBAR_WIDTH_MIN));
            }
            SplitterDrag::Terminal => {
                let Some(splitter_y) = self.terminal_splitter_y else {
                    return;
                };
                // Right pane spans [splitter_y - editor_h, splitter_y +
                // current_terminal_h). Compute the right-pane bottom from
                // the captured height so a drag past it just clamps.
                let bottom = splitter_y + self.terminal_height.unwrap_or(0);
                let actual_bottom = if bottom == splitter_y {
                    // Pre-drag we may have used a percent split; fall back
                    // to the captured content height.
                    splitter_y.saturating_add(self.last_content_height)
                } else {
                    bottom
                };
                let new_h = actual_bottom.saturating_sub(row);
                let max_h = self
                    .last_content_height
                    .saturating_sub(EDITOR_HEIGHT_MIN)
                    .max(TERMINAL_HEIGHT_MIN);
                self.terminal_height = Some(new_h.clamp(TERMINAL_HEIGHT_MIN, max_h));
            }
            SplitterDrag::EditorSeam(seam) => {
                if !self.editor_layout.is_split() {
                    return;
                }
                // Map the pointer onto the seam's axis (column for a vertical
                // seam line, row for a horizontal one) and repartition only the
                // two groups it separates. The seam carries no dedicated cell,
                // so the two groups tile the boundary exactly.
                let pointer = match seam.dir {
                    editor_layout::SplitDir::Horizontal => column,
                    editor_layout::SplitDir::Vertical => row,
                };
                self.editor_layout
                    .drag_seam(&seam, pointer, EDITOR_SPLIT_MIN);
            }
        }
    }
}

/// The tail of a recorded frame (#356) that leaves a player's cursor where
/// the pane's was: moved to its `(row, col)` and shown, or hidden when the
/// program hid it. Always one or the other, so a frame never inherits the
/// previous frame's cursor state.
pub(super) fn cursor_suffix(cursor: Option<(u16, u16)>) -> String {
    match cursor {
        Some((row, col)) => format!("\u{1b}[{};{}H\u{1b}[?25h", row + 1, col + 1),
        None => String::from("\u{1b}[?25l"),
    }
}
