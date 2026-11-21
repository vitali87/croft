//! What a click lands on: the header pills, the branch name, a row's stage, unstage and
//! discard icons, the Commit and Initialize buttons, and a nested repository's Open button.

use super::*;

impl SourceControlPanel {
    /// True when (x, y) lands on the "⋯" header pill, so the App can open
    /// the Source Control actions menu.
    pub fn click_more(&self, x: u16, y: u16) -> bool {
        rect_hit(self.header_more_btn, x, y)
    }

    /// Returns the entry index when (x, y) lands inside the Discard icon
    /// of the currently-selected row. The widget only paints this icon on
    /// the selected unstaged row, so a hit implies that row is the action
    /// target.
    pub fn click_discard_action(&self, x: u16, y: u16) -> Option<usize> {
        let a = self.last_row_actions?;
        rect_hit(a.discard, x, y).then_some(a.entry_idx)
    }

    /// Returns the entry index when (x, y) lands inside the Stage icon of
    /// the currently-selected row. Mirrors `click_discard_action`.
    pub fn click_stage_action(&self, x: u16, y: u16) -> Option<usize> {
        let a = self.last_row_actions?;
        rect_hit(a.stage, x, y).then_some(a.entry_idx)
    }

    /// Returns the entry index when (x, y) lands inside the Unstage icon
    /// of the currently-selected *staged* row. The widget only paints this
    /// icon on a selected staged row, so a hit implies that row.
    pub fn click_unstage_action(&self, x: u16, y: u16) -> Option<usize> {
        let a = self.last_row_actions?;
        rect_hit(a.unstage, x, y).then_some(a.entry_idx)
    }

    /// True when (x, y) lands on the branch name, so the App can open the
    /// Checkout / Create Branch picker.
    pub fn click_branch(&self, x: u16, y: u16) -> bool {
        rect_hit(self.last_branch_area, x, y)
    }

    /// True when (x, y) lands on the header refresh pill, so the App can
    /// force an immediate git re-scan.
    pub fn click_header_refresh(&self, x: u16, y: u16) -> bool {
        rect_hit(self.header_refresh_btn, x, y)
    }

    pub fn click_button(&self, x: u16, y: u16) -> bool {
        rect_hit(self.last_button_area, x, y)
    }

    pub fn click_init_repo_button(&self, x: u16, y: u16) -> bool {
        rect_hit(self.last_init_repo_button_area, x, y)
    }

    /// The subfolder repo whose Open button (x, y) lands on (#1227).
    pub fn click_nested_repo(&self, x: u16, y: u16) -> Option<std::path::PathBuf> {
        self.nested_repo_button_areas
            .iter()
            .find(|(r, _)| rect_hit(*r, x, y))
            .map(|(_, path)| path.clone())
    }
}

/// Paint a chunky 3-row solid-bg button at `area`, with rounded corner
/// glyphs that let the panel bg show through the four corner notches so
/// the button reads as a rounded rectangle (matching the VS Code mockup).
pub(super) fn rect_hit(rect: Rect, x: u16, y: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && x >= rect.x
        && x < rect.x + rect.width
        && y >= rect.y
        && y < rect.y + rect.height
}
