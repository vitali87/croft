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
        if let Some((_, x, y)) = self.last {
            if col != x || row != y {
                self.last = None;
            }
        }
    }
}
