use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffData {
    pub left_path: PathBuf,
    pub right_path: PathBuf,
    pub left_lines: Vec<String>,
    pub right_lines: Vec<String>,
    pub rows: Vec<DiffRow>,
    /// Top-most row index visible in the viewport. Mirrors `Editor.scroll_y`
    /// for non-diff tabs.
    pub scroll: usize,
    /// Number of leading characters to skip on each rendered row so the
    /// user can pan horizontally past long lines. Same value applies to
    /// both columns so the side-by-side rows stay aligned.
    pub scroll_x: usize,
}

/// One visual row in a side-by-side diff view. The left column shows
/// `left_lines[left]` (or blank when `Added`) and the right column shows
/// `right_lines[right]` (or blank when `Removed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffRow {
    Equal { left: usize, right: usize },
    Removed { left: usize },
    Added { right: usize },
    /// A line that was replaced: both columns paint, left in `left_lines`
    /// and right in `right_lines`. Produced by pairing consecutive
    /// Delete + Insert hunks so the visual rows align without forcing a
    /// pure-add or pure-remove zigzag.
    Replaced { left: usize, right: usize },
}

impl DiffData {
    pub fn build(
        left_path: PathBuf,
        right_path: PathBuf,
        left_lines: Vec<String>,
        right_lines: Vec<String>,
    ) -> Self {
        let rows = build_diff_rows(&left_lines, &right_lines);
        Self {
            left_path,
            right_path,
            left_lines,
            right_lines,
            rows,
            scroll: 0,
            scroll_x: 0,
        }
    }

    /// Length (in chars) of the longest line across both files. Used to
    /// clamp horizontal scrolling so the user can't pan into empty space
    /// past the end of the longest content.
    pub fn longest_line_chars(&self) -> usize {
        self.left_lines
            .iter()
            .chain(self.right_lines.iter())
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0)
    }

    pub fn total_rows(&self) -> usize {
        self.rows.len()
    }

    pub fn scroll_up_by(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    pub fn scroll_down_by(&mut self, n: usize) {
        let max = self.rows.len();
        // Clamp loosely here; render_diff also re-clamps against the live
        // viewport, so we don't need to know it at scroll time.
        self.scroll = (self.scroll + n).min(max);
    }

    pub fn scroll_home(&mut self) {
        self.scroll = 0;
    }

    pub fn scroll_end(&mut self) {
        self.scroll = self.rows.len();
    }

    pub fn scroll_left_by(&mut self, n: usize) {
        self.scroll_x = self.scroll_x.saturating_sub(n);
    }

    pub fn scroll_right_by(&mut self, n: usize) {
        let max = self.longest_line_chars();
        self.scroll_x = (self.scroll_x + n).min(max);
    }
}

