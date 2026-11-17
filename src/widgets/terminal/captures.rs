//! What a clear wipes away: spotting a program clearing the screen, keeping the rows it
//! cleared, finding a captured line again, and the rewind buffer.

use super::*;

/// One PTY chunk's arrival stamps for the timestamps gutter: every row the
/// cursor advanced past gets the chunk's arrival time, keyed on the scroll
/// clock (`clock reading + cursor grid line` = a stable content id) — NOT
/// on `history_size`, which treated any upward cursor motion as a
/// scrollback wipe (a `docker pull` progress redraw erased every stamp, an
/// alt-screen round trip erased them and then re-stamped the whole
/// scrollback with the exit time) and which saturates once the ring fills,
/// freezing ids to screen positions. Alternate-screen output stamps
/// nothing: it isn't scrollback rows, and the gutter is normal-screen only.
/// Destructive-clear detector over the raw PTY byte stream: ED 2
/// (`\x1b[2J`, erases the live screen), ED 3 (`\x1b[3J`, erases the
/// scrollback) and RIS (`\x1bc`, both). Keyed on the bytes rather than on
/// `history_size` movement, which is 0 → 0 when nothing has scrolled into
/// history yet. The scan is POSITIONAL: it tracks alt-screen entry/exit
/// (DECSET/DECRST 47/1047/1049) through the stream, so a wipe counts only
/// where it landed — `clear && vim` wipes the primary screen even though
/// the same chunk ends inside the alt screen. An unterminated trailing
/// sequence (bounded) carries to the next read; consumed bytes are never
/// rescanned, which keeps the alt tracking single-shot.
#[derive(Default)]
pub(super) struct WipeSniffer {
    carry: Vec<u8>,
}

/// Longest carried partial sequence; a real DECSET list or ED is far
/// shorter, and the bound stops a hostile unterminated CSI from growing.
pub(super) const WIPE_CARRY_MAX: usize = 64;

impl WipeSniffer {
    /// Scan one chunk. `alt` is the terminal's alt-screen state where the
    /// chunk STARTS. Returns the primary-screen wipes in it, in order.
    pub(super) fn scan(&mut self, chunk: &[u8], mut alt: bool) -> Vec<Wipe> {
        let mut data = std::mem::take(&mut self.carry);
        // Offsets into `data` are reported against `chunk`: a sequence
        // carried from the last read ends in this one, never before it.
        let carried = data.len();
        data.extend_from_slice(chunk);
        let mut wipes = Vec::new();
        let mut wipe = |end: usize, screen: bool, hist: bool| {
            let end = end.saturating_sub(carried);
            wipes.push(Wipe { end, screen, hist });
        };
        let mut i = 0;
        while i < data.len() {
            if data[i] != 0x1b {
                i += 1;
                continue;
            }
            let Some(&next) = data.get(i + 1) else {
                self.carry = data.split_off(i);
                return wipes;
            };
            match next {
                // RIS: full reset — everything is gone, back on primary.
                b'c' => {
                    wipe(i + 2, true, true);
                    alt = false;
                    i += 2;
                }
                b'[' => {
                    let mut j = i + 2;
                    while j < data.len()
                        && !(0x40..=0x7e).contains(&data[j])
                        && !matches!(data[j], 0x18 | 0x1a | 0x1b)
                    {
                        j += 1;
                    }
                    if j >= data.len() {
                        if data.len() - i <= WIPE_CARRY_MAX {
                            self.carry = data.split_off(i);
                        }
                        return wipes;
                    }
                    // CAN/SUB cancel the sequence and an ESC inside an
                    // incomplete CSI begins a NEW escape (VT100) — none may
                    // be swallowed as a parameter, or whatever follows a
                    // cancelled CSI goes unseen.
                    if matches!(data[j], 0x18 | 0x1a | 0x1b) {
                        i = if data[j] == 0x1b { j } else { j + 1 };
                        continue;
                    }
                    let params = &data[i + 2..j];
                    match data[j] {
                        b'J' if params == b"2" && !alt => wipe(j + 1, true, false),
                        b'J' if params == b"3" && !alt => wipe(j + 1, false, true),
                        fin @ (b'h' | b'l') if params.first() == Some(&b'?') => {
                            for p in params[1..].split(|&c| c == b';') {
                                if p == b"47" || p == b"1047" || p == b"1049" {
                                    alt = fin == b'h';
                                }
                            }
                        }
                        _ => {}
                    }
                    i = j + 1;
                }
                _ => i += 2,
            }
        }
        wipes
    }
}

