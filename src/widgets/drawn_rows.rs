//! The rows a list drew on its last frame, for the clicks read before the
//! next one (#2751).
//!
//! croft reads every input queued since the last frame before it draws the
//! next. A wheel step, a key or a click read ahead of a click may already
//! have scrolled the list, opened a folder above the pointer or narrowed a
//! filter, while the screen still shows the last frame. The click means the
//! row drawn on its line: a list notes each drawn row's place and key as it
//! paints, and a click follows that row to wherever it is now, or meets
//! nothing when it is gone.

/// One frame's drawn rows: per screen line from the list's top, the row's
/// index in the list as drawn and the key that names it.
#[derive(Debug, Clone)]
pub struct DrawnRows<K> {
    rows: Vec<(usize, K)>,
}

impl<K> Default for DrawnRows<K> {
    fn default() -> Self {
        Self { rows: Vec::new() }
    }
}

impl<K> DrawnRows<K> {
    /// Forget the last frame's rows, as a frame starts.
    pub fn clear(&mut self) {
        self.rows.clear();
    }

    /// Note the row drawn on the next line down.
    pub fn push(&mut self, at: usize, key: K) {
        self.rows.push((at, key));
    }

    /// Note a frame's rows, top line first.
    pub fn record(&mut self, rows: impl IntoIterator<Item = (usize, K)>) {
        self.rows.clear();
        self.rows.extend(rows);
    }

    /// The indices the rows were drawn at, top line first.
    #[cfg(test)]
    pub fn drawn_at(&self) -> impl Iterator<Item = usize> + '_ {
        self.rows.iter().map(|&(at, _)| at)
    }

    /// The index now of the row drawn on `line`. `holds(i, key)` says whether
    /// index `i`, below `len`, holds the row named `key`.
    pub fn now(&self, line: usize, len: usize, holds: impl Fn(usize, &K) -> bool) -> Option<usize> {
        let (at, key) = self.rows.get(line)?;
        follow(*at, key, len, holds)
    }
}

/// Where the row drawn at index `at` and named `key` is now: still at `at`
/// when nothing above it moved, else the first index that holds it, else
/// none, as when a filter dropped it or its folder closed.
pub fn follow<K>(
    at: usize,
    key: &K,
    len: usize,
    holds: impl Fn(usize, &K) -> bool,
) -> Option<usize> {
    if at < len && holds(at, key) {
        return Some(at);
    }
    (0..len).find(|&i| holds(i, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(rows: &[(usize, &'static str)]) -> DrawnRows<&'static str> {
        let mut d = DrawnRows::default();
        d.record(rows.iter().copied());
        d
    }

    #[test]
    fn a_row_still_in_its_place_is_met_there() {
        let list = ["a", "b", "c"];
        let d = drawn(&[(1, "b"), (2, "c")]);
        assert_eq!(d.now(0, list.len(), |i, k| list[i] == *k), Some(1));
    }

    #[test]
    fn a_row_that_moved_is_followed() {
        let list = ["x", "y", "a", "b", "c"];
        let d = drawn(&[(1, "b"), (2, "c")]);
        assert_eq!(d.now(0, list.len(), |i, k| list[i] == *k), Some(3));
    }

    #[test]
    fn a_row_that_is_gone_or_a_line_past_the_rows_meets_nothing() {
        let list = ["a", "c"];
        let d = drawn(&[(1, "b"), (2, "c")]);
        assert_eq!(d.now(0, list.len(), |i, k| list[i] == *k), None);
        assert_eq!(d.now(2, list.len(), |i, k| list[i] == *k), None);
    }
}
