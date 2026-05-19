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
    /// True when the raw byte contents differ but `build_diff_rows`
    /// produced zero non-Equal rows — meaning the difference is invisible
    /// at the line level (trailing newline, CRLF↔LF, BOM, or whitespace
    /// the `.lines()` splitter normalises). Surfaces in the diff header
    /// so the user understands why no red/green bands are painted even
    /// though git reports the file as modified.
    pub bytes_differ_but_lines_equal: bool,
    /// True when the diff should render as a single-column unified view
    /// instead of the two-column side-by-side. Set by
    /// `build_unified_deletion` so a tombstone for a file the source-
    /// control panel reports as deleted can show every removed line with
    /// a `-` sign and a red band — visually identical to `git diff` for a
    /// removed file.
    pub unified: bool,
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
        Self::build_with_byte_check(left_path, right_path, left_lines, right_lines, None, None)
    }

    /// Like `build`, but also takes the raw byte contents so the
    /// constructor can detect the "byte-different but line-identical"
    /// case (trailing newline, CRLF, BOM, etc.) and set
    /// `bytes_differ_but_lines_equal` accordingly. Pass `None` for the
    /// raw texts when the caller doesn't have them (synthetic / test
    /// diffs); the flag stays false and the renderer behaves as before.
    pub fn build_with_byte_check(
        left_path: PathBuf,
        right_path: PathBuf,
        left_lines: Vec<String>,
        right_lines: Vec<String>,
        left_raw: Option<&str>,
        right_raw: Option<&str>,
    ) -> Self {
        let rows = build_diff_rows(&left_lines, &right_lines);
        let bytes_differ_but_lines_equal = match (left_raw, right_raw) {
            (Some(l), Some(r)) => {
                l != r && rows.iter().all(|r| matches!(r, DiffRow::Equal { .. }))
            }
            _ => false,
        };
        Self {
            left_path,
            right_path,
            left_lines,
            right_lines,
            rows,
            scroll: 0,
            scroll_x: 0,
            bytes_differ_but_lines_equal,
            unified: false,
        }
    }

    /// Build a single-column unified diff that represents a fully-deleted
    /// file: every line of `text` becomes a `Removed` row keyed off
    /// `left_lines`. The right side is empty; the renderer keys off
    /// `unified == true` to draw a one-column view instead of two.
    pub fn build_unified_deletion(label: PathBuf, text: &str) -> Self {
        let left_lines: Vec<String> = text.lines().map(str::to_string).collect();
        let rows: Vec<DiffRow> = (0..left_lines.len())
            .map(|i| DiffRow::Removed { left: i })
            .collect();
        Self {
            left_path: label,
            right_path: PathBuf::from("/dev/null"),
            left_lines,
            right_lines: Vec::new(),
            rows,
            scroll: 0,
            scroll_x: 0,
            bytes_differ_but_lines_equal: false,
            unified: true,
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

    /// Indices into `rows` where each contiguous change hunk begins (an
    /// Added / Removed / Replaced row preceded by either start-of-file or
    /// an Equal row). Drives Next / Prev change navigation in the diff
    /// view and the "jump to first change on open" behaviour the Source
    /// Control panel relies on.
    pub fn hunk_starts(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut in_hunk = false;
        for (i, r) in self.rows.iter().enumerate() {
            let is_change = !matches!(r, DiffRow::Equal { .. });
            if is_change && !in_hunk {
                out.push(i);
                in_hunk = true;
            } else if !is_change {
                in_hunk = false;
            }
        }
        out
    }

    /// First-row index of the first change hunk, or `None` when the two
    /// sides are identical.
    pub fn first_change_row(&self) -> Option<usize> {
        self.hunk_starts().into_iter().next()
    }

    /// First-row index of the next hunk strictly past `current`, or
    /// `None` when `current` is already in/past the last hunk.
    pub fn next_change_row(&self, current: usize) -> Option<usize> {
        self.hunk_starts().into_iter().find(|&s| s > current)
    }

    /// First-row index of the hunk that starts strictly before `current`,
    /// or `None` when `current` is already at/before the first hunk.
    pub fn prev_change_row(&self, current: usize) -> Option<usize> {
        self.hunk_starts().into_iter().rev().find(|&s| s < current)
    }

    /// Park `scroll` two rows above `target` so the change row lands with
    /// a slice of context above it, the way users read diffs.
    pub fn scroll_to_row(&mut self, target: usize) {
        self.scroll = target.saturating_sub(2);
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

    #[test]
    fn first_change_row_skips_leading_equal_rows() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d"]),
            lines(&["a", "b", "X", "d"]),
        );
        assert_eq!(
            d.first_change_row(),
            Some(2),
            "first change must skip the two leading Equal rows"
        );
    }

    #[test]
    fn first_change_row_is_none_for_identical_inputs() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b"]),
            lines(&["a", "b"]),
        );
        assert_eq!(d.first_change_row(), None);
    }

    #[test]
    fn hunk_starts_lists_one_index_per_contiguous_change_block() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d", "e", "f"]),
            lines(&["a", "B", "c", "d", "E", "F"]),
        );
        // Hunk 1: row 1 (Replaced b→B), Hunk 2: rows 4-5 (Replaced e→E, f→F).
        let starts = d.hunk_starts();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0], 1);
        assert_eq!(starts[1], 4);
    }

    #[test]
    fn next_change_row_jumps_to_the_next_hunk_after_the_cursor() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d", "e"]),
            lines(&["a", "B", "c", "d", "E"]),
        );
        assert_eq!(d.next_change_row(0), Some(1));
        assert_eq!(d.next_change_row(1), Some(4), "must skip past the current hunk");
        assert_eq!(d.next_change_row(4), None);
    }

    #[test]
    fn prev_change_row_jumps_to_the_previous_hunk_before_the_cursor() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d", "e"]),
            lines(&["a", "B", "c", "d", "E"]),
        );
        assert_eq!(d.prev_change_row(4), Some(1));
        assert_eq!(d.prev_change_row(1), None);
        assert_eq!(d.prev_change_row(0), None);
    }

    #[test]
    fn scroll_to_row_parks_two_rows_of_context_above_the_target() {
        let mut d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d", "e"]),
            lines(&["a", "b", "C", "d", "e"]),
        );
        d.scroll_to_row(2);
        assert_eq!(d.scroll, 0, "row 2 with 2-row context lands scroll at 0");
        d.scroll_to_row(7);
        assert_eq!(d.scroll, 5);
    }

    #[test]
    fn build_with_byte_check_flags_trailing_newline_only_difference() {
        // Same lines on both sides but the working copy lost its trailing
        // newline. `.lines()` collapses the difference; the byte check
        // catches it so the renderer can surface a banner.
        let left_text = "alpha\nbeta\n";
        let right_text = "alpha\nbeta";
        let left_lines: Vec<String> = left_text.lines().map(str::to_string).collect();
        let right_lines: Vec<String> = right_text.lines().map(str::to_string).collect();
        let d = DiffData::build_with_byte_check(
            PathBuf::from("/x"),
            PathBuf::from("/x"),
            left_lines,
            right_lines,
            Some(left_text),
            Some(right_text),
        );
        assert!(
            d.bytes_differ_but_lines_equal,
            "trailing-newline difference must set bytes_differ_but_lines_equal so the diff header explains why no red/green band paints"
        );
        assert!(
            d.rows.iter().all(|r| matches!(r, DiffRow::Equal { .. })),
            "the line-level diff is still entirely Equal rows — the flag is what tells the user the file is byte-different"
        );
    }

    #[test]
    fn build_with_byte_check_leaves_flag_false_when_a_real_line_changes() {
        let left_text = "alpha\nbeta\n";
        let right_text = "alpha\nBETA\n";
        let left_lines: Vec<String> = left_text.lines().map(str::to_string).collect();
        let right_lines: Vec<String> = right_text.lines().map(str::to_string).collect();
        let d = DiffData::build_with_byte_check(
            PathBuf::from("/x"),
            PathBuf::from("/x"),
            left_lines,
            right_lines,
            Some(left_text),
            Some(right_text),
        );
        assert!(
            !d.bytes_differ_but_lines_equal,
            "the flag must stay false when the diff has real Replaced rows — otherwise the header would lie that there's no line-level change"
        );
    }

    #[test]
    fn build_unified_deletion_emits_only_removed_rows_and_sets_unified_flag() {
        let d = DiffData::build_unified_deletion(
            PathBuf::from("doomed.rs"),
            "fn main() {\n    println!(\"bye\");\n}\n",
        );
        assert!(d.unified, "unified flag must be set so the renderer picks the single-column path");
        assert_eq!(d.left_lines.len(), 3, "three source lines must produce three rows");
        assert!(d.right_lines.is_empty(), "deletion view has no right side");
        assert_eq!(
            d.rows,
            vec![
                DiffRow::Removed { left: 0 },
                DiffRow::Removed { left: 1 },
                DiffRow::Removed { left: 2 },
            ],
            "every row must be Removed so all lines paint red with a `-` sign"
        );
    }

    #[test]
    fn standard_diff_builders_leave_unified_flag_off() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a"]),
            lines(&["b"]),
        );
        assert!(!d.unified, "side-by-side diffs must stay non-unified");
    }

    #[test]
    fn build_with_byte_check_leaves_flag_false_when_bytes_are_identical() {
        let text = "alpha\nbeta\n";
        let lines_vec: Vec<String> = text.lines().map(str::to_string).collect();
        let d = DiffData::build_with_byte_check(
            PathBuf::from("/x"),
            PathBuf::from("/x"),
            lines_vec.clone(),
            lines_vec,
            Some(text),
            Some(text),
        );
        assert!(!d.bytes_differ_but_lines_equal);
    }
}
