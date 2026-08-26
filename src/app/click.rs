use std::time::{Duration, Instant};

const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);

#[derive(Default)]
pub struct ClickTracker {
    last: Option<(Instant, u16, u16)>,
}

impl ClickTracker {
    pub fn is_double(&self, now: Instant, col: u16, row: u16) -> bool {
        matches!(
            self.last,
            Some((t, x, y))
                if row == y
                    && col.abs_diff(x) <= 1
                    && now.duration_since(t) <= DOUBLE_CLICK_WINDOW
        )
    }

    pub fn record(&mut self, now: Instant, col: u16, row: u16) {
        self.last = Some((now, col, row));
    }

    pub fn clear(&mut self) {
        self.last = None;
    }

    pub fn clear_if_moved(&mut self, col: u16, row: u16) {
        if let Some((_, x, y)) = self.last
            && (col != x || row != y)
        {
            self.last = None;
        }
    }
}

/// Double-click tracking for MODIFIED clicks, kept apart from [`ClickTracker`].
///
/// A modified click must never be recorded into the built-in trackers: those
/// are read by the editor's plain select-word (and the tree's open-file), none
/// of which consult modifiers, so arming them with a ctrl+click would make the
/// user's next ORDINARY click select a word they never asked for.
///
/// But `double_click` is classified by asking a tracker whether it just saw a
/// click at this cell — so with nothing recording modified clicks, a
/// `ctrl+double_click` binding could never fire at all. This is that missing
/// record, in a tracker only the binding dispatch reads.
///
/// The modifier set is part of the match: ctrl+click followed by alt+click is
/// two different gestures, not a double of either.
#[derive(Default)]
pub struct ModifiedClickTracker {
    last: Option<(Instant, u16, u16, crossterm::event::KeyModifiers)>,
}

impl ModifiedClickTracker {
    pub fn is_double(
        &self,
        now: Instant,
        col: u16,
        row: u16,
        mods: crossterm::event::KeyModifiers,
    ) -> bool {
        matches!(
            self.last,
            Some((t, x, y, m))
                if m == mods
                    && row == y
                    && col.abs_diff(x) <= 1
                    && now.duration_since(t) <= DOUBLE_CLICK_WINDOW
        )
    }

    pub fn record(
        &mut self,
        now: Instant,
        col: u16,
        row: u16,
        mods: crossterm::event::KeyModifiers,
    ) {
        self.last = Some((now, col, row, mods));
    }

    pub fn clear(&mut self) {
        self.last = None;
    }
}
