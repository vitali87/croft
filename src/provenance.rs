//! Which seat wrote each line (#349).
//!
//! Git blame answers "which commit"; this answers "which seat" — you, the
//! navigator, an agent pane, a collab peer. The question it exists for is
//! "did I write this or did the model?", which no commit can answer because
//! a commit records the author of the save, not of the keystrokes.
//!
//! # The invariant that shapes everything here
//!
//! **A line croft did not observe being written is `Unknown`, never guessed.**
//! A provenance overlay that is right most of the time is worse than none: it
//! would be read as fact, and the one line it attributes wrongly is exactly
//! the line someone is arguing about. So every operation below either knows
//! which seat wrote a line or records that it does not, and there is no path
//! that infers a seat from a neighbouring line.
//!
//! That is also why this is a map from line to `Option<Seat>` rather than a
//! map that omits unknown lines: an absent entry and an unknown one must not
//! be the same thing to a caller, or "no record" silently reads as "not
//! attributed to anyone" in one place and "index out of range" in another.

use std::collections::BTreeMap;

/// Who wrote a line.
///
/// Cloned rather than interned: a buffer has a handful of distinct seats and
/// the map is per-buffer, so the sharing an interner would buy is not worth
/// the indirection at the point where the gutter paints.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Seat {
    /// The person at this croft.
    Me,
    /// The navigator's streamed suggestion, accepted into the buffer.
    Navigator,
    /// A coding agent, named by the pane it runs in.
    Agent(String),
    /// A collab participant, by display name.
    Peer(String),
    /// Text croft inserted on the user's behalf that no person or agent
    /// typed: an accepted LSP completion, an expanded snippet.
    ///
    /// A separate seat rather than `Me`, because "you wrote this" is a claim
    /// and a completion body is not one the user made — they chose it from a
    /// list, which is a different act from writing it. Attributing generated
    /// text to a person is the same class of wrong answer as inheriting a
    /// seat from a neighbouring line, and it is the shape most likely to be
    /// argued about later.
    Generated,
}

impl Seat {
    /// The label the inline blame annotation shows.
    pub fn label(&self) -> String {
        match self {
            Seat::Me => String::from("you"),
            Seat::Navigator => String::from("navigator"),
            Seat::Agent(pane) => format!("agent ({pane})"),
            Seat::Peer(name) => name.clone(),
            Seat::Generated => String::from("generated"),
        }
    }
}

/// Per-buffer line provenance.
///
/// Keyed on 0-based line index. A line with no entry is `Unknown` and stays
/// that way: see the module invariant.
#[derive(Clone, Debug, Default)]
pub struct Provenance {
    lines: BTreeMap<usize, Seat>,
}

impl Provenance {
    pub fn new() -> Self {
        Self::default()
    }

    /// The seat that wrote `line`, or `None` when croft did not see it
    /// written.
    pub fn seat(&self, line: usize) -> Option<&Seat> {
        self.lines.get(&line)
    }

    /// Record that `seat` wrote the lines in `range`.
    pub fn record(&mut self, range: std::ops::Range<usize>, seat: Seat) {
        for line in range {
            self.lines.insert(line, seat.clone());
        }
    }

    /// How many lines carry an attribution.
    pub fn attributed(&self) -> usize {
        self.lines.len()
    }

    /// Forget every attribution at or beyond `len`.
    ///
    /// The primitive every whole-buffer change needs: after a reload, an
    /// undo, or a re-decode, the map may hold keys for lines that no longer
    /// exist, and a key past the end is an attribution for a line croft
    /// never watched being written. Separate from `splice`, which shifts and
    /// never truncates.
    ///
    /// "No key is ever >= `lines.len()`" is the assertable form of this
    /// module's invariant, and this is what makes it hold.
    pub fn truncate(&mut self, len: usize) {
        self.lines.retain(|&line, _| line < len);
    }

