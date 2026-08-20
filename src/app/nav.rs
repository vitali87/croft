use std::path::PathBuf;

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavLoc {
    pub path: PathBuf,
    pub row: usize,
    pub col: usize,
}

/// Jump history for Go Back / Go Forward (VS Code's navigation stack).
/// `record` pushes the position a jump left from and forks history (the
/// forward trail dies); `back`/`forward` walk the trail, threading the
/// CURRENT position onto the opposite stack so the walk is reversible.
#[derive(Debug, Default)]
pub struct NavHistory {
    back: Vec<NavLoc>,
    forward: Vec<NavLoc>,
}

impl NavHistory {
    pub fn record(&mut self, loc: NavLoc) {
        if self.back.last() == Some(&loc) {
            return;
        }
        self.back.push(loc);
        if self.back.len() > MAX_DEPTH {
            self.back.remove(0);
        }
        // A fresh jump forks history: the old forward trail no longer
        // describes where the user came from (VS Code drops it too).
        self.forward.clear();
    }

    /// Step back, remembering `current` (where the user is now) so Go
    /// Forward can return there. `current` is None on surfaces with no
    /// file position (the welcome screen); the step still happens.
    pub fn back(&mut self, current: Option<NavLoc>) -> Option<NavLoc> {
        let loc = self.back.pop()?;
        if let Some(cur) = current
            && self.forward.last() != Some(&cur)
        {
            self.forward.push(cur);
            if self.forward.len() > MAX_DEPTH {
                self.forward.remove(0);
            }
        }
        Some(loc)
    }

    /// Step forward after a Go Back, mirroring [`Self::back`].
    pub fn forward(&mut self, current: Option<NavLoc>) -> Option<NavLoc> {
        let loc = self.forward.pop()?;
        if let Some(cur) = current
            && self.back.last() != Some(&cur)
        {
            self.back.push(cur);
            if self.back.len() > MAX_DEPTH {
                self.back.remove(0);
            }
        }
        Some(loc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(name: &str, row: usize, col: usize) -> NavLoc {
        NavLoc {
            path: PathBuf::from(name),
            row,
            col,
        }
    }

    #[test]
    fn back_pops_locations_in_reverse_order() {
        let mut h = NavHistory::default();
        h.record(loc("a.rs", 1, 2));
        h.record(loc("b.rs", 3, 4));
        assert_eq!(h.back(None), Some(loc("b.rs", 3, 4)));
        assert_eq!(h.back(None), Some(loc("a.rs", 1, 2)));
        assert_eq!(h.back(None), None);
    }

    #[test]
    fn back_is_none_when_empty() {
        let mut h = NavHistory::default();
        assert_eq!(h.back(None), None);
    }

    #[test]
    fn record_skips_consecutive_duplicates() {
        let mut h = NavHistory::default();
        h.record(loc("a.rs", 1, 2));
        h.record(loc("a.rs", 1, 2));
        assert_eq!(h.back(None), Some(loc("a.rs", 1, 2)));
        assert_eq!(
            h.back(None),
            None,
            "a location identical to the top of the stack must not be recorded twice"
        );
    }

    #[test]
    fn record_caps_at_max_depth_dropping_the_oldest() {
        let mut h = NavHistory::default();
        for i in 0..(MAX_DEPTH + 10) {
            h.record(loc("f.rs", i, 0));
        }
        let mut count = 0;
        let mut last = None;
        while let Some(l) = h.back(None) {
            count += 1;
            last = Some(l);
        }
        assert_eq!(count, MAX_DEPTH, "history is bounded at MAX_DEPTH entries");
        assert_eq!(
            last,
            Some(loc("f.rs", 10, 0)),
            "the oldest entries are dropped once the cap is exceeded, never the newest"
        );
    }

    /// Issue #210: Go Forward. Back threads the current position onto the
    /// forward stack, so forward returns exactly there, and the walk is
    /// symmetric in both directions.
    #[test]
    fn back_then_forward_round_trips_through_the_current_position() {
        let mut h = NavHistory::default();
        h.record(loc("a.rs", 1, 0)); // jumped away from a.rs:1
        // Now at b.rs:9. Going back lands on a.rs:1 and remembers b.rs:9.
        assert_eq!(h.back(Some(loc("b.rs", 9, 3))), Some(loc("a.rs", 1, 0)));
        // Going forward from a.rs:1 returns to b.rs:9 and re-records a.rs:1.
        assert_eq!(h.forward(Some(loc("a.rs", 1, 0))), Some(loc("b.rs", 9, 3)));
        assert_eq!(h.forward(None), None, "the forward trail is spent");
        assert_eq!(
            h.back(None),
            Some(loc("a.rs", 1, 0)),
            "back still works after the round trip"
        );
    }

    #[test]
    fn a_fresh_jump_clears_the_forward_trail() {
        let mut h = NavHistory::default();
        h.record(loc("a.rs", 1, 0));
        assert_eq!(h.back(Some(loc("b.rs", 9, 0))), Some(loc("a.rs", 1, 0)));
        // A new jump from here forks history: forward must die.
        h.record(loc("a.rs", 1, 0));
        assert_eq!(h.forward(None), None, "record forks history");
    }
}
