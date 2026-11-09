use crossterm::event::KeyModifiers;
use std::time::{Duration, Instant};

const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);

/// How far a run of plain clicks at one cell has got (#1570).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Run {
    /// One click, which the next can make a double.
    Single,
    /// A double the editor kept, which the next click can make a triple.
    Double,
}

#[derive(Default)]
pub struct ClickTracker {
    /// The last plain click and how far its run has got.
    last: Option<(Instant, u16, u16, Run)>,
}

impl ClickTracker {
    /// The run this click would continue, or `None` when it starts a new
    /// one: a modified click, another cell, or too late.
    fn run_before(&self, now: Instant, col: u16, row: u16, mods: KeyModifiers) -> Option<Run> {
        match self.last {
            Some((t, x, y, run))
                if mods.is_empty()
                    && row == y
                    && col.abs_diff(x) <= 1
                    && now.duration_since(t) <= DOUBLE_CLICK_WINDOW =>
            {
                Some(run)
            }
            _ => None,
        }
    }

    /// True when this click completes a double-click at (near enough) the
    /// same cell, inside the window.
    ///
    /// A click carrying modifiers is never half of a double (#317). The two
    /// halves have to be ONE gesture, and ctrl / alt / shift + click is a
    /// different gesture with its own meaning: open a link, add a caret,
    /// extend a selection. The modifiers are taken here rather than tested at
    /// the call sites so that every site - including ones added later - is
    /// forced by the compiler to answer the question. A site that silently
    /// did not is exactly what armed the tracker from a ctrl+click that
    /// matched no link, so the user's next ordinary click selected a word
    /// they never double-clicked.
    pub fn is_double(&self, now: Instant, col: u16, row: u16, mods: KeyModifiers) -> bool {
        self.run_before(now, col, row, mods) == Some(Run::Single)
    }

    /// True when this click is the third of a run: it follows a double that
    /// [`record_double`](Self::record_double) kept, at the same cell and
    /// inside the window (#1570). Only the editor keeps one, for its
    /// whole-line selection; every other tracker clears after a double.
    pub fn is_triple(&self, now: Instant, col: u16, row: u16, mods: KeyModifiers) -> bool {
        self.run_before(now, col, row, mods) == Some(Run::Double)
    }

    /// Arm the tracker with this click as a possible first half of a double.
    /// A modified click CLEARS instead of arming, for the same reason
    /// [`is_double`](Self::is_double) refuses one: it can be neither half of
    /// a pair. Clearing rather than merely skipping the arm also stops a
    /// modified click from sitting harmlessly between two plain ones and
    /// letting them pair across it.
    pub fn record(&mut self, now: Instant, col: u16, row: u16, mods: KeyModifiers) {
        self.last = mods.is_empty().then_some((now, col, row, Run::Single));
    }

    /// Keep this click as the second half of a double, so a third plain
    /// click here inside the window is a triple (#1570). Clearing instead,
    /// as a double used to, made the third click a fresh single that
    /// dropped the word the double had just selected.
    pub fn record_double(&mut self, now: Instant, col: u16, row: u16) {
        self.last = Some((now, col, row, Run::Double));
    }

    pub fn clear(&mut self) {
        self.last = None;
    }

    pub fn clear_if_moved(&mut self, col: u16, row: u16) {
        if let Some((_, x, y, _)) = self.last
            && (col != x || row != y)
        {
            self.last = None;
        }
    }
}

/// Double-click tracking for MODIFIED clicks, kept apart from [`ClickTracker`].
///
/// A modified click must never be recorded into the built-in trackers: those
/// are read by the editor's plain select-word (and the tree's open-file), none
/// of which consult modifiers, so arming them with a ctrl+click would make the
/// user's next ORDINARY click select a word they never asked for.
///
/// But `double_click` is classified by asking a tracker whether it just saw a
/// click at this cell — so with nothing recording modified clicks, a
/// `ctrl+double_click` binding could never fire at all. This is that missing
/// record, in a tracker only the binding dispatch reads.
///
/// The modifier set is part of the match: ctrl+click followed by alt+click is
/// two different gestures, not a double of either.
#[derive(Default)]
pub struct ModifiedClickTracker {
    last: Option<(Instant, u16, u16, crossterm::event::KeyModifiers)>,
}

impl ModifiedClickTracker {
    pub fn is_double(
        &self,
        now: Instant,
        col: u16,
        row: u16,
        mods: crossterm::event::KeyModifiers,
    ) -> bool {
        matches!(
            self.last,
            Some((t, x, y, m))
                if m == mods
                    && row == y
                    && col.abs_diff(x) <= 1
                    && now.duration_since(t) <= DOUBLE_CLICK_WINDOW
        )
    }

    pub fn record(
        &mut self,
        now: Instant,
        col: u16,
        row: u16,
        mods: crossterm::event::KeyModifiers,
    ) {
        self.last = Some((now, col, row, mods));
    }

    pub fn clear(&mut self) {
        self.last = None;
    }

