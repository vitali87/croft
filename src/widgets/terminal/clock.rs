//! The scroll clock and row times: how far output has scrolled, which keeps selections and
//! notes on their text, and the time each row was printed.

use super::*;

/// A row arriving this long after its predecessor is highlighted amber in
/// the timestamps gutter (the "where did the deploy stall" signal).
pub(super) const STALL_GAP_MS: u64 = 60_000;

impl ScrollClock {
    pub(super) fn new() -> Self {
        Self {
            base: 0,
            planted: None,
            hist: 0,
        }
    }

    /// Plant the one-cell tracer selection at `line` in alacritty's own
    /// `Term::selection` slot (croft paints selections itself, so the slot
    /// is otherwise unused). alacritty rotates it on every scroll —
    /// including ring rotation once the scrollback is full, where
    /// `history_size` saturates and stops counting — so its drift between
    /// ticks is the true lines-scrolled count.
    pub(super) fn plant_tracer(term: &mut Term<VoidListener>, line: i32) {
        let point = Point::new(Line(line), Column(0));
        let mut tracer = TracerSelection::new(SelectionType::Simple, point, Side::Left);
        tracer.update(point, Side::Right);
        term.selection = Some(tracer);
    }

    /// Where the tracer sits now, when it survived since the last plant
    /// (screen clears, alt-screen swaps and content rotating fully off the
    /// scrollback kill it).
    pub(super) fn tracer_line(term: &Term<VoidListener>) -> Option<i32> {
        let range = term.selection.as_ref()?.to_range(term)?;
        Some(range.start.line.0)
    }

    pub(super) fn now(&self, term: &Term<VoidListener>) -> i64 {
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return self.base;
        }
        match (self.planted, Self::tracer_line(term)) {
            (Some(planted), Some(cur)) => self.base + i64::from(planted - cur),
            _ => {
                let growth = term.grid().history_size() as i64 - self.hist;
                self.base + growth.max(0)
            }
        }
    }

    /// Fold the tracer's drift into the clock, re-plant it fresh and return
    /// the folded reading. The newest history line is the parking spot:
    /// application clears only touch live-screen rows, so nothing kills it
    /// there, and it sits a full scrollback's depth from rotating off the
    /// top before the next tick (every rendered frame ticks).
    pub(super) fn tick(&mut self, term: &mut Term<VoidListener>) -> i64 {
        let now = self.now(term);
        if !term.mode().contains(TermMode::ALT_SCREEN) {
            self.base = now;
            self.hist = term.grid().history_size() as i64;
            let park = if self.hist > 0 { -1 } else { 0 };
            Self::plant_tracer(term, park);
            self.planted = Some(park);
        }
        now
    }
}

pub(super) fn stamp_chunk(
    term: &mut Term<VoidListener>,
    clock: &mut ScrollClock,
    lt: &mut std::collections::BTreeMap<i64, u64>,
    prev_id: &mut i64,
    now_ms: u64,
) {
    if term.mode().contains(TermMode::ALT_SCREEN) {
        return;
    }
    let now = clock.tick(term);
    let cur_id = now + i64::from(term.grid().cursor.point.line.0);
    // Stamp from the PREVIOUS cursor row inclusive (last touch wins: the
    // row content just landed on is re-stamped as it fills). A redraw that
    // moved the cursor UP re-arrives at existing rows: stamp only the
    // cursor row, never wipe.
    let from = (*prev_id).min(cur_id);
    for a in from..=cur_id {
        lt.insert(a, now_ms);
    }
    *prev_id = cur_id;
    // Bounded by the largest configurable scrollback plus a screen;
    // oldest stamps go first.
    while lt.len() > 210_000 {
        lt.pop_first();
    }
}

impl PtyTerminal {
    /// The pane's monotonic scroll-clock reading (see `clock_now`): the
    /// content anchor the app pairs with grid coordinates it captures now
    /// (annotation prompts, copy mode), so the pair survives streaming
    /// output and scrollback saturation alike.
    pub fn scroll_clock(&mut self) -> i64 {
        self.tick_clock()
    }

    /// When the row at current grid `line` arrived (epoch millis), if the
    /// reader thread stamped it.
    pub fn row_time(&self, line: i32) -> Option<u64> {
        let term = self.term.lock();
        let now = self.clock_now(&term);
        drop(term);
        self.line_times
            .lock()
            .unwrap()
            .get(&(i64::from(line) + now))
            .copied()
    }

    /// The current scroll-clock reading against an already-locked term.
    pub(super) fn clock_now(&self, term: &Term<VoidListener>) -> i64 {
        self.clock.lock().unwrap().now(term)
    }

    /// Fold the tracer's drift into the clock, re-plant it fresh and return
    /// the folded reading (see [`ScrollClock::tick`]).
    pub(super) fn tick_clock(&mut self) -> i64 {
        let mut term = self.term.lock();
        self.clock.lock().unwrap().tick(&mut term)
    }

    /// Test-only: run one reader-thread timestamp pass over the current
    /// grid, exactly as a real chunk arrival would.
    #[cfg(test)]
    pub fn stamp_chunk_for_test(&self, prev_id: &mut i64, now_ms: u64) {
        let mut term = self.term.lock();
        let mut clock = self.clock.lock().unwrap();
        let mut lt = self.line_times.lock().unwrap();
        stamp_chunk(&mut term, &mut clock, &mut lt, prev_id, now_ms);
    }

    /// Test-only: the stamped (content id, arrival ms) pairs.
    #[cfg(test)]
    pub fn line_time_entries_for_test(&self) -> Vec<(i64, u64)> {
        self.line_times
            .lock()
            .unwrap()
            .iter()
            .map(|(&k, &v)| (k, v))
            .collect()
    }
}

/// Epoch millis → local wall-clock `HH:MM:SS` (libc localtime, the same
/// no-date-crate route the trash metadata writer takes).
pub(super) fn hhmmss(millis: u64) -> String {
    // `localtime_r` takes a `*const time_t`, so the binding's own definition is
    // the only correct type here. musl's is mid-migration to 64-bit and libc
    // marks the alias deprecated ahead of the change, which `-D warnings`
    // turns into a build error on the Linux targets; naming the field type
    // through `tm` is not possible (it has no `time_t` member), so the alias
    // stays and the deprecation is acknowledged at this one call.
    #[allow(deprecated)]
    let secs = (millis / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return String::from("--:--:--");
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}
