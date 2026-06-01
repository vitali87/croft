use std::time::{Duration, Instant};

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
        const HALF: Duration = Duration::from_millis(530);
        let elapsed = self.anchor.elapsed();
        let phases = (elapsed.as_millis() / HALF.as_millis()) as u64;
        phases.is_multiple_of(2)
    }
}