    /// Shift the map for an edit that replaced `removed` lines starting at
    /// `at` with `added` new ones.
    ///
    /// The new lines are left UNATTRIBUTED rather than inheriting from
    /// whatever sat there: the caller records them against the seat that
    /// made the edit, and a caller that forgets leaves them unknown, which
    /// is the safe direction. Lines below the edit keep their seats and
    /// move; lines inside the removed range are dropped.
    pub fn splice(&mut self, at: usize, removed: usize, added: usize) {
        let mut next = BTreeMap::new();
        for (&line, seat) in &self.lines {
            if line < at {
                next.insert(line, seat.clone());
            } else if line >= at.saturating_add(removed) {
                // Below the edit: shifts by the net change. Saturating on
                // both sides so an absurd `at` cannot wrap the guard and turn
                // the shift into an attribution at ~2^64 — a release build
                // would wrap rather than panic, so the guard alone is not
                // enough to make the arithmetic safe.
                let shifted = (line + added).saturating_sub(removed);
                next.insert(shifted, seat.clone());
            }
            // Inside the removed range: the line is gone, and so is its
            // attribution. Nothing is carried onto the replacement.
        }
        self.lines = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant the whole feature rests on: a line croft did not watch
    /// being written is Unknown, and no operation invents a seat for it.
    ///
    /// Asserted as a CONTRAST — an attributed line beside an unattributed
    /// one — because "seat(9) is None" passes just as well against a map
    /// that attributes nothing at all.
    #[test]
    fn an_unobserved_line_is_never_attributed() {
        let mut p = Provenance::new();
        p.record(0..2, Seat::Me);
        assert_eq!(p.seat(0), Some(&Seat::Me), "staging: line 0 is attributed");
        assert_eq!(p.seat(1), Some(&Seat::Me));
        // The line after, and one far beyond the buffer, are both unknown —
        // not inherited from the neighbour above.
        assert_eq!(p.seat(2), None, "a line nobody wrote must stay unknown");
        assert_eq!(p.seat(9_999), None);
    }

    /// An edit moves the seats below it and drops the ones it replaced.
    ///
    /// The replacement lines are deliberately NOT given the removed lines'
    /// seats: the caller records them against whoever made the edit, and a
    /// caller that forgets leaves them unknown rather than wrong.
    #[test]
    fn an_edit_shifts_below_and_drops_what_it_replaced() {
        let mut p = Provenance::new();
        p.record(0..1, Seat::Me);
        p.record(1..2, Seat::Navigator);
        p.record(2..3, Seat::Agent(String::from("pane 2")));

        // Replace line 1 (the navigator's) with three lines.
        p.splice(1, 1, 3);
        assert_eq!(p.seat(0), Some(&Seat::Me), "above the edit is untouched");
        // The three new lines carry nothing until someone records them.
        for line in 1..4 {
            assert_eq!(p.seat(line), None, "line {line} was invented, not observed");
        }
        // The agent's line moved down by the net change (+2).
        assert_eq!(
            p.seat(4),
            Some(&Seat::Agent(String::from("pane 2"))),
            "a line below the edit keeps its seat and moves"
        );
        assert_eq!(p.attributed(), 2, "the replaced line's seat is gone");
    }

    /// A pure deletion pulls the lines below it up.
    #[test]
    fn a_deletion_pulls_the_seats_below_it_up() {
        let mut p = Provenance::new();
        p.record(0..1, Seat::Me);
        p.record(3..4, Seat::Peer(String::from("ada")));
        p.splice(1, 2, 0);
        assert_eq!(p.seat(0), Some(&Seat::Me));
        assert_eq!(
            p.seat(1),
            Some(&Seat::Peer(String::from("ada"))),
            "the peer's line moved up into the gap"
        );
        assert_eq!(p.seat(3), None, "nothing left behind at the old index");
    }

    /// The shift arithmetic cannot underflow, swept rather than argued.
    ///
    /// `line + added - removed` is a usize expression, so an underflow would
    /// panic in debug and wrap in release — the second being the dangerous
    /// one, since a wrapped index would attribute a line at some absurd
    /// offset instead of crashing. The guard `line >= at + removed` is what
    /// makes it safe, and this pins the two together so neither can be
    /// changed without the other.
    #[test]
    fn the_shift_never_underflows_for_any_edit_shape() {
        for at in 0..6usize {
            for removed in 0..6usize {
                for added in 0..6usize {
                    let mut p = Provenance::new();
                    // Attribute every line in a window wider than any edit.
                    p.record(0..12, Seat::Me);
                    p.splice(at, removed, added);
                    // Every surviving key is a plausible line index, and the
                    // count is exactly what the edit implies.
                    let survivors = p.attributed();
                    assert_eq!(
                        survivors,
                        12 - removed.min(12 - at.min(12)),
                        "at={at} removed={removed} added={added}"
                    );
                    for line in 0..40 {
                        // Reading any line must not panic, and no seat may
                        // appear at an index the edit could not produce.
                        if p.seat(line).is_some() {
                            assert!(
                                line < 12 + added,
                                "seat at {line} after at={at} removed={removed} added={added}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Truncation drops exactly the keys past the end and nothing else.
    ///
    /// The assertable form of this module's invariant is "no attributed key
    /// is ever >= the buffer's line count", and this is the primitive that
    /// makes it hold after a whole-buffer swap.
    #[test]
    fn truncation_drops_the_keys_past_the_end() {
        let mut p = Provenance::new();
        p.record(0..5, Seat::Me);
        p.truncate(3);
        assert_eq!(p.attributed(), 3, "kept exactly the lines that remain");
        assert_eq!(p.seat(2), Some(&Seat::Me));
        assert_eq!(p.seat(3), None, "a line past the end must be forgotten");
        // Truncating to zero forgets everything, which is what a buffer
        // replaced wholesale needs.
        p.truncate(0);
        assert_eq!(p.attributed(), 0);
        // Truncating above the highest key is a no-op rather than an error.
        p.record(0..2, Seat::Navigator);
        p.truncate(99);
        assert_eq!(p.attributed(), 2);
    }

    /// The labels are what the inline annotation shows, so they are part of
    /// the interface rather than a debug rendering.
    #[test]
    fn a_seat_labels_itself_for_the_annotation() {
        assert_eq!(Seat::Me.label(), "you");
        assert_eq!(Seat::Navigator.label(), "navigator");
        assert_eq!(
            Seat::Agent(String::from("pane 2")).label(),
            "agent (pane 2)"
        );
        assert_eq!(Seat::Peer(String::from("ada")).label(), "ada");
        assert_eq!(Seat::Generated.label(), "generated");
    }
}
