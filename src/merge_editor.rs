//! Three-way merge editor state (#253): VS Code's merge-editor model
//! adapted to the TUI. Current (ours) and Incoming (theirs) render as
//! read-only panes above an editable Result, with Base toggleable.
//!
//! The three-way computation diffs base→ours and base→theirs, clusters
//! overlapping hunks into conflict regions, and auto-resolves everything
//! else straight into the initial Result — the main UX win over the
//! in-buffer marker flow, where every hunk needs a click. The Result
//! buffer itself is the host editor's ordinary text buffer; this module
//! only tracks where each conflict region lives inside it and what state
//! the region is in.

use crate::widgets::diff::{DiffRow, build_diff_rows};

/// What the user decided for one conflict region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictState {
    /// Untouched: the Result still holds the base text for this region.
    Unresolved,
    /// Accept Current (ours).
    Current,
    /// Accept Incoming (theirs).
    Incoming,
    /// Accept Combination: ours then theirs.
    Both,
    /// Accept Combination: theirs then ours.
    BothReverse,
    /// Ignore: explicitly keep the base text.
    Base,
    /// The user edited the region by hand in the Result pane.
    Manual,
}

impl ConflictState {
    /// Counts toward "N of M resolved" — everything except Unresolved.
    pub fn resolved(self) -> bool {
        self != ConflictState::Unresolved
    }
}

/// One conflict region: the three-side content plus its current span in
/// the Result buffer. Side `*_start` fields index into the full side
/// texts, anchoring pane scroll during conflict navigation.
#[derive(Clone, Debug)]
pub struct MergeConflict {
    pub base: Vec<String>,
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
    /// First line of the region in the base / ours / theirs full texts.
    pub base_start: usize,
    pub ours_start: usize,
    pub theirs_start: usize,
    /// Current span in the Result buffer, maintained across accepts and
    /// (heuristically) across manual edits.
    pub result_start: usize,
    pub result_len: usize,
    pub state: ConflictState,
}

impl MergeConflict {
    /// The Result lines an accept action splices in for `state`.
    pub fn replacement(&self, state: ConflictState) -> Vec<String> {
        match state {
            ConflictState::Current => self.ours.clone(),
            ConflictState::Incoming => self.theirs.clone(),
            ConflictState::Both => {
                let mut v = self.ours.clone();
                v.extend(self.theirs.iter().cloned());
                v
            }
            ConflictState::BothReverse => {
                let mut v = self.theirs.clone();
                v.extend(self.ours.iter().cloned());
                v
            }
            ConflictState::Base | ConflictState::Unresolved | ConflictState::Manual => {
                self.base.clone()
            }
        }
    }

    /// True when `row` (a Result line index) falls inside this region.
    /// A zero-length region claims its boundary row so an accept that
    /// emptied the region can still be re-accepted differently.
    pub fn contains_result_row(&self, row: usize) -> bool {
        if self.result_len == 0 {
            row == self.result_start
        } else {
            row >= self.result_start && row < self.result_start + self.result_len
        }
    }
}

/// Which pane a per-conflict checkbox belongs to (mouse hit-testing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckSide {
    Current,
    Incoming,
}

/// The merge editor's whole state, hung off `Editor.merge`. The Result
/// text itself lives in the host editor's `lines`.
pub struct MergeView {
    pub base: Vec<String>,
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
    pub conflicts: Vec<MergeConflict>,
    /// "Show Base" — a third source pane when there's room.
    pub show_base: bool,
    /// Conflict index the last navigation landed on; accept actions fall
    /// back to it when the cursor is not inside any region.
    pub active: usize,
    /// Source-pane viewports, re-anchored by conflict navigation and
    /// nudged together by Alt+Up / Alt+Down.
    pub ours_scroll: usize,
    pub theirs_scroll: usize,
    pub base_scroll: usize,
    /// True when the sides were synthesized from conflict markers rather
    /// than read from git index stages.
    pub from_markers: bool,
    /// Manual-edit sync bookkeeping: the Result length and `edit_seq`
    /// this view last reconciled against (see `sync_with_buffer`).
    pub synced_len: usize,
    pub synced_seq: u64,
    /// Per-frame checkbox hit rects: (screen y, x range, conflict, side).
    pub check_spans: Vec<(u16, std::ops::Range<u16>, usize, CheckSide)>,
    /// Per-frame rect of the source-pane area (mouse routing).
    pub last_panes_area: ratatui::layout::Rect,
}

