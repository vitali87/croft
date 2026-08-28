use std::time::{Duration, Instant};

/// How long focus must stay away from the sidebar before auto-hide collapses
/// it. Long enough that a click passing *through* the panel on its way to the
/// editor does not collapse it under the pointer (#302), short enough that a
/// deliberate move away still feels immediate.
pub const AUTO_HIDE_DWELL: Duration = Duration::from_millis(400);

/// The "focus left, but only just" timer behind auto-hide's grace delay.
///
/// Mirrors [`super::hover::HoverDwell`]: `now` is a parameter rather than read
/// from the clock inside, so the whole state machine tests without sleeping.
///
/// The anchor is set when focus FIRST leaves and deliberately not re-stamped
/// while it stays away — re-arming every frame would push the deadline forever
/// and the collapse would never fire.
#[derive(Default)]
pub struct SidebarDwell {
    anchor: Option<Instant>,
}

impl SidebarDwell {
    /// Focus moved somewhere that wants the sidebar collapsed. Starts the
    /// clock on the first such move and leaves a running one alone.
    pub fn arm(&mut self, now: Instant) {
        if self.anchor.is_none() {
            self.anchor = Some(now);
        }
    }

    /// Whether the grace period has elapsed on an armed timer.
    pub fn due(&self, now: Instant, threshold: Duration) -> bool {
        matches!(self.anchor, Some(a) if now.duration_since(a) >= threshold)
    }

    /// Whether a collapse is currently pending.
    pub fn armed(&self) -> bool {
        self.anchor.is_some()
    }

    /// Cancel a pending collapse: focus came back, or the collapse fired.
    pub fn disarm(&mut self) {
        self.anchor = None;
    }

    /// Push an ARMED timer's deadline out to `now`, leaving a disarmed one
    /// alone. Used while a structural suppression (Zen Mode, hidden activity
    /// bar) holds the collapse off: the dwell must survive — cancelling it
    /// strands the feature, since nothing re-arms when the suppression lifts —
    /// but it must not fire the instant it does, for a focus move made an hour
    /// earlier. Re-stamping keeps the pending collapse alive while restarting
    /// its grace period from the moment a collapse first becomes possible.
    ///
    /// Deliberately NOT `arm`: `arm` starts a timer that was not running, and a
    /// suppressed tick must never begin one on its own.
    pub fn restamp(&mut self, now: Instant) {
        if self.anchor.is_some() {
            self.anchor = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dwell_becomes_due_only_after_the_threshold() {
        let mut d = SidebarDwell::default();
        let t0 = Instant::now();
        d.arm(t0);
        assert!(
            !d.due(t0, AUTO_HIDE_DWELL),
            "not due the instant focus left"
        );
        assert!(
            !d.due(t0 + Duration::from_millis(399), AUTO_HIDE_DWELL),
            "not due one ms before the threshold"
        );
        assert!(
            d.due(t0 + Duration::from_millis(400), AUTO_HIDE_DWELL),
            "due once the 400ms grace elapses"
        );
    }

    #[test]
    fn an_unarmed_dwell_is_never_due() {
        let d = SidebarDwell::default();
        let t0 = Instant::now();
        assert!(!d.armed());
        assert!(
            !d.due(t0 + Duration::from_secs(10), AUTO_HIDE_DWELL),
            "no focus move means nothing to collapse, however long we wait"
        );
    }

    #[test]
    fn re_arming_does_not_push_the_deadline() {
        let mut d = SidebarDwell::default();
        let t0 = Instant::now();
        d.arm(t0);
        // Several more focus moves land inside the grace window. Anchoring at
        // each of them would move the deadline out forever.
        d.arm(t0 + Duration::from_millis(100));
        d.arm(t0 + Duration::from_millis(200));
        d.arm(t0 + Duration::from_millis(399));
        assert!(
            d.due(t0 + Duration::from_millis(400), AUTO_HIDE_DWELL),
            "the deadline is anchored at the FIRST departure, not the latest"
        );
    }

    #[test]
    fn disarming_cancels_a_pending_collapse() {
        let mut d = SidebarDwell::default();
        let t0 = Instant::now();
        d.arm(t0);
        d.disarm();
        assert!(!d.armed());
        assert!(
            !d.due(t0 + Duration::from_secs(1), AUTO_HIDE_DWELL),
            "focus coming back cancels the collapse outright"
        );
    }

    #[test]
    fn re_arming_after_a_disarm_starts_a_fresh_window() {
        let mut d = SidebarDwell::default();
        let t0 = Instant::now();
        d.arm(t0);
        d.disarm();
        let t1 = t0 + Duration::from_millis(500);
        d.arm(t1);
        assert!(
            !d.due(t1 + Duration::from_millis(399), AUTO_HIDE_DWELL),
            "the second departure gets its own full grace period"
        );
        assert!(d.due(t1 + Duration::from_millis(400), AUTO_HIDE_DWELL));
    }
}
