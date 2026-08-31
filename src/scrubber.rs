//! Moving through a branch's history (#371).
//!
//! TIMELINE lists a file's history and the COMMITS graph lists the repo's;
//! neither lets you *move* through time. This is the cursor that does: it
//! sits at a commit, steps between them, and knows how to get back.
//!
//! # The invariant
//!
//! **Leaving the scrubber must restore the live buffer exactly, including
//! unsaved changes.** Scrubbing is a way of looking, not of editing, and a
//! feature that loses a user's uncommitted work to answer a question about
//! history would be worse than not having the feature. So the working tree
//! is a POSITION in the cursor rather than a thing the scrubber replaces:
//! there is no state in which the live buffer has been discarded and the
//! scrubber is responsible for putting it back.
//!
//! That is why [`Position::Working`] is a variant rather than `Option::None`
//! over a commit index. An `Option` invites "no commit selected" and "back at
//! the working tree" to be the same value, and they are not — the second is
//! where the user started and must be reachable from anywhere.

/// Where the scrubber is looking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    /// The live buffer, with whatever unsaved edits it holds. The scrubber
    /// starts here and `Home` returns here.
    Working,
    /// A commit, by index into the loaded list — 0 is the newest.
    At(usize),
}

/// The scrubber's cursor over a branch's commits.
#[derive(Clone, Debug)]
pub struct Scrubber {
    /// The branch's first-parent history, newest first, so index 0 is HEAD.
    ///
    /// Whole `GraphCommit`s rather than hashes: the slider needs `short_hash`
    /// for its labels and `parents` for the per-commit gutter delta, and
    /// re-fetching them once the widget exists would mean two sources for
    /// one list.
    commits: Vec<crate::git::GraphCommit>,
    position: Position,
}

