use std::path::PathBuf;

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavLoc {
    pub path: PathBuf,
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Default)]
pub struct NavHistory {
    stack: Vec<NavLoc>,
}

impl NavHistory {
    pub fn record(&mut self, loc: NavLoc) {
        if self.stack.last() == Some(&loc) {
            return;
        }
        self.stack.push(loc);
        if self.stack.len() > MAX_DEPTH {
            self.stack.remove(0);
        }
    }

    pub fn back(&mut self) -> Option<NavLoc> {
        self.stack.pop()
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
        assert_eq!(h.back(), Some(loc("b.rs", 3, 4)));
        assert_eq!(h.back(), Some(loc("a.rs", 1, 2)));
        assert_eq!(h.back(), None);
    }

    #[test]
    fn back_is_none_when_empty() {
        let mut h = NavHistory::default();
        assert_eq!(h.back(), None);
    }

    #[test]
    fn record_skips_consecutive_duplicates() {
        let mut h = NavHistory::default();
        h.record(loc("a.rs", 1, 2));
        h.record(loc("a.rs", 1, 2));
        assert_eq!(h.back(), Some(loc("a.rs", 1, 2)));
        assert_eq!(
            h.back(),
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
        while let Some(l) = h.back() {
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
}