/// How many rows either side of its recorded row a captured line is still
/// looked for (#1450). The row is read with the parser stopped right after
/// the line, so it is exact unless a program moved the cursor around the
/// line; looking any further could land on another run's copy.
pub(super) const CAPTURE_SLACK: i32 = 2;

/// Where a capture's row is now ([`CaptureRows::line`]).
pub(super) enum RowAt {
    /// On this grid line.
    Line(i64),
    /// Still captured, but where is unknown. Output keeps its order, so it
    /// is the match of its text this many matches up from the bottom: one
    /// for each later printed line still held that the same text matches.
    Unknown { later: usize },
}

impl CaptureRows {
    /// Record the line the parser just completed, whose text is `text`,
    /// and return its id. The parser stands just past the line's newline,
    /// so the line ended on the row above the cursor and starts where that
    /// row's soft wraps begin. Past the CAPTURES panel's cap the oldest are
    /// dropped.
    pub(super) fn record(
        &mut self,
        term: &mut Term<VoidListener>,
        clock: &mut ScrollClock,
        text: &str,
    ) -> u64 {
        let top = term.grid().topmost_line().0;
        let mut line = term.grid().cursor.point.line.0 - 1;
        while line > top && row_wraps(term, line - 1) {
            line -= 1;
        }
        let id = next_mark_id();
        let row = Some(clock.tick(term) + i64::from(line));
        // Two triggers that catch one line stop the parser at the same
        // place, so they record the same row: one printed line, one serial.
        if self
            .rows
            .last_key_value()
            .is_none_or(|(_, last)| last.row != row)
        {
            self.line += 1;
        }
        let text = text.trim_end().to_owned();
        let line = self.line;
        self.rows.insert(id, CapturedRow { row, text, line });
        while self.rows.len() > crate::widgets::captures::CAPTURES_MAX {
            self.rows.pop_first();
        }
        id
    }

    /// Where `id`'s row is at clock reading `now`, or None once it was
    /// erased. `needle` is the text the jump looks for.
    pub(super) fn line(&self, id: u64, now: i64, needle: &str) -> Option<RowAt> {
        let captured = self.rows.get(&id)?;
        Some(match captured.row {
            Some(row) => RowAt::Line(row - now),
            None => {
                // Serials rise with ids, so the later lines' come in order.
                let mut later: Vec<u64> = self
                    .rows
                    .range((std::ops::Bound::Excluded(id), std::ops::Bound::Unbounded))
                    .filter(|(_, c)| c.line != captured.line && c.text.starts_with(needle))
                    .map(|(_, c)| c.line)
                    .collect();
                later.dedup();
                RowAt::Unknown { later: later.len() }
            }
        })
    }

    /// Drop the rows a wipe that just landed erased, `now` being the clock
    /// read right after it (the same test as [`RunningCommand::follow_wipe`]).
    /// A row whose place is unknown cannot be told on screen from in the
    /// scrollback, so it goes with any wipe of the scrollback (a reset, or
    /// the ED 3 of a `clear`, whose ED 2 has already pushed the screen
    /// there): an entry then says its line is gone rather than taking text
    /// printed after the wipe.
    pub(super) fn follow_wipe(&mut self, wipe: Wipe, now: i64) {
        self.rows.retain(|_, captured| match captured.row {
            Some(row) => {
                let line = row - now;
                !((line >= 0 && wipe.screen) || (line < 0 && wipe.hist))
            }
            None => !wipe.hist,
        });
    }

    /// Carry every row across `resize`. A resize re-wraps a soft-wrapped
    /// line onto more or fewer rows, but keeps every line and their order
    /// (short of the scrollback overflowing, which drops the oldest), and
    /// the cursor stays on its line. So a row's place in the list of line
    /// starts counted up from the cursor survives it, and so does a row
    /// below the cursor. Under a full-screen program the primary grid is
    /// re-wrapped too, out of reach behind the alternate screen, so every
    /// row's place becomes unknown and its entry finds it by its text and
    /// its order among the captures of that text.
    pub(super) fn reflow(
        &mut self,
        term: &mut Term<VoidListener>,
        clock: &mut ScrollClock,
        resize: impl FnOnce(&mut Term<VoidListener>),
    ) {
        if self.rows.is_empty() {
            resize(term);
            return;
        }
        if term.mode().contains(TermMode::ALT_SCREEN) {
            resize(term);
            self.rows
                .values_mut()
                .for_each(|captured| captured.row = None);
            return;
        }
        let now = clock.tick(term);
        let before = line_starts_upward(term);
        let top = i64::from(term.grid().topmost_line().0);
        let cursor = i64::from(term.grid().cursor.point.line.0);
        // (id, the row's text and serial, Ok(place among the line starts) |
        // Err(line below the cursor)), None for a row whose place was
        // already unknown.
        type Held = (u64, String, u64, Option<Result<usize, i64>>);
        let held: Vec<Held> = std::mem::take(&mut self.rows)
            .into_iter()
            .filter_map(
                |(
                    id,
                    CapturedRow {
                        row,
                        text,
                        line: serial,
                    },
                )| {
                    let Some(row) = row else {
                        return Some((id, text, serial, None));
                    };
                    let line = row - now;
                    let at = if line > cursor {
                        Err(line)
                    } else {
                        Ok(before.partition_point(|&s| i64::from(s) > line))
                    };
                    (line >= top).then_some((id, text, serial, Some(at)))
                },
            )
            .collect();
        resize(term);
        let now = clock.tick(term);
        let after = line_starts_upward(term);
        for (id, text, serial, at) in held {
            let row = match at {
                None => None,
                Some(Ok(k)) => match after.get(k) {
                    Some(&l) => Some(now + i64::from(l)),
                    None => continue,
                },
                Some(Err(line)) => Some(now + line),
            };
            self.rows.insert(
                id,
                CapturedRow {
                    row,
                    text,
                    line: serial,
                },
            );
        }
    }
}