impl MergeView {
    /// Build the view plus the initial Result: non-overlapping hunks from
    /// either side are applied automatically; overlapping clusters become
    /// conflict regions holding the base text until acted on.
    pub fn new(
        base: Vec<String>,
        ours: Vec<String>,
        theirs: Vec<String>,
        from_markers: bool,
    ) -> (Self, Vec<String>) {
        let (result, conflicts) = three_way(&base, &ours, &theirs);
        let view = MergeView {
            base,
            ours,
            theirs,
            conflicts,
            show_base: false,
            active: 0,
            ours_scroll: 0,
            theirs_scroll: 0,
            base_scroll: 0,
            from_markers,
            synced_len: result.len().max(1),
            synced_seq: 0,
            check_spans: Vec::new(),
            last_panes_area: ratatui::layout::Rect::default(),
        };
        (view, result)
    }

    pub fn resolved_count(&self) -> usize {
        self.conflicts.iter().filter(|c| c.state.resolved()).count()
    }

    pub fn unresolved_count(&self) -> usize {
        self.conflicts.len() - self.resolved_count()
    }

    /// The conflict containing `row` in the Result, if any.
    pub fn conflict_at_result_row(&self, row: usize) -> Option<usize> {
        self.conflicts
            .iter()
            .position(|c| c.contains_result_row(row))
    }

    /// The next conflict strictly after / before `row` in the Result,
    /// wrapping — F7's jump order, matching the in-buffer flow.
    pub fn next_conflict(&self, row: usize, backwards: bool) -> Option<usize> {
        if self.conflicts.is_empty() {
            return None;
        }
        if backwards {
            self.conflicts
                .iter()
                .rposition(|c| c.result_start < row)
                .or_else(|| Some(self.conflicts.len() - 1))
        } else {
            self.conflicts
                .iter()
                .position(|c| c.result_start > row)
                .or(Some(0))
        }
    }

    /// Bookkeeping for a programmatic accept: region `idx` was replaced
    /// by `new_len` lines and is now in `state`; later regions shift by
    /// the length delta. The caller performed the actual buffer splice
    /// and passes the post-edit `len`/`seq` so `sync_with_buffer` treats
    /// this edit as already reconciled.
    pub fn note_accept(
        &mut self,
        idx: usize,
        new_len: usize,
        state: ConflictState,
        buffer_len: usize,
        edit_seq: u64,
    ) {
        let old_len = self.conflicts[idx].result_len;
        let delta = new_len as isize - old_len as isize;
        self.conflicts[idx].result_len = new_len;
        self.conflicts[idx].state = state;
        for c in self.conflicts.iter_mut().skip(idx + 1) {
            c.result_start = (c.result_start as isize + delta).max(0) as usize;
        }
        self.active = idx;
        self.synced_len = buffer_len;
        self.synced_seq = edit_seq;
    }

    /// Reconcile with a manual buffer edit the view didn't perform
    /// itself. `edit_row` is where the edit happened (the cursor row at
    /// undo-push time); the length delta shifts regions after it, grows
    /// or shrinks the region containing it, and an edit inside a region
    /// marks that conflict manually resolved. Heuristic by design: one
    /// keystroke per frame is the norm, and drift self-limits to region
    /// boundaries — content is never touched.
    pub fn sync_with_buffer(&mut self, buffer_len: usize, edit_seq: u64, edit_row: usize) {
        if edit_seq == self.synced_seq {
            return;
        }
        let delta = buffer_len as isize - self.synced_len as isize;
        for c in self.conflicts.iter_mut() {
            if c.contains_result_row(edit_row) {
                c.result_len = (c.result_len as isize + delta).max(0) as usize;
                c.state = ConflictState::Manual;
            } else if c.result_start > edit_row {
                c.result_start = (c.result_start as isize + delta).max(0) as usize;
            }
        }
        self.synced_len = buffer_len;
        self.synced_seq = edit_seq;
    }

