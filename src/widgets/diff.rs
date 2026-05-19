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

    /// Build a side-by-side diff from the raw stdout of `git diff` (or
    /// `git diff --staged`, etc.) by separating the parsed text into a
    /// left (old) and right (new) stream and emitting the same
    /// Equal/Removed/Added/Replaced rows the two-column renderer uses
    /// for file-vs-file diffs. The result has `unified = false` so the
    /// editor pane renders it through the standard side-by-side path.
    ///
    /// Parsing rules per line of `raw`:
    /// * `diff --git …` and `@@ … @@` → Equal row in both columns
    ///   (acts as a visible file / hunk header).
    /// * `index …`, `--- …`, `+++ …`, `\ No newline at end of file`
    ///   → skipped (redundant after the `diff --git` header).
    /// * `-xxx` → push body to `left_lines`, buffer as pending removed.
    /// * `+xxx` → push body to `right_lines`, buffer as pending added.
    /// * any other line (context, blank) → flush buffered runs (pairing
    ///   removed+added into Replaced rows), then push the body to both
    ///   sides as an Equal row.
    ///
    /// Empty `raw` produces a single Equal row carrying "(no changes)"
    /// so the renderer always has something to paint.
    pub fn build_side_by_side_from_git_text(label: PathBuf, raw: &str) -> Self {
        let mut left_lines: Vec<String> = Vec::new();
        let mut right_lines: Vec<String> = Vec::new();
        let mut rows: Vec<DiffRow> = Vec::new();
        if raw.trim().is_empty() {
            left_lines.push(String::from("(no changes)"));
            right_lines.push(String::from("(no changes)"));
            rows.push(DiffRow::Equal { left: 0, right: 0 });
            return Self {
                left_path: label,
                right_path: PathBuf::new(),
                left_lines,
                right_lines,
                rows,
                scroll: 0,
                scroll_x: 0,
                bytes_differ_but_lines_equal: false,
                unified: false,
            };
        }
        let mut pending_remove: Vec<usize> = Vec::new();
        let mut pending_add: Vec<usize> = Vec::new();
        let flush = |pending_remove: &mut Vec<usize>,
                     pending_add: &mut Vec<usize>,
                     rows: &mut Vec<DiffRow>| {
            let pair = pending_remove.len().min(pending_add.len());
            for k in 0..pair {
                rows.push(DiffRow::Replaced {
                    left: pending_remove[k],
                    right: pending_add[k],
                });
            }
            for k in pair..pending_remove.len() {
                rows.push(DiffRow::Removed { left: pending_remove[k] });
            }
            for k in pair..pending_add.len() {
                rows.push(DiffRow::Added { right: pending_add[k] });
            }
            pending_remove.clear();
            pending_add.clear();
        };
        for line in raw.split('\n') {
            if line.starts_with("diff --git") || line.starts_with("@@") {
                flush(&mut pending_remove, &mut pending_add, &mut rows);
                let i = left_lines.len();
                let j = right_lines.len();
                left_lines.push(line.to_string());
                right_lines.push(line.to_string());
                rows.push(DiffRow::Equal { left: i, right: j });
            } else if line.starts_with("index ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
                || line == "---"
                || line == "+++"
                || line.starts_with("\\ No newline")
                || line.starts_with("new file mode")
                || line.starts_with("deleted file mode")
                || line.starts_with("similarity index")
                || line.starts_with("rename from")
                || line.starts_with("rename to")
            {
                // Header / metadata noise — skip; the `diff --git` line
                // already names the file pair.
                continue;
            } else if let Some(rest) = line.strip_prefix('-') {
                let i = left_lines.len();
                left_lines.push(rest.to_string());
                pending_remove.push(i);
            } else if let Some(rest) = line.strip_prefix('+') {
                let j = right_lines.len();
                right_lines.push(rest.to_string());
                pending_add.push(j);
            } else {
                flush(&mut pending_remove, &mut pending_add, &mut rows);
                let body = line.strip_prefix(' ').unwrap_or(line);
                let i = left_lines.len();
                let j = right_lines.len();
                left_lines.push(body.to_string());
                right_lines.push(body.to_string());
                rows.push(DiffRow::Equal { left: i, right: j });
            }
        }
        flush(&mut pending_remove, &mut pending_add, &mut rows);
        Self {
            left_path: label,
            right_path: PathBuf::new(),
            left_lines,
            right_lines,
            rows,
            scroll: 0,
            scroll_x: 0,
            bytes_differ_but_lines_equal: false,
            unified: false,
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

    /// Like `next_change_row`, but wraps around to the first hunk when
    /// `current` is at/past the last one. Used by the diff-pane ›
    /// arrow and F7 so a user reading bottom-to-top can keep clicking
    /// the same arrow to cycle through every hunk. Returns `None` only
    /// when the diff has no change rows at all.
    pub fn next_change_row_wrap(&self, current: usize) -> Option<usize> {
        let starts = self.hunk_starts();
        if starts.is_empty() {
            return None;
        }
        starts
            .iter()
            .find(|&&s| s > current)
            .copied()
            .or_else(|| starts.first().copied())
    }

    /// Mirror of `next_change_row_wrap` going the other way: when
    /// `current` sits at/before the first hunk, the next call wraps to
    /// the last hunk instead of stalling.
    pub fn prev_change_row_wrap(&self, current: usize) -> Option<usize> {
        let starts = self.hunk_starts();
        if starts.is_empty() {
            return None;
        }
        starts
            .iter()
            .rev()
            .find(|&&s| s < current)
            .copied()
            .or_else(|| starts.last().copied())
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
    fn next_change_row_wrap_loops_from_last_hunk_back_to_first() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d", "e"]),
            lines(&["a", "B", "c", "d", "E"]),
        );
        assert_eq!(d.next_change_row_wrap(0), Some(1));
        assert_eq!(d.next_change_row_wrap(1), Some(4));
        // Past the last hunk: wrap to the first instead of stalling.
        assert_eq!(
            d.next_change_row_wrap(4),
            Some(1),
            "at/past the last hunk the next ⟶ click must loop back to the first"
        );
        assert_eq!(d.next_change_row_wrap(999), Some(1));
    }

    #[test]
    fn prev_change_row_wrap_loops_from_first_hunk_back_to_last() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d", "e"]),
            lines(&["a", "B", "c", "d", "E"]),
        );
        assert_eq!(d.prev_change_row_wrap(4), Some(1));
        assert_eq!(
            d.prev_change_row_wrap(1),
            Some(4),
            "at/before the first hunk the previous ⟵ click must loop to the last"
        );
        assert_eq!(d.prev_change_row_wrap(0), Some(4));
    }

    #[test]
    fn change_row_wrap_returns_none_when_diff_has_no_changes() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c"]),
            lines(&["a", "b", "c"]),
        );
        assert_eq!(d.next_change_row_wrap(0), None);
        assert_eq!(d.prev_change_row_wrap(0), None);
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

    #[test]
    fn build_side_by_side_from_git_text_pairs_remove_add_into_replaced() {
        // A one-line edit (- old / + new) must collapse into a single
        // Replaced row so the two sides align visually instead of
        // producing a zigzag of Removed-above-Added.
        let raw = "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1,3 +1,3 @@\n keep\n-old\n+new\n";
        let d = DiffData::build_side_by_side_from_git_text(PathBuf::from("staged"), raw);
        assert!(
            !d.unified,
            "side-by-side builder must clear the unified flag so the two-column renderer takes over"
        );
        let tags: Vec<&'static str> = d
            .rows
            .iter()
            .map(|r| match r {
                DiffRow::Equal { .. } => "eq",
                DiffRow::Added { .. } => "add",
                DiffRow::Removed { .. } => "rm",
                DiffRow::Replaced { .. } => "rep",
            })
            .collect();
        // Expected after skipping index/---/+++ noise:
        //   diff --git → Equal (file header)
        //   @@ ...     → Equal (hunk header)
        //    keep      → Equal (context)
        //   -old / +new → paired into Replaced
        //   trailing empty line → Equal
        assert_eq!(
            tags,
            vec!["eq", "eq", "eq", "rep", "eq"],
            "remove+add pair must collapse into one Replaced row; file/hunk headers stay as Equal markers"
        );
        assert!(
            d.left_lines.iter().any(|s| s == "old"),
            "removed body (without leading -) belongs in left_lines"
        );
        assert!(
            d.right_lines.iter().any(|s| s == "new"),
            "added body (without leading +) belongs in right_lines"
        );
    }

    #[test]
    fn build_side_by_side_from_git_text_overflow_runs_emit_extra_removed_or_added() {
        // 3 removed + 1 added → 1 Replaced + 2 Removed (added side runs out).
        let raw = "diff --git a/x b/x\n@@ -1,4 +1,2 @@\n keep\n-a\n-b\n-c\n+A\n other\n";
        let d = DiffData::build_side_by_side_from_git_text(PathBuf::from("staged"), raw);
        let change_tags: Vec<&'static str> = d
            .rows
            .iter()
            .filter_map(|r| match r {
                DiffRow::Replaced { .. } => Some("rep"),
                DiffRow::Removed { .. } => Some("rm"),
                DiffRow::Added { .. } => Some("add"),
                _ => None,
            })
            .collect();
        assert_eq!(
            change_tags,
            vec!["rep", "rm", "rm"],
            "unequal runs pair what they can then overflow with the longer side"
        );
    }

    #[test]
    fn build_side_by_side_from_git_text_skips_index_and_dashdash_headers() {
        let raw = "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n";
        let d = DiffData::build_side_by_side_from_git_text(PathBuf::from("staged"), raw);
        // No row may carry "index ", "--- a/x", or "+++ b/x" in either side.
        assert!(
            !d.left_lines.iter().any(|s| s.starts_with("index ")
                || s.starts_with("--- a/")
                || s.starts_with("+++ b/")),
            "index / --- / +++ noise must be skipped from left_lines: {:?}",
            d.left_lines
        );
        assert!(
            !d.right_lines.iter().any(|s| s.starts_with("index ")
                || s.starts_with("--- a/")
                || s.starts_with("+++ b/")),
            "index / --- / +++ noise must be skipped from right_lines: {:?}",
            d.right_lines
        );
    }

    #[test]
    fn build_side_by_side_from_git_text_empty_input_paints_placeholder_row() {
        let d = DiffData::build_side_by_side_from_git_text(PathBuf::from("staged"), "");
        assert_eq!(d.rows.len(), 1);
        assert!(matches!(d.rows[0], DiffRow::Equal { .. }));
        assert_eq!(d.left_lines, vec!["(no changes)".to_string()]);
        assert!(!d.unified);
    }
}