/// Run a line-level diff over `left` vs `right` and emit one DiffRow per
/// visual row. Adjacent Delete + Insert runs are paired (so a one-line
/// "edit" becomes a single Replaced row, not a Removed row above an Added
/// row), which is what makes the side-by-side alignment readable.
pub fn build_diff_rows(left: &[String], right: &[String]) -> Vec<DiffRow> {
    use similar::{ChangeTag, TextDiff};
    let l: Vec<&str> = left.iter().map(|s| s.as_str()).collect();
    let r: Vec<&str> = right.iter().map(|s| s.as_str()).collect();
    let diff = TextDiff::from_slices(&l, &r);
    let changes: Vec<_> = diff.iter_all_changes().collect();
    let mut rows = Vec::new();
    let mut li = 0usize;
    let mut ri = 0usize;
    let mut i = 0usize;
    while i < changes.len() {
        match changes[i].tag() {
            ChangeTag::Equal => {
                rows.push(DiffRow::Equal { left: li, right: ri });
                li += 1;
                ri += 1;
                i += 1;
            }
            _ => {
                let mut removed: Vec<usize> = Vec::new();
                while i < changes.len() && changes[i].tag() == ChangeTag::Delete {
                    removed.push(li);
                    li += 1;
                    i += 1;
                }
                let mut added: Vec<usize> = Vec::new();
                while i < changes.len() && changes[i].tag() == ChangeTag::Insert {
                    added.push(ri);
                    ri += 1;
                    i += 1;
                }
                let pair = removed.len().min(added.len());
                for k in 0..pair {
                    rows.push(DiffRow::Replaced { left: removed[k], right: added[k] });
                }
                for k in pair..removed.len() {
                    rows.push(DiffRow::Removed { left: removed[k] });
                }
                for k in pair..added.len() {
                    rows.push(DiffRow::Added { right: added[k] });
                }
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn identical_inputs_yield_only_equal_rows() {
        let rows = build_diff_rows(
            &lines(&["alpha", "beta", "gamma"]),
            &lines(&["alpha", "beta", "gamma"]),
        );
        assert_eq!(rows.len(), 3);
        for r in rows {
            assert!(matches!(r, DiffRow::Equal { .. }));
        }
    }

    #[test]
    fn pure_addition_emits_added_rows() {
        let rows = build_diff_rows(
            &lines(&[]),
            &lines(&["new1", "new2"]),
        );
        assert_eq!(rows, vec![
            DiffRow::Added { right: 0 },
            DiffRow::Added { right: 1 },
        ]);
    }

    #[test]
    fn pure_removal_emits_removed_rows() {
        let rows = build_diff_rows(
            &lines(&["old1", "old2"]),
            &lines(&[]),
        );
        assert_eq!(rows, vec![
            DiffRow::Removed { left: 0 },
            DiffRow::Removed { left: 1 },
        ]);
    }

    #[test]
    fn one_line_edit_is_paired_into_a_single_replaced_row() {
        let rows = build_diff_rows(
            &lines(&["alpha", "beta", "gamma"]),
            &lines(&["alpha", "BETA", "gamma"]),
        );
        assert_eq!(
            rows,
            vec![
                DiffRow::Equal { left: 0, right: 0 },
                DiffRow::Replaced { left: 1, right: 1 },
                DiffRow::Equal { left: 2, right: 2 },
            ]
        );
    }

    #[test]
    fn unequal_run_lengths_pair_what_they_can_then_overflow() {
        // 3 removed + 1 added → 1 replaced + 2 removed (added-side runs out).
        let rows = build_diff_rows(
            &lines(&["a", "b", "c", "d"]),
            &lines(&["A"]),
        );
        let kinds: Vec<&'static str> = rows
            .iter()
            .map(|r| match r {
                DiffRow::Equal { .. } => "eq",
                DiffRow::Replaced { .. } => "rep",
                DiffRow::Removed { .. } => "rm",
                DiffRow::Added { .. } => "add",
            })
            .collect();
        assert_eq!(kinds, vec!["rep", "rm", "rm", "rm"]);
    }

    #[test]
    fn horizontal_scroll_helpers_clamp_at_longest_line() {
        let mut d = DiffData::build(
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            lines(&["short", "medium length", "much-much-much-much-much-longer"]),
            lines(&["short", "medium length", "much-much-much-much-much-longer"]),
        );
        assert_eq!(d.scroll_x, 0);
        d.scroll_left_by(5);
        assert_eq!(d.scroll_x, 0, "saturating sub at 0");
        d.scroll_right_by(10);
        assert_eq!(d.scroll_x, 10);
        let longest = d.longest_line_chars();
        d.scroll_right_by(99_999);
        assert_eq!(d.scroll_x, longest, "right scroll clamps at longest line");
        d.scroll_left_by(longest);
        assert_eq!(d.scroll_x, 0);
    }

    #[test]
    fn scroll_helpers_clamp_at_zero_and_total() {
        let mut d = DiffData::build(
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            lines(&["1", "2", "3", "4", "5"]),
            lines(&["1", "2", "X", "4", "5"]),
        );
        assert_eq!(d.scroll, 0);
        d.scroll_up_by(3);
        assert_eq!(d.scroll, 0, "saturating sub at 0");
        d.scroll_down_by(2);
        assert_eq!(d.scroll, 2);
        d.scroll_down_by(99);
        assert_eq!(d.scroll, d.rows.len(), "clamp at total rows");
        d.scroll_home();
        assert_eq!(d.scroll, 0);
        d.scroll_end();
        assert_eq!(d.scroll, d.rows.len());
    }

    #[test]
    fn build_data_loads_paths_and_rows() {
        let d = DiffData::build(
            PathBuf::from("/tmp/a.txt"),
            PathBuf::from("/tmp/b.txt"),
            lines(&["x", "y"]),
            lines(&["x", "Y"]),
        );
        assert_eq!(d.total_rows(), 2);
        assert_eq!(d.left_lines, lines(&["x", "y"]));
        assert_eq!(d.right_lines, lines(&["x", "Y"]));
        assert_eq!(d.scroll, 0);
    }
}