    /// Re-anchor all source-pane viewports on conflict `idx`, keeping a
    /// couple of context lines above.
    pub fn anchor_panes_on(&mut self, idx: usize) {
        const CTX: usize = 2;
        let Some(c) = self.conflicts.get(idx) else {
            return;
        };
        self.active = idx;
        self.ours_scroll = c.ours_start.saturating_sub(CTX);
        self.theirs_scroll = c.theirs_start.saturating_sub(CTX);
        self.base_scroll = c.base_start.saturating_sub(CTX);
    }

    /// Scroll all source panes together by `delta` (Alt+Up / Alt+Down).
    pub fn scroll_panes(&mut self, delta: isize) {
        let apply = |s: usize, max: usize| -> usize {
            (s as isize + delta).clamp(0, max.saturating_sub(1) as isize) as usize
        };
        self.ours_scroll = apply(self.ours_scroll, self.ours.len());
        self.theirs_scroll = apply(self.theirs_scroll, self.theirs.len());
        self.base_scroll = apply(self.base_scroll, self.base.len());
    }
}

/// One maximal changed run from a two-way diff: base rows
/// `[base_lo, base_hi)` were replaced by derived rows
/// `[derived_lo, derived_hi)`.
#[derive(Clone, Copy, Debug)]
struct Hunk {
    base_lo: usize,
    base_hi: usize,
    derived_lo: usize,
    derived_hi: usize,
}

/// Collapse a `DiffRow` alignment into maximal non-Equal runs.
fn hunks(base: &[String], derived: &[String]) -> Vec<Hunk> {
    let rows = build_diff_rows(base, derived);
    let mut out: Vec<Hunk> = Vec::new();
    // Row cursors advance through both sides; a changed row extends the
    // open hunk (or opens one), an Equal row closes it.
    let mut b = 0usize;
    let mut d = 0usize;
    let mut open: Option<Hunk> = None;
    for row in &rows {
        let changed = !matches!(row, DiffRow::Equal { .. });
        if changed && open.is_none() {
            open = Some(Hunk {
                base_lo: b,
                base_hi: b,
                derived_lo: d,
                derived_hi: d,
            });
        }
        if !changed && let Some(h) = open.take() {
            out.push(h);
        }
        match *row {
            DiffRow::Equal { .. } => {
                b += 1;
                d += 1;
            }
            DiffRow::Removed { .. } => b += 1,
            DiffRow::Added { .. } => d += 1,
            DiffRow::Replaced { .. } => {
                b += 1;
                d += 1;
            }
        }
        if let Some(h) = open.as_mut() {
            h.base_hi = b;
            h.derived_hi = d;
        }
    }
    if let Some(h) = open.take() {
        out.push(h);
    }
    out
}

/// Which side a hunk came from during clustering.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Ours,
    Theirs,
}