    /// Cancel a pending pair when the pointer drags away, exactly as
    /// [`ClickTracker::clear_if_moved`] does for the built-ins.
    ///
    /// Without it, click → drag away → click back counted as a double for
    /// modified gestures where the identical unmodified sequence correctly
    /// did not: a drag is a deliberate gesture of its own, and the click that
    /// began it is spent, not the first half of a pair.
    pub fn clear_if_moved(&mut self, col: u16, row: u16) {
        if let Some((_, x, y, _)) = self.last
            && (col != x || row != y)
        {
            self.last = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #317: the two halves of a double-click must be ONE gesture. A modified
    // click is a different gesture (open a link, add a caret, extend a
    // selection), so it can be neither half: it must not COMPLETE a double,
    // and it must not ARM one for the next ordinary click - which is how a
    // ctrl+click that matched no link made the user's following plain click
    // select a word they never double-clicked.
    #[test]
    fn a_modified_click_is_neither_half_of_a_double() {
        let now = Instant::now();
        let plain = KeyModifiers::NONE;

        // The baseline the fix must not break: two plain clicks pair.
        let mut t = ClickTracker::default();
        t.record(now, 10, 3, plain);
        assert!(t.is_double(now, 10, 3, plain), "two plain clicks must pair");

        // A modified click cannot COMPLETE a double armed by a plain one.
        for mods in [
            KeyModifiers::CONTROL,
            KeyModifiers::SUPER,
            KeyModifiers::ALT,
            KeyModifiers::SHIFT,
        ] {
            let mut t = ClickTracker::default();
            t.record(now, 10, 3, plain);
            assert!(
                !t.is_double(now, 10, 3, mods),
                "{mods:?}+click must not classify as a double-click"
            );
        }

        // And a modified click cannot ARM one for the next plain click. This
        // is the reported defect: the ctrl+click fell through the link path
        // and armed the tracker, so the NEXT ordinary click word-selected.
        for mods in [
            KeyModifiers::CONTROL,
            KeyModifiers::SUPER,
            KeyModifiers::ALT,
            KeyModifiers::SHIFT,
        ] {
            let mut t = ClickTracker::default();
            t.record(now, 10, 3, mods);
            assert!(
                !t.is_double(now, 10, 3, plain),
                "a plain click after a {mods:?}+click must not be a double"
            );
        }
    }

    // A modified click does not merely fail to arm: it clears. Otherwise it
    // would sit harmlessly between two plain clicks and let them pair across
    // it, which is the same word-selection-nobody-asked-for by a longer route.
    #[test]
    fn a_modified_click_breaks_a_pair_it_sits_between() {
        let now = Instant::now();
        let mut t = ClickTracker::default();
        t.record(now, 10, 3, KeyModifiers::NONE);
        t.record(now, 10, 3, KeyModifiers::CONTROL);
        assert!(
            !t.is_double(now, 10, 3, KeyModifiers::NONE),
            "a modified click between two plain ones must break the pair, not be invisible"
        );
    }

    // The window and the cell tolerance are unchanged by the modifier work.
    #[test]
    fn pairing_still_needs_the_same_cell_inside_the_window() {
        let now = Instant::now();
        let plain = KeyModifiers::NONE;
        let mut t = ClickTracker::default();
        t.record(now, 10, 3, plain);
        assert!(
            t.is_double(now, 11, 3, plain),
            "one cell of drift still pairs"
        );
        assert!(
            !t.is_double(now, 13, 3, plain),
            "a distant cell does not pair"
        );
        assert!(!t.is_double(now, 10, 4, plain), "another row does not pair");
        assert!(
            !t.is_double(
                now + DOUBLE_CLICK_WINDOW + Duration::from_millis(1),
                10,
                3,
                plain
            ),
            "past the window does not pair"
        );
    }

    // #1570: a third plain click at the same cell inside the window after a
    // double is a triple, not a fresh single and not another double; a
    // fourth starts over once the triple has cleared the run.
    #[test]
    fn a_third_click_after_a_kept_double_is_a_triple() {
        let now = Instant::now();
        let plain = KeyModifiers::NONE;
        let mut t = ClickTracker::default();
        t.record(now, 10, 3, plain);
        assert!(t.is_double(now, 10, 3, plain) && !t.is_triple(now, 10, 3, plain));
        t.record_double(now, 10, 3);
        assert!(t.is_triple(now, 10, 3, plain), "three clicks are a triple");
        assert!(!t.is_double(now, 10, 3, plain), "and not a second double");
        assert!(!t.is_triple(now, 10, 3, KeyModifiers::SHIFT));
        assert!(!t.is_triple(now, 10, 4, plain), "another row starts over");
        let late = now + DOUBLE_CLICK_WINDOW + Duration::from_millis(1);
        assert!(!t.is_triple(late, 10, 3, plain), "too late starts over");
        t.clear();
        assert!(!t.is_double(now, 10, 3, plain) && !t.is_triple(now, 10, 3, plain));
    }
}
