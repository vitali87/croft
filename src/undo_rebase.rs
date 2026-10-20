//! Carrying a collab peer's edit into this user's undo and redo history
//! (#919). Undo holds whole-buffer states, each one step away from the
//! next; a peer's edit opens no step of its own but lands in every held
//! state, so Ctrl+Z walks back only this user's own steps and keeps the
//! peer's text. A state takes the edit where the same text sits in it: text
//! a step left alone maps by offset, and an end of the edit that falls in
//! text the step wrote, which the other state does not have, moves to the
//! edge of what the other state holds there.

use std::time::Duration;

/// A replacement in lines joined by `\n`: bytes `at..at + deleted` become
/// `inserted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Splice {
    pub at: usize,
    pub deleted: usize,
    pub inserted: String,
}

/// How long the line diff of one step may take before it settles for a
/// coarser answer: a whole-file reformat still diffs in bounded time.
const STEP_DIFF_TIMEOUT: Duration = Duration::from_millis(20);

/// `edit`, written against `source`, carried to `target`, a state one undo
/// or redo step away from it. `None` when it would change nothing there,
/// because it only deleted text the step wrote; then no state further on
/// changes either.
pub fn rebase(target: &str, source: &str, edit: &Splice) -> Option<Splice> {
    let (start, end) = (edit.at, edit.at + edit.deleted);
    let mut prefix = common_prefix(target.as_bytes(), source.as_bytes());
    while !target.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = common_suffix(&target.as_bytes()[prefix..], &source.as_bytes()[prefix..]);
    while !target.is_char_boundary(target.len() - suffix) {
        suffix -= 1;
    }
    let (target_mid, source_mid) = (target.len() - suffix, source.len() - suffix);
    // Shared stretches as (target offset, source offset, length), in order.
    let mut runs = vec![(0, 0, prefix)];
    // The step's own span needs a line diff only when an end of the edit
    // falls inside it: a multi-caret edit or a format leaves untouched
    // lines between the ones it changed.
    let inside = |x: usize| prefix < x && x < source_mid;
    if inside(start) || inside(end) {
        let mid = line_runs(&target[prefix..target_mid], &source[prefix..source_mid]);
        runs.extend(mid.into_iter().map(|(t, s, n)| (t + prefix, s + prefix, n)));
    }
    runs.push((target_mid, source_mid, suffix));
    let at = map_offset(&runs, start, true);
    let to = map_offset(&runs, end, false).max(at);
    (to > at || !edit.inserted.is_empty()).then(|| Splice {
        at,
        deleted: to - at,
        inserted: edit.inserted.clone(),
    })
}

/// Where offset `x` of the source falls in the target. In a stretch the two
/// share it keeps its place; between stretches (text the step wrote) the
/// start of an edit goes to the end of the target's text there and the end
/// of an edit to its start, so the edit covers none of it.
fn map_offset(runs: &[(usize, usize, usize)], x: usize, start: bool) -> usize {
    let mut gap_start = 0;
    for &(t, s, n) in runs {
        if x < s {
            return if start { t } else { gap_start };
        }
        if x <= s + n {
            return t + (x - s);
        }
        gap_start = t + n;
    }
    gap_start
}

/// The lines `target` and `source` share, as `(target offset, source
/// offset, length)` byte ranges in order.
fn line_runs(target: &str, source: &str) -> Vec<(usize, usize, usize)> {
    let diff = similar::TextDiff::configure()
        .timeout(STEP_DIFF_TIMEOUT)
        .diff_lines(target, source);
    let offsets = |lens: &mut dyn Iterator<Item = usize>| -> Vec<usize> {
        std::iter::once(0)
            .chain(lens.scan(0, |sum, n| {
                *sum += n;
                Some(*sum)
            }))
            .collect()
    };
    let old = offsets(&mut diff.iter_old_slices().map(str::len));
    let new = offsets(&mut diff.iter_new_slices().map(str::len));
    diff.ops()
        .iter()
        .filter_map(|op| match op.as_tag_tuple() {
            (similar::DiffTag::Equal, o, n) => {
                Some((old[o.start], new[n.start], old[o.end] - old[o.start]))
            }
            _ => None,
        })
        .collect()
}

