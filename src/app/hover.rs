use std::time::{Duration, Instant};

pub const HOVER_DELAY: Duration = Duration::from_millis(300);

#[allow(dead_code)]
#[derive(Default)]
pub struct HoverDwell {
    anchor: Option<Instant>,
    cell: Option<(u16, u16)>,
    fired: bool,
}

#[allow(dead_code)]
impl HoverDwell {
    pub fn on_move(&mut self, now: Instant, col: u16, row: u16) {
        if self.cell != Some((col, row)) {
            self.cell = Some((col, row));
            self.anchor = Some(now);
            self.fired = false;
        }
    }

    pub fn due(&self, now: Instant, threshold: Duration) -> bool {
        !self.fired && matches!(self.anchor, Some(a) if now.duration_since(a) >= threshold)
    }

    pub fn mark_fired(&mut self) {
        self.fired = true;
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.cell = None;
        self.fired = false;
    }

    pub fn cell(&self) -> Option<(u16, u16)> {
        self.cell
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_dwell_becomes_due_only_after_the_threshold() {
        let mut h = HoverDwell::default();
        let t0 = Instant::now();
        h.on_move(t0, 10, 5);
        assert!(
            !h.due(t0, HOVER_DELAY),
            "not due the instant the pointer arrives"
        );
        assert!(
            !h.due(t0 + Duration::from_millis(299), HOVER_DELAY),
            "not due one ms before the threshold"
        );
        assert!(
            h.due(t0 + Duration::from_millis(300), HOVER_DELAY),
            "due once the 300ms dwell elapses"
        );
    }

    #[test]
    fn hover_dwell_restarts_when_the_pointer_moves_to_a_new_cell() {
        let mut h = HoverDwell::default();
        let t0 = Instant::now();
        h.on_move(t0, 10, 5);
        h.on_move(t0 + Duration::from_millis(200), 11, 5);
        assert!(
            !h.due(t0 + Duration::from_millis(450), HOVER_DELAY),
            "the new cell has only been rested on for 250ms"
        );
        assert!(
            h.due(t0 + Duration::from_millis(500), HOVER_DELAY),
            "300ms after arriving at the new cell it is due"
        );
    }

    #[test]
    fn hover_dwell_keeps_counting_while_the_pointer_stays_in_one_cell() {
        let mut h = HoverDwell::default();
        let t0 = Instant::now();
        h.on_move(t0, 10, 5);
        h.on_move(t0 + Duration::from_millis(200), 10, 5);
        assert!(
            h.due(t0 + Duration::from_millis(300), HOVER_DELAY),
            "a repeated event on the same cell must not reset the timer"
        );
    }

    #[test]
    fn hover_dwell_fires_at_most_once_per_rest() {
        let mut h = HoverDwell::default();
        let t0 = Instant::now();
        h.on_move(t0, 10, 5);
        assert!(h.due(t0 + Duration::from_millis(300), HOVER_DELAY));
        h.mark_fired();
        assert!(
            !h.due(t0 + Duration::from_millis(400), HOVER_DELAY),
            "must not fire again while resting on the same cell"
        );
        h.on_move(t0 + Duration::from_millis(500), 20, 9);
        assert!(
            !h.due(t0 + Duration::from_millis(600), HOVER_DELAY),
            "the new cell has only been rested on for 100ms"
        );
        assert!(
            h.due(t0 + Duration::from_millis(800), HOVER_DELAY),
            "fires again after dwelling on the new cell"
        );
    }

    #[test]
    fn hover_dwell_cell_reports_the_current_pointer_cell() {
        let mut h = HoverDwell::default();
        let t0 = Instant::now();
        assert_eq!(h.cell(), None);
        h.on_move(t0, 7, 3);
        assert_eq!(h.cell(), Some((7, 3)));
    }
}
