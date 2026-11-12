use std::time::{Duration, Instant};

/// How long the caret stays shown, and then hidden, in each blink.
pub(super) const HALF: Duration = Duration::from_millis(530);

pub struct CursorBlink {
    anchor: Instant,
}

impl CursorBlink {
    pub fn new() -> Self {
        Self {
            anchor: Instant::now(),
        }
    }

    pub fn poke(&mut self) {
        self.anchor = Instant::now();
    }

    pub fn visible_phase(&self) -> bool {
        let elapsed = self.anchor.elapsed();
        let phases = (elapsed.as_millis() / HALF.as_millis()) as u64;
        phases.is_multiple_of(2)
    }
}
