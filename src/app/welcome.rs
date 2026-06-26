use crate::release_notes::{RELEASE_NOTES, ReleaseNote};

/// Backing state for the welcome screen's "IN THIS RELEASE" panel. The
/// highlights are hand-curated data baked into the binary
/// ([`crate::release_notes::RELEASE_NOTES`]) — no network, no git log, no
/// background thread. The list is an accurate property of the build, so every
/// launch is free and the panel reflects exactly what this version ships.
pub struct WelcomeState {
    notes: Vec<ReleaseNote>,
}

impl WelcomeState {
    pub fn new() -> Self {
        Self {
            notes: RELEASE_NOTES.to_vec(),
        }
    }

    pub fn notes(&self) -> &[ReleaseNote] {
        &self.notes
    }

    pub fn has_recent_panel(&self) -> bool {
        !self.notes.is_empty()
    }

    #[cfg(test)]
    pub fn set_test_notes(&mut self, notes: Vec<ReleaseNote>) {
        self.notes = notes;
    }
}
