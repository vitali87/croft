use crate::release_notes::{ReleaseNote, release_notes};
use ratatui::layout::Rect;

/// An action on the welcome screen's Start list (Croft Loch): a palette
/// command run as if picked from the palette, or the palette itself, which
/// is a key rather than a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartAction {
    Command(crate::widgets::command_palette::Command),
    CommandPalette,
}

/// Backing state for the welcome screen's "IN THIS RELEASE" panel. The
/// highlights are hand-curated data baked into the binary
/// ([`crate::release_notes::release_notes`]) - no network, no git log, no
/// background thread. The list is an accurate property of the build, so every
/// launch is free and the panel reflects exactly what this version ships.
pub struct WelcomeState {
    notes: Vec<ReleaseNote>,
    /// The Start list's rows painted on the last welcome frame, with the
    /// action each runs. Cleared at the start of every welcome render, so a
    /// click resolves against what is on screen (#103's invariant).
    start_rows: Vec<(Rect, StartAction)>,
}

impl WelcomeState {
    pub fn new() -> Self {
        Self {
            notes: release_notes().to_vec(),
            start_rows: Vec::new(),
        }
    }

    pub fn notes(&self) -> &[ReleaseNote] {
        &self.notes
    }

    pub fn has_recent_panel(&self) -> bool {
        !self.notes.is_empty()
    }

    /// Forget the Start rows, before a frame paints them again.
    pub fn clear_start_rows(&mut self) {
        self.start_rows.clear();
    }

    /// Record a painted Start row and the action a click on it runs.
    pub fn push_start_row(&mut self, rect: Rect, action: StartAction) {
        self.start_rows.push((rect, action));
    }

    /// The Start rows painted on the last frame, top to bottom.
    #[cfg(test)]
    pub fn start_rows(&self) -> &[(Rect, StartAction)] {
        &self.start_rows
    }

    /// The Start action under the cell `(x, y)`, if a row is painted there.
    pub fn start_action_at(&self, x: u16, y: u16) -> Option<StartAction> {
        self.start_rows
            .iter()
            .find(|(r, _)| x >= r.x && x < r.x.saturating_add(r.width) && y == r.y)
            .map(|&(_, action)| action)
    }

    #[cfg(test)]
    pub fn set_test_notes(&mut self, notes: Vec<ReleaseNote>) {
        self.notes = notes;
    }
}
