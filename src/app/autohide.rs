//! Auto-hiding side bar (#260): collapse the panel when focus leaves it, so
//! the editor gets the columns back.
//!
//! The collapse is deliberately *delayed*. A click that passes through the
//! sidebar on its way to the editor, or a command that briefly focuses the
//! editor before returning, would otherwise make the panel flap open and
//! shut. A grace window means the sidebar only goes away once focus has
//! genuinely settled elsewhere.
//!
//! `now` is a parameter rather than read from the clock inside, following
//! [`crate::app::hover::HoverDwell`] — it is the only timing primitive in
//! croft that can be tested without sleeping, and a flap bug is exactly the
//! kind of thing that needs deterministic tests.

use std::time::{Duration, Instant};

/// How long focus must stay away from the side bar before it collapses.
/// Long enough that a click passing through does not trigger it, short
/// enough that the columns come back promptly.
pub const AUTO_HIDE_DELAY: Duration = Duration::from_millis(400);

/// Why the side bar is being kept open despite focus being elsewhere.
///
/// Commands that deliberately reveal the panel (reveal-in-explorer, opening
/// a sidebar view) need no variant here: they focus the tree, and the
/// `focus_pane` hook disarms the timer for them.
/// Reported so a user who wonders why auto-hide "stopped working" can be
/// told, rather than guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suppressed {
    /// A seam drag is in progress; collapsing mid-drag would yank the
    /// splitter out from under the pointer.
    Dragging,
    /// A modal is borrowing the screen. Collapsing behind it would make the
    /// layout jump when it closes.
    Modal,
}

/// The pending-collapse timer.
///
/// `armed` holds the instant focus left the side bar. It is cleared the
/// moment focus returns, so a round trip never collapses.
#[derive(Debug, Default)]
pub struct AutoHide {
    armed: Option<Instant>,
    suppressed: Option<Suppressed>,
}

impl AutoHide {
    /// Focus is somewhere other than the side bar. Starts the grace window
    /// if it is not already running — re-arming on every frame would push
    /// the deadline forever and the panel would never collapse.
    pub fn focus_left(&mut self, now: Instant) {
        if self.armed.is_none() {
            self.armed = Some(now);
        }
    }

    /// Focus is on the side bar (or it was deliberately revealed): cancel
    /// any pending collapse.
    pub fn focus_returned(&mut self) {
        self.armed = None;
    }

    /// Hold the panel open for a reason, and cancel the pending collapse.
    pub fn suppress(&mut self, why: Suppressed) {
        self.suppressed = Some(why);
        self.armed = None;
    }

    /// Release a hold. The caller re-arms on the next frame if focus is
    /// still away, so releasing does not itself collapse anything.
    pub fn release(&mut self) {
        self.suppressed = None;
    }

    /// Test-only seam: assertions read the hold's reason so a suppression
    /// test pins WHY the panel stayed open, not merely that it did.
    #[cfg(test)]
    pub fn suppressed(&self) -> Option<Suppressed> {
        self.suppressed
    }

    /// When the grace window started, if one is running. Lets a test assert
    /// that focus movement armed the timer, and drive the deadline from it,
    /// without waiting for it to fire.
    #[cfg(test)]
    pub fn armed_at(&self) -> Option<Instant> {
        self.armed
    }

    /// True once the grace window has elapsed with focus still away and
    /// nothing holding the panel open. Consuming it is the caller's job:
    /// [`Self::focus_returned`] after collapsing, so it fires once.
    pub fn due(&self, now: Instant, threshold: Duration) -> bool {
        self.suppressed.is_none()
            && matches!(self.armed, Some(a) if now.duration_since(a) >= threshold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn the_grace_window_must_elapse_before_a_collapse_is_due() {
        let start = t0();
        let mut a = AutoHide::default();
        assert!(!a.due(start, AUTO_HIDE_DELAY), "nothing armed, nothing due");

        a.focus_left(start);
        assert!(
            !a.due(start + Duration::from_millis(399), AUTO_HIDE_DELAY),
            "a click passing through must not collapse the panel"
        );
        assert!(
            a.due(start + Duration::from_millis(400), AUTO_HIDE_DELAY),
            "but settled focus does"
        );
    }

    #[test]
    fn focus_returning_cancels_a_pending_collapse() {
        let start = t0();
        let mut a = AutoHide::default();
        a.focus_left(start);
        a.focus_returned();
        assert!(
            !a.due(start + Duration::from_secs(10), AUTO_HIDE_DELAY),
            "a round trip through the editor and back must never collapse"
        );
    }

    #[test]
    fn re_arming_does_not_push_the_deadline_forever() {
        // `focus_left` runs every frame while focus is away. If it reset the
        // anchor each time, the window would never elapse and the panel
        // would never collapse.
        let start = t0();
        let mut a = AutoHide::default();
        a.focus_left(start);
        for ms in [100, 200, 300, 399] {
            a.focus_left(start + Duration::from_millis(ms));
        }
        assert!(
            a.due(start + Duration::from_millis(400), AUTO_HIDE_DELAY),
            "the window runs from when focus FIRST left"
        );
    }

    #[test]
    fn a_hold_keeps_the_panel_open_and_names_its_reason() {
        let start = t0();
        let mut a = AutoHide::default();
        a.focus_left(start);
        a.suppress(Suppressed::Dragging);
        assert_eq!(a.suppressed(), Some(Suppressed::Dragging));
        assert!(
            !a.due(start + Duration::from_secs(10), AUTO_HIDE_DELAY),
            "collapsing mid-drag would yank the splitter from under the pointer"
        );

        // Releasing does not itself collapse: the caller re-arms next frame.
        a.release();
        assert!(
            !a.due(start + Duration::from_secs(10), AUTO_HIDE_DELAY),
            "the release cleared the arm too, so nothing is pending yet"
        );
        a.focus_left(start + Duration::from_secs(10));
        assert!(
            a.due(start + Duration::from_secs(11), AUTO_HIDE_DELAY),
            "and the next frame starts a fresh window"
        );
    }
}