/// Bytes at the start of `a` and `b` that match.
fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    // Whole chunks first: slice equality is a memcmp.
    while i + 64 <= n && a[i..i + 64] == b[i..i + 64] {
        i += 64;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

/// Bytes at the end of `a` and `b` that match.
fn common_suffix(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let (a, b) = (&a[a.len() - n..], &b[b.len() - n..]);
    let mut i = 0;
    while i + 64 <= n && a[n - i - 64..n - i] == b[n - i - 64..n - i] {
        i += 64;
    }
    while i < n && a[n - i - 1] == b[n - i - 1] {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn splice(at: usize, deleted: usize, inserted: &str) -> Splice {
        Splice {
            at,
            deleted,
            inserted: inserted.to_string(),
        }
    }

    /// `edit` applied to `text`.
    fn apply(text: &str, edit: &Splice) -> String {
        let mut out = text.to_string();
        out.replace_range(edit.at..edit.at + edit.deleted, &edit.inserted);
        out
    }

    /// The older state takes the peer's edit, wherever it sits relative to
    /// the step that made the newer state.
    #[test]
    fn an_edit_beside_the_step_lands_in_the_older_state() {
        let before = "one\ntwo\nthree";
        let after = "one # A\ntwo\nthree";
        // Below the step: shifted by what the step added.
        let below = splice(after.find("two").unwrap() + 3, 0, " # B");
        let got = rebase(before, after, &below).unwrap();
        assert_eq!(apply(before, &got), "one\ntwo # B\nthree");
        // Above the step: same offset.
        let above = splice(0, 3, "ONE");
        let got = rebase(before, after, &above).unwrap();
        assert_eq!(apply(before, &got), "ONE\ntwo\nthree");
        // Right where the step's text begins: before it, so kept.
        let at_edge = splice(3, 0, "!");
        let got = rebase(before, after, &at_edge).unwrap();
        assert_eq!(apply(before, &got), "one!\ntwo\nthree");
    }

    /// A step that changed two separate lines leaves the lines between them
    /// alone, and an edit there maps exactly instead of to the step's edges.
    #[test]
    fn an_edit_between_the_lines_of_one_step_maps_exactly() {
        let before = "let a = x;\nlet b = x;\nlet c = x;";
        let after = "let a = y;\nlet b = x;\nlet c = y;";
        let at = after.find("b = x").unwrap() + 4;
        let got = rebase(before, after, &splice(at, 1, "z")).unwrap();
        assert_eq!(apply(before, &got), "let a = x;\nlet b = z;\nlet c = x;");
    }

    /// Text the step wrote does not exist in the older state: deleting it
    /// changes nothing there, typing inside it lands at its place, and a
    /// deletion running out of it removes only the text past it.
    #[test]
    fn an_edit_inside_the_steps_own_text_keeps_out_of_it() {
        let before = "ab";
        let after = "aXYZb";
        assert_eq!(rebase(before, after, &splice(2, 1, "")), None);
        let got = rebase(before, after, &splice(2, 1, "q")).unwrap();
        assert_eq!(apply(before, &got), "aqb");
        let got = rebase(before, after, &splice(3, 2, "")).unwrap();
        assert_eq!(apply(before, &got), "a");
        // A deletion spanning the step's text takes the older text there.
        let got = rebase("aOLDb", "aNEWb", &splice(0, 5, "")).unwrap();
        assert_eq!(apply("aOLDb", &got), "");
    }

    /// Offsets never split a character: two texts that differ only in the
    /// last byte of one character share no part of it.
    #[test]
    fn shared_stretches_end_on_char_boundaries() {
        let before = "xé y";
        let after = "xè y";
        let at = after.len() - 1;
        let got = rebase(before, after, &splice(at, 1, "z")).unwrap();
        assert_eq!(apply(before, &got), "xé z");
        let got = rebase(before, after, &splice(0, 1, "w")).unwrap();
        assert_eq!(apply(before, &got), "wé y");
    }

    #[test]
    fn common_ends_are_counted_past_whole_chunks() {
        let long = "a".repeat(200);
        let (x, y) = (format!("{long}1{long}"), format!("{long}22{long}"));
        assert_eq!(common_prefix(x.as_bytes(), y.as_bytes()), 200);
        assert_eq!(common_suffix(x.as_bytes(), y.as_bytes()), 200);
        assert_eq!(common_prefix(b"abc", b"abc"), 3);
        assert_eq!(common_suffix(b"abc", b"xbc"), 2);
    }
}