/// The three-way computation: diff base→ours and base→theirs, cluster
/// transitively overlapping hunks, then walk the base emitting auto-
/// resolved replacements and conflict regions (holding base text).
///
/// Overlap uses doubled coordinates so a pure insertion at base row `p`
/// occupies `(2p, 2p+1)` instead of the empty `[p, p)`: two insertions at
/// the same point conflict, while a hunk ending exactly where the other
/// side's begins stays independent (git's adjacency rule).
fn three_way(
    base: &[String],
    ours: &[String],
    theirs: &[String],
) -> (Vec<String>, Vec<MergeConflict>) {
    let a = hunks(base, ours);
    let b = hunks(base, theirs);
    let mut all: Vec<(Side, Hunk)> = a
        .iter()
        .map(|h| (Side::Ours, *h))
        .chain(b.iter().map(|h| (Side::Theirs, *h)))
        .collect();
    all.sort_by_key(|(_, h)| (h.base_lo, h.base_hi));
    let scaled = |h: &Hunk| -> (usize, usize) {
        if h.base_lo == h.base_hi {
            (2 * h.base_lo, 2 * h.base_lo + 1)
        } else {
            (2 * h.base_lo, 2 * h.base_hi)
        }
    };

    // Cluster transitively overlapping hunks (in scaled coordinates).
    let mut clusters: Vec<Vec<(Side, Hunk)>> = Vec::new();
    let mut cluster_hi = 0usize;
    for (side, h) in all {
        let (lo, hi) = scaled(&h);
        match clusters.last_mut() {
            Some(cur) if lo < cluster_hi => {
                cur.push((side, h));
                cluster_hi = cluster_hi.max(hi);
            }
            _ => {
                clusters.push(vec![(side, h)]);
                cluster_hi = hi;
            }
        }
    }

    // Cumulative side offsets: how far each side's text has drifted from
    // base line numbers before a given cluster, for `*_start` anchors.
    let mut result: Vec<String> = Vec::new();
    let mut conflicts: Vec<MergeConflict> = Vec::new();
    let mut base_pos = 0usize; // next base row not yet emitted
    let mut delta_ours = 0isize;
    let mut delta_theirs = 0isize;
    for cluster in clusters {
        let lo = cluster.iter().map(|(_, h)| h.base_lo).min().unwrap_or(0);
        let hi = cluster
            .iter()
            .map(|(_, h)| h.base_hi)
            .max()
            .unwrap_or(lo)
            .max(lo);
        result.extend(base[base_pos..lo].iter().cloned());
        let ours_in: Vec<&Hunk> = cluster
            .iter()
            .filter(|(s, _)| *s == Side::Ours)
            .map(|(_, h)| h)
            .collect();
        let theirs_in: Vec<&Hunk> = cluster
            .iter()
            .filter(|(s, _)| *s == Side::Theirs)
            .map(|(_, h)| h)
            .collect();
        let both = !ours_in.is_empty() && !theirs_in.is_empty();
        if both {
            // Conflict: Result holds the base slice until the user acts.
            let ours_slice = apply_hunks(base, ours, lo, hi, &ours_in);
            let theirs_slice = apply_hunks(base, theirs, lo, hi, &theirs_in);
            conflicts.push(MergeConflict {
                base: base[lo..hi].to_vec(),
                ours: ours_slice,
                theirs: theirs_slice,
                base_start: lo,
                ours_start: (lo as isize + delta_ours).max(0) as usize,
                theirs_start: (lo as isize + delta_theirs).max(0) as usize,
                result_start: result.len(),
                result_len: hi - lo,
                state: ConflictState::Unresolved,
            });
            result.extend(base[lo..hi].iter().cloned());
        } else {
            // One-sided cluster: auto-resolve into the Result.
            let (derived, hunks_in) = if theirs_in.is_empty() {
                (ours, &ours_in)
            } else {
                (theirs, &theirs_in)
            };
            result.extend(apply_hunks(base, derived, lo, hi, hunks_in));
        }
        for h in &ours_in {
            delta_ours += (h.derived_hi - h.derived_lo) as isize - (h.base_hi - h.base_lo) as isize;
        }
        for h in &theirs_in {
            delta_theirs +=
                (h.derived_hi - h.derived_lo) as isize - (h.base_hi - h.base_lo) as isize;
        }
        base_pos = hi;
    }
    result.extend(base[base_pos..].iter().cloned());
    (result, conflicts)
}

/// Apply `hunks_in` (sorted, all within base `[lo, hi)`) onto that base
/// slice, yielding the derived side's text for the region.
fn apply_hunks(
    base: &[String],
    derived: &[String],
    lo: usize,
    hi: usize,
    hunks_in: &[&Hunk],
) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = lo;
    let mut sorted: Vec<&&Hunk> = hunks_in.iter().collect();
    sorted.sort_by_key(|h| h.base_lo);
    for h in sorted {
        out.extend(base[pos..h.base_lo.max(pos)].iter().cloned());
        out.extend(derived[h.derived_lo..h.derived_hi].iter().cloned());
        pos = h.base_hi.max(pos);
    }
    out.extend(base[pos..hi.max(pos)].iter().cloned());
    out
}

