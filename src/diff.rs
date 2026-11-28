//! The line diff under every diff croft shows: which lines of two texts
//! pair up as equal, removed, added or replaced, within a time budget, and
//! which characters of a replaced line changed. The diff tab's own state
//! (`widgets::diff::DiffData`), the merge editor, SARIF fixes and fleet all
//! build on it; nothing here draws.

/// The changed characters of a replaced line against its new version
/// (#1351), as `(start, end)` char ranges for the old and the new line:
/// what is left once the common prefix and suffix are trimmed, which marks
/// the usual edit (a value, a name, an inserted argument) exactly. `None`
/// when the lines are equal, or share under 30% of the longer one, so a
/// rewritten line keeps its plain tint rather than turning into confetti.
pub fn changed_char_ranges(old: &str, new: &str) -> Option<((usize, usize), (usize, usize))> {
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();
    if a == b {
        return None;
    }
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let longer = a.len().max(b.len());
    if (prefix + suffix) * 10 < longer * 3 {
        return None;
    }
    Some(((prefix, a.len() - suffix), (prefix, b.len() - suffix)))
}

/// One visual row in a side-by-side diff view. The left column shows
/// `left_lines[left]` (or blank when `Added`) and the right column shows
/// `right_lines[right]` (or blank when `Removed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffRow {
    Equal {
        left: usize,
        right: usize,
    },
    Removed {
        left: usize,
    },
    Added {
        right: usize,
    },
    /// A line that was replaced: both columns paint, left in `left_lines`
    /// and right in `right_lines`. Produced by pairing consecutive
    /// Delete + Insert hunks so the visual rows align without forcing a
    /// pure-add or pure-remove zigzag.
    Replaced {
        left: usize,
        right: usize,
    },
}

/// How long a line diff may search for the smallest edit script (#1152).
/// Myers' cost grows with file length times lines changed: a 200k-line
/// lockfile with half its lines changed ran for minutes on the UI thread.
/// Past the deadline the rest of the change is reported as plain removals
/// and additions: coarser, but still exactly the two texts, so staging a
/// hunk stays correct. VS Code caps its diff the same way.
pub const DIFF_DEADLINE: std::time::Duration = std::time::Duration::from_millis(500);

/// Run a line-level diff over `left` vs `right` and emit one DiffRow per
/// visual row. Adjacent Delete + Insert runs are paired (so a one-line
/// "edit" becomes a single Replaced row, not a Removed row above an Added
/// row), which is what makes the side-by-side alignment readable.
pub fn build_diff_rows<L: AsRef<str>, R: AsRef<str>>(left: &[L], right: &[R]) -> Vec<DiffRow> {
    build_diff_rows_within(left, right, DIFF_DEADLINE).0
}

/// [`build_diff_rows`] with its search bounded by `budget`; the flag is
/// true when the budget ran out and the rows are the coarser diff.
pub fn build_diff_rows_within<L: AsRef<str>, R: AsRef<str>>(
    left: &[L],
    right: &[R],
    budget: std::time::Duration,
) -> (Vec<DiffRow>, bool) {
    use similar::{ChangeTag, TextDiff};
    let l: Vec<&str> = left.iter().map(AsRef::as_ref).collect();
    let r: Vec<&str> = right.iter().map(AsRef::as_ref).collect();
    let started = std::time::Instant::now();
    let diff = TextDiff::configure().timeout(budget).diff_slices(&l, &r);
    let coarse = started.elapsed() >= budget;
    let changes: Vec<_> = diff.iter_all_changes().collect();
    let mut rows = Vec::new();
    let mut li = 0usize;
    let mut ri = 0usize;
    let mut i = 0usize;
    while i < changes.len() {
        match changes[i].tag() {
            ChangeTag::Equal => {
                rows.push(DiffRow::Equal {
                    left: li,
                    right: ri,
                });
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
                    rows.push(DiffRow::Replaced {
                        left: removed[k],
                        right: added[k],
                    });
                }
                for &left in removed.iter().skip(pair) {
                    rows.push(DiffRow::Removed { left });
                }
                for &right in added.iter().skip(pair) {
                    rows.push(DiffRow::Added { right });
                }
            }
        }
    }
    (rows, coarse)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1351: the changed characters of a paired line, by common prefix
    /// and suffix: `1.5` → `2.5` is just `1` and `2`.
    #[test]
    fn changed_chars_trim_the_common_prefix_and_suffix() {
        assert_eq!(
            changed_char_ranges("backoff=1.5", "backoff=2.5"),
            Some(((8, 9), (8, 9)))
        );
        assert_eq!(
            changed_char_ranges("abc", "abXc"),
            Some(((2, 2), (2, 3))),
            "an insertion marks only the new side"
        );
    }

    /// #1351 negative: identical lines and lines sharing too little have no
    /// ranges.
    #[test]
    fn changed_chars_are_none_for_equal_or_rewritten_lines() {
        assert_eq!(changed_char_ranges("same", "same"), None);
        assert_eq!(
            changed_char_ranges("alpha beta gamma", "quite other words"),
            None
        );
    }

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
        let rows = build_diff_rows(&lines(&[]), &lines(&["new1", "new2"]));
        assert_eq!(
            rows,
            vec![DiffRow::Added { right: 0 }, DiffRow::Added { right: 1 },]
        );
    }

    #[test]
    fn pure_removal_emits_removed_rows() {
        let rows = build_diff_rows(&lines(&["old1", "old2"]), &lines(&[]));
        assert_eq!(
            rows,
            vec![DiffRow::Removed { left: 0 }, DiffRow::Removed { left: 1 },]
        );
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
        let rows = build_diff_rows(&lines(&["a", "b", "c", "d"]), &lines(&["A"]));
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
}