impl Scrubber {
    /// A scrubber over `commits` (newest first), parked at the working tree.
    pub fn new(commits: Vec<crate::git::GraphCommit>) -> Self {
        Self {
            commits,
            position: Position::Working,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn position(&self) -> Position {
        self.position
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    /// The commit the cursor is on, or `None` at the working tree.
    ///
    /// The whole record, so a caller can label the slider with git's own
    /// `short_hash` rather than byte-slicing the full one — that slice is
    /// safe only while the string is ASCII hex, and the width git shows is
    /// per-repo rather than always seven.
    pub fn commit(&self) -> Option<&crate::git::GraphCommit> {
        match self.position {
            Position::Working => None,
            Position::At(i) => self.commits.get(i),
        }
    }

    /// Step one commit toward the PAST.
    ///
    /// From the working tree that is HEAD (index 0), not index 1: the
    /// working tree and HEAD are different views — one has your unsaved
    /// edits — so stepping back from the tree must show HEAD rather than
    /// skipping it.
    pub fn older(&mut self) {
        if self.commits.is_empty() {
            return;
        }
        self.position = match self.position {
            Position::Working => Position::At(0),
            Position::At(i) if i + 1 < self.commits.len() => Position::At(i + 1),
            // Already at the oldest loaded commit: stay rather than wrap.
            // Wrapping to the present would look like the drag "slipped".
            other => other,
        };
    }

    /// Step one commit toward the PRESENT, arriving at the working tree from
    /// HEAD.
    pub fn newer(&mut self) {
        self.position = match self.position {
            Position::Working => Position::Working,
            Position::At(0) => Position::Working,
            Position::At(i) => Position::At(i - 1),
        };
    }

    /// Jump to the working tree — `Home`, and the only way back that a
    /// caller ever needs.
    pub fn home(&mut self) {
        self.position = Position::Working;
    }

    /// Jump to a commit by index, clamped to what is loaded.
    ///
    /// Not reached from the keyboard — this is what a DRAG on the slider
    /// calls, and the slider widget is the next layer of #371. Kept here
    /// with its tests because the clamping rule is the part with a right
    /// answer, and deciding it alongside the painting would bury it.
    ///
    /// Clamped rather than refused because this is what a DRAG calls: a
    /// pointer beyond the last commit means "as far as it goes", and
    /// refusing would make the slider stick short of its own end.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn seek(&mut self, index: usize) {
        if self.commits.is_empty() {
            self.position = Position::Working;
            return;
        }
        self.position = Position::At(index.min(self.commits.len() - 1));
    }

    /// Where the slider's handle sits, as a fraction from 0.0 (oldest loaded)
    /// to 1.0 (the working tree).
    ///
    /// Same as [`Self::seek`]: the widget that reads this does not exist
    /// yet.
    ///
    /// The working tree is its own stop at the far right rather than sharing
    /// HEAD's position, because they are different views and a slider that
    /// showed them at the same place would give the user no way to tell
    /// which one they are looking at.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn fraction(&self) -> f32 {
        let stops = self.commits.len();
        if stops == 0 {
            return 1.0;
        }
        match self.position {
            Position::Working => 1.0,
            Position::At(i) => {
                // `stops` intervals between `stops + 1` positions.
                (stops - i - 1) as f32 / stops as f32
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(i: usize) -> crate::git::GraphCommit {
        crate::git::GraphCommit {
            hash: format!("c{i}"),
            short_hash: format!("c{i}"),
            parents: Vec::new(),
            refs: Vec::new(),
            summary: format!("commit {i}"),
            author: String::from("t"),
            age_secs: 0,
        }
    }

    fn scrubber(n: usize) -> Scrubber {
        Scrubber::new((0..n).map(commit).collect())
    }

    /// The working tree is always reachable, from every position and by
    /// stepping as well as by `Home`.
    ///
    /// This is the module's invariant in its testable form: the live buffer
    /// with its unsaved edits is a POSITION, so there is no state the
    /// scrubber can reach from which the user cannot get back to what they
    /// were editing.
    #[test]
    fn the_working_tree_is_reachable_from_everywhere() {
        let mut s = scrubber(5);
        assert_eq!(s.position(), Position::Working, "starts at the live buffer");

        // From the oldest commit, by Home.
        s.seek(4);
        assert_eq!(s.position(), Position::At(4));
        s.home();
        assert_eq!(s.position(), Position::Working);

        // And by stepping forward, which must ARRIVE rather than stop at
        // HEAD — a scrubber that stranded the user one step short of their
        // own edits would be the invariant failing quietly.
        s.seek(3);
        for _ in 0..4 {
            s.newer();
        }
        assert_eq!(
            s.position(),
            Position::Working,
            "stepping forward from any commit reaches the live buffer"
        );

        // Already there: stepping forward again is a no-op, not a wrap.
        s.newer();
        assert_eq!(s.position(), Position::Working);
    }

    /// Stepping back from the working tree lands on HEAD, not past it.
    ///
    /// The working tree and HEAD are different views — one carries unsaved
    /// edits — so skipping HEAD would hide the commit the user most wants to
    /// compare against.
    #[test]
    fn stepping_back_from_the_tree_shows_head_first() {
        let mut s = scrubber(3);
        s.older();
        assert_eq!(
            s.position(),
            Position::At(0),
            "HEAD, not the commit below it"
        );
        assert_eq!(s.commit().map(|c| c.hash.as_str()), Some("c0"));
        s.older();
        assert_eq!(s.position(), Position::At(1));
    }

    /// The far end stops rather than wrapping.
    ///
    /// A drag that ran off the oldest commit and reappeared at the present
    /// would read as the slider slipping, and the user would not know which
    /// end they were at.
    #[test]
    fn the_oldest_commit_is_a_wall_not_a_wrap() {
        let mut s = scrubber(3);
        for _ in 0..10 {
            s.older();
        }
        assert_eq!(s.position(), Position::At(2), "stopped at the oldest");
        assert_eq!(s.commit().map(|c| c.hash.as_str()), Some("c2"));
    }

    /// A drag beyond the end clamps, because a pointer past the last commit
    /// means "as far as it goes".
    #[test]
    fn a_seek_past_the_end_clamps_to_the_oldest() {
        let mut s = scrubber(3);
        s.seek(99);
        assert_eq!(s.position(), Position::At(2));
        assert_eq!(s.commit().map(|c| c.hash.as_str()), Some("c2"));
    }

    /// An empty history has nowhere to go, and says so by staying put.
    ///
    /// A repo with no commits is ordinary — a fresh `git init` — so this is
    /// the empty state, not an error case.
    #[test]
    fn an_empty_history_stays_at_the_working_tree() {
        let mut s = scrubber(0);
        assert!(s.is_empty());
        s.older();
        assert_eq!(s.position(), Position::Working, "nothing to step back to");
        s.seek(3);
        assert_eq!(s.position(), Position::Working);
        assert_eq!(s.commit().map(|c| c.hash.as_str()), None);
        assert_eq!(s.fraction(), 1.0);
    }

    /// The handle's position is monotonic across every stop, and the working
    /// tree has its own place at the far right.
    ///
    /// Asserted as an ordering over the whole range rather than at a few
    /// points: a formula that is right at the ends and wrong in the middle
    /// would pass spot checks, and the middle is where a drag spends its
    /// time.
    #[test]
    fn the_handle_moves_monotonically_from_oldest_to_the_tree() {
        let mut s = scrubber(4);
        let mut seen = Vec::new();
        for i in (0..4).rev() {
            s.seek(i);
            seen.push(s.fraction());
        }
        s.home();
        seen.push(s.fraction());

        assert_eq!(
            seen.first().copied(),
            Some(0.0),
            "oldest sits at the left edge"
        );
        assert_eq!(
            seen.last().copied(),
            Some(1.0),
            "the tree sits at the right"
        );
        for pair in seen.windows(2) {
            assert!(pair[1] > pair[0], "the handle went backwards: {seen:?}");
        }
        // HEAD and the working tree are DIFFERENT stops, so the user can see
        // which of the two they are looking at.
        s.seek(0);
        assert!(s.fraction() < 1.0, "HEAD must not sit on top of the tree");
    }
}