/// Synthesize (base, ours, theirs) from a marker-filled buffer, so the
/// merge editor also works on a plain conflicted file outside any git
/// merge. Context outside blocks is shared verbatim; inside a block each
/// side gets its own section, and base gets the diff3 `|||||||` section
/// when present (nothing otherwise, so both sides differ from base and
/// the block stays a conflict). Returns None when there are no blocks.
pub fn synthesize_from_markers(
    lines: &[String],
) -> Option<(Vec<String>, Vec<String>, Vec<String>)> {
    let blocks = crate::merge::find_conflicts(lines);
    if blocks.is_empty() {
        return None;
    }
    let mut base = Vec::new();
    let mut ours = Vec::new();
    let mut theirs = Vec::new();
    let mut pos = 0usize;
    for b in &blocks {
        for l in &lines[pos..b.ours_start] {
            base.push(l.clone());
            ours.push(l.clone());
            theirs.push(l.clone());
        }
        let ours_end = b.base_start.unwrap_or(b.sep);
        ours.extend(lines[b.ours_start + 1..ours_end].iter().cloned());
        if let Some(bs) = b.base_start {
            base.extend(lines[bs + 1..b.sep].iter().cloned());
        }
        theirs.extend(lines[b.sep + 1..b.theirs_end].iter().cloned());
        pos = b.theirs_end + 1;
    }
    for l in &lines[pos..] {
        base.push(l.clone());
        ours.push(l.clone());
        theirs.push(l.clone());
    }
    Some((base, ours, theirs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn non_overlapping_hunks_auto_resolve_without_a_conflict() {
        let base = lines(&["a", "b", "c", "d", "e"]);
        let ours = lines(&["A", "b", "c", "d", "e"]); // changed line 0
        let theirs = lines(&["a", "b", "c", "d", "E"]); // changed line 4
        let (view, result) = MergeView::new(base, ours, theirs, false);
        assert!(view.conflicts.is_empty(), "distant edits merge cleanly");
        assert_eq!(result, lines(&["A", "b", "c", "d", "E"]));
    }

    #[test]
    fn overlapping_hunks_become_one_conflict_holding_base() {
        let base = lines(&["a", "b", "c"]);
        let ours = lines(&["a", "OURS", "c"]);
        let theirs = lines(&["a", "THEIRS", "c"]);
        let (view, result) = MergeView::new(base, ours, theirs, false);
        assert_eq!(view.conflicts.len(), 1);
        let c = &view.conflicts[0];
        assert_eq!(c.base, lines(&["b"]));
        assert_eq!(c.ours, lines(&["OURS"]));
        assert_eq!(c.theirs, lines(&["THEIRS"]));
        assert_eq!((c.result_start, c.result_len), (1, 1));
        assert_eq!(result, lines(&["a", "b", "c"]), "conflict keeps base");
    }

    #[test]
    fn same_point_insertions_conflict_but_adjacent_hunks_do_not() {
        let base = lines(&["a", "b"]);
        let ours = lines(&["a", "X", "b"]);
        let theirs = lines(&["a", "Y", "b"]);
        let (view, _) = MergeView::new(base.clone(), ours, theirs, false);
        assert_eq!(view.conflicts.len(), 1, "double insert at one point");

        // Ours edits row 0, theirs edits row 1: adjacent, independent.
        let ours2 = lines(&["A", "b"]);
        let theirs2 = lines(&["a", "B"]);
        let (view2, result2) = MergeView::new(base, ours2, theirs2, false);
        assert!(view2.conflicts.is_empty());
        assert_eq!(result2, lines(&["A", "B"]));
    }

    #[test]
    fn replacement_covers_every_state() {
        let base = lines(&["a", "mid", "z"]);
        let ours = lines(&["a", "one", "z"]);
        let theirs = lines(&["a", "two", "z"]);
        let (view, _) = MergeView::new(base, ours, theirs, false);
        let c = &view.conflicts[0];
        assert_eq!(c.replacement(ConflictState::Current), lines(&["one"]));
        assert_eq!(c.replacement(ConflictState::Incoming), lines(&["two"]));
        assert_eq!(c.replacement(ConflictState::Both), lines(&["one", "two"]));
        assert_eq!(
            c.replacement(ConflictState::BothReverse),
            lines(&["two", "one"])
        );
        assert_eq!(c.replacement(ConflictState::Base), lines(&["mid"]));
    }

    #[test]
    fn note_accept_shifts_later_regions_by_the_delta() {
        let base = lines(&["a", "x", "b", "c", "y", "d"]);
        let ours = lines(&["a", "O1", "O2", "b", "c", "OY", "d"]);
        let theirs = lines(&["a", "T", "b", "c", "TY", "d"]);
        let (mut view, result) = MergeView::new(base, ours, theirs, false);
        assert_eq!(view.conflicts.len(), 2);
        let second_before = view.conflicts[1].result_start;
        // Accept ours (2 lines) over base (1 line): +1 delta.
        view.note_accept(0, 2, ConflictState::Current, result.len() + 1, 1);
        assert_eq!(view.conflicts[0].state, ConflictState::Current);
        assert_eq!(view.conflicts[1].result_start, second_before + 1);
        assert_eq!(view.resolved_count(), 1);
        assert_eq!(view.unresolved_count(), 1);
    }

    #[test]
    fn sync_with_buffer_marks_the_edited_region_manual_and_shifts_the_rest() {
        let base = lines(&["a", "x", "b", "c", "y", "d"]);
        let ours = lines(&["a", "OX", "b", "c", "OY", "d"]);
        let theirs = lines(&["a", "TX", "b", "c", "TY", "d"]);
        let (mut view, result) = MergeView::new(base, ours, theirs, false);
        assert_eq!(view.conflicts.len(), 2);
        let first = view.conflicts[0].result_start;
        let second_before = view.conflicts[1].result_start;
        // Simulate the user pressing Enter inside region 0: +1 line.
        view.sync_with_buffer(result.len() + 1, 1, first);
        assert_eq!(view.conflicts[0].state, ConflictState::Manual);
        assert_eq!(view.conflicts[0].result_len, 2);
        assert_eq!(view.conflicts[1].result_start, second_before + 1);
        // An edit outside every region resolves nothing further.
        view.sync_with_buffer(result.len() + 1, 2, 0);
        assert_eq!(view.resolved_count(), 1);
    }

    #[test]
    fn next_conflict_wraps_in_both_directions() {
        let base = lines(&["a", "x", "b", "c", "y", "d"]);
        let ours = lines(&["a", "OX", "b", "c", "OY", "d"]);
        let theirs = lines(&["a", "TX", "b", "c", "TY", "d"]);
        let (view, _) = MergeView::new(base, ours, theirs, false);
        let starts: Vec<usize> = view.conflicts.iter().map(|c| c.result_start).collect();
        assert_eq!(
            view.next_conflict(0, false),
            (starts[0] > 0).then_some(0).or(Some(1))
        );
        assert_eq!(
            view.next_conflict(starts[1], false),
            Some(0),
            "wraps forward"
        );
        assert_eq!(
            view.next_conflict(starts[0], true),
            Some(1),
            "wraps backward"
        );
    }

    #[test]
    fn synthesize_from_markers_reconstructs_all_three_sides() {
        let doc = lines(&[
            "top",
            "<<<<<<< HEAD",
            "ours line",
            "||||||| merged common ancestors",
            "base line",
            "=======",
            "theirs line",
            ">>>>>>> feature",
            "bottom",
        ]);
        let (base, ours, theirs) = synthesize_from_markers(&doc).unwrap();
        assert_eq!(base, lines(&["top", "base line", "bottom"]));
        assert_eq!(ours, lines(&["top", "ours line", "bottom"]));
        assert_eq!(theirs, lines(&["top", "theirs line", "bottom"]));
        // And the resulting three-way sees exactly one conflict.
        let (view, _) = MergeView::new(base, ours, theirs, true);
        assert_eq!(view.conflicts.len(), 1);
        assert!(view.from_markers);
    }

    #[test]
    fn synthesize_without_a_diff3_section_leaves_base_empty_for_the_block() {
        let doc = lines(&["<<<<<<< HEAD", "mine", "=======", "yours", ">>>>>>> br"]);
        let (base, ours, theirs) = synthesize_from_markers(&doc).unwrap();
        assert!(base.is_empty());
        assert_eq!(ours, lines(&["mine"]));
        assert_eq!(theirs, lines(&["yours"]));
        let (view, result) = MergeView::new(base, ours, theirs, true);
        assert_eq!(view.conflicts.len(), 1);
        assert!(result.is_empty(), "conflict region starts as (empty) base");
    }

    #[test]
    fn a_marker_free_buffer_synthesizes_nothing() {
        assert!(synthesize_from_markers(&lines(&["plain", "text"])).is_none());
    }

    #[test]
    fn zero_length_region_still_claims_its_boundary_row() {
        let c = MergeConflict {
            base: vec![],
            ours: lines(&["x"]),
            theirs: lines(&["y"]),
            base_start: 0,
            ours_start: 0,
            theirs_start: 0,
            result_start: 3,
            result_len: 0,
            state: ConflictState::Unresolved,
        };
        assert!(c.contains_result_row(3));
        assert!(!c.contains_result_row(2));
        assert!(!c.contains_result_row(4));
    }
}