/// The rows that start a logical line, from the cursor's line up to the top
/// of the scrollback.
pub(super) fn line_starts_upward(term: &Term<VoidListener>) -> Vec<i32> {
    let top = term.grid().topmost_line().0;
    let cursor = term.grid().cursor.point.line.0;
    (top..=cursor)
        .rev()
        .filter(|&l| l == top || !row_wraps(term, l - 1))
        .collect()
}

impl PtyTerminal {
    /// The grid line where a captured line starts, or `None`. `anchor` is
    /// this pane's id for the row its reader saw the line start on
    /// ([`crate::triggers::TriggerHit::anchor`]): that row, or the matching
    /// row nearest it within [`CAPTURE_SLACK`] (the upper one on a tie), so a
    /// line every re-run prints again finds its own copy rather than the
    /// newest (#1450). Once the row was erased or rotated off the scrollback
    /// the line is gone, never stood in for by another copy. Without an
    /// anchor the newest match wins. The straight case is a row carrying
    /// `needle` as a prefix; a pane narrower than the needle instead shows
    /// the line's FIRST WRAPPED ROW, whose text is a prefix OF the needle.
    /// That reverse arm is gated on real wrap metadata, not row length: the
    /// row must itself soft-wrap (WRAPLINE on its last cell) — otherwise any
    /// short unrelated row sharing the prefix would win — and continuation
    /// rows (the PREVIOUS row wraps) never match either arm, or a later
    /// continuation sharing the prefix would beat the true start under the
    /// newest-first scan.
    pub fn find_captured_line(&self, needle: &str, anchor: Option<u64>) -> Option<i32> {
        if needle.is_empty() {
            return None;
        }
        let term = self.term.lock();
        let cols = term.columns();
        let wraps = |line: i32| {
            cols > 0
                && term.grid()[Point::new(Line(line), Column(cols - 1))]
                    .flags
                    .contains(Flags::WRAPLINE)
        };
        let top = term.grid().topmost_line().0;
        let bottom = term.screen_lines() as i32 - 1;
        let starts = |line: i32| {
            if !(top..=bottom).contains(&line) || (line > top && wraps(line - 1)) {
                return false;
            }
            let (text, _) = row_text_and_cols(&term, line);
            let t = text.trim_end();
            !t.is_empty() && (t.starts_with(needle) || (wraps(line) && needle.starts_with(t)))
        };
        let Some(id) = anchor else {
            return (top..=bottom).rev().find(|&line| starts(line));
        };
        let now = self.clock_now(&term);
        let at = match self.capture_rows.lock().unwrap().line(id, now, needle)? {
            RowAt::Line(at) => at,
            RowAt::Unknown { later } => {
                return (top..=bottom).rev().filter(|&line| starts(line)).nth(later);
            }
        };
        if at < i64::from(top) {
            return None;
        }
        let at = at.min(i64::from(bottom)) as i32;
        (0..=CAPTURE_SLACK).find_map(|d| [at - d, at + d].into_iter().find(|&line| starts(line)))
    }

    /// The pane's rewind buffer (#357), for the scrubber to replay from.
    ///
    /// Handed out as the shared handle rather than a copy: the buffer is
    /// megabytes by design, and the reader thread is writing to it while the
    /// overlay reads, so a snapshot would be both expensive and immediately
    /// stale.
    pub fn rewind(&self) -> &Arc<std::sync::Mutex<crate::rewind::RewindBuffer>> {
        &self.rewind
    }
}
