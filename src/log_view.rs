//! Windowed backing store for the rendered ANSI log view (#257).
//!
//! Logs are the worst case for size, so the file is NEVER loaded whole. This
//! keeps only a line-offset index plus a small byte window around the
//! viewport, exactly the posture [`crate::hex`] uses: opening a multi-gigabyte
//! log costs one index pass, and scrolling costs one bounded read per refill.
//!
//! The index stores a `u64` offset per line. That is the one cost that scales
//! with line COUNT rather than viewport size, so it is capped: past
//! [`MAX_INDEXED_LINES`] the view reports truncation rather than silently
//! showing a prefix as if it were the whole file.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::ansi_text::{AnsiLine, AnsiStyle, parse_into, parse_line};
use crate::widgets::editor_find::{MatchPos, line_matches};
use crate::widgets::search::{SearchOpts, line_may_match};

/// One window refill: many screens' worth, still instant to read.
const WINDOW_BYTES: usize = 256 * 1024;

/// Index at most this many lines. At 8 bytes each this caps index memory
/// around 80 MB, which a 2 GB log of short lines could otherwise blow past.
/// Beyond it the view is explicitly truncated, never silently short.
pub const MAX_INDEXED_LINES: usize = 10_000_000;

/// Bytes read while building the index, per chunk.
const INDEX_CHUNK: usize = 1024 * 1024;

/// Lines requested per find-sweep read. The read is clamped to
/// [`WINDOW_BYTES`] regardless, so this only bounds the index arithmetic.
const SCAN_CHUNK_LINES: usize = 4096;

/// How much stripped text one find sweep may read before it gives up and
/// says so. A find runs on every keystroke, so an unbounded sweep would
/// stream the whole file per keypress: the exact cost this view exists to
/// avoid. Same stance as the trigger scanner's `LINE_CAP`, one level up.
pub const FIND_SCAN_BYTES: usize = 4 * 1024 * 1024;

/// The outcome of one find step over a windowed file.
///
/// A budgeted search has three answers, not two. Collapsing the third into
/// `None` is what makes "there is no match" and "there is no match I could
/// reach" indistinguishable, which is the same dishonesty as a partial count
/// shown as a total, one layer up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A match, at this position.
    Found(MatchPos),
    /// The file genuinely contains no match, searched end to end.
    Absent,
    /// The sweep ran out of budget (or hit bytes it could not read) before
    /// it could answer. The file may still contain a match.
    OutOfReach,
}

impl Step {
    /// The match, if there is one. Callers that cannot act on the difference
    /// still have to NAME it by calling this.
    pub fn found(self) -> Option<MatchPos> {
        match self {
            Step::Found(m) => Some(m),
            _ => None,
        }
    }

    /// Whether the answer was cut short rather than complete.
    pub fn out_of_reach(self) -> bool {
        matches!(self, Step::OutOfReach)
    }
}

/// One window of a find sweep: its text, how many COMPLETE lines it covers,
/// and whether bytes were dropped to the window cap.
#[derive(Default)]
struct Window {
    text: String,
    consumed: usize,
    /// Set when the read stopped inside a line, so the sweep saw only part
    /// of the content in this range.
    partial: bool,
}

/// Bytes a single find sweep has left.
struct ScanBudget(usize);

impl ScanBudget {
    fn new() -> Self {
        Self(FIND_SCAN_BYTES)
    }

    /// A budget of a different size, for a sweep with its own bound (a copy
    /// may gather more than a per-keystroke find may scan).
    fn new_with(bytes: usize) -> Self {
        Self(bytes)
    }

    fn charge(&mut self, bytes: usize) {
        self.0 = self.0.saturating_sub(bytes);
    }

    fn spent(&self) -> bool {
        self.0 == 0
    }
}

/// How much text one copy may gather. A log is unbounded and a selection
/// can span it, so the copy is capped and the caller is told when it hit the
/// cap: a clipboard that silently holds less than the user selected is worse
/// than one that says it was clamped.
pub const MAX_COPY_BYTES: usize = 4 * 1024 * 1024;

pub struct LogView {
    pub path: PathBuf,
    pub file_len: u64,
    /// Byte offset of the start of each line.
    line_starts: Vec<u64>,
    /// True when the file has more lines than [`MAX_INDEXED_LINES`], so the
    /// UI can say so instead of pretending the tail does not exist.
    pub truncated: bool,
    /// Parsed lines for the current window, keyed by absolute line number.
    cache: std::collections::BTreeMap<usize, AnsiLine>,
    /// Line number the cache starts at, for cheap eviction.
    cache_start: usize,
    /// Anchor and head of a mouse selection, as absolute (line, char column).
    ///
    /// Kept here rather than in the editor's own `selection` because those
    /// coordinates index `lines`, which for a log tab is a one-line stub.
    /// Same shape the Markdown preview uses for the same reason.
    pub selection: Option<((usize, usize), (usize, usize))>,
    /// Whether a drag is in progress, so a move outside the body still
    /// extends the selection rather than starting a new one.
    pub dragging: bool,
    /// The body rect the last frame painted, for mouse hit-testing. Frame
    /// truth: the renderer writes it, the mouse path reads it.
    pub last_body: ratatui::layout::Rect,
}

impl LogView {
    /// Index `path`'s line offsets without reading its contents into memory.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let file_len = file.metadata()?.len();
        let mut reader = BufReader::new(file);
        let mut line_starts = vec![0u64];
        let mut buf = vec![0u8; INDEX_CHUNK];
        let mut pos = 0u64;
        let mut truncated = false;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            for (i, &b) in buf[..n].iter().enumerate() {
                if b == b'\n' {
                    if line_starts.len() >= MAX_INDEXED_LINES {
                        truncated = true;
                        break;
                    }
                    line_starts.push(pos + i as u64 + 1);
                }
            }
            if truncated {
                break;
            }
            pos += n as u64;
        }
        // A trailing newline produces an empty final entry; drop it so
        // `len()` matches what a reader would call the line count.
        if line_starts.len() > 1 && line_starts.last() == Some(&file_len) {
            line_starts.pop();
        }
        Ok(Self {
            path: path.to_path_buf(),
            file_len,
            line_starts,
            truncated,
            cache: std::collections::BTreeMap::new(),
            cache_start: 0,
            selection: None,
            dragging: false,
            last_body: ratatui::layout::Rect::default(),
        })
    }

    pub fn len(&self) -> usize {
        // An empty file seeds one line-start but has no lines; the renderer
        // reads `len()`, so it must not report a phantom row.
        if self.file_len == 0 {
            return 0;
        }
        self.line_starts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Byte range of one line, excluding its newline.
    fn line_range(&self, idx: usize) -> Option<(u64, u64)> {
        let start = *self.line_starts.get(idx)?;
        let end = self
            .line_starts
            .get(idx + 1)
            .map(|&s| s.saturating_sub(1))
            .unwrap_or(self.file_len);
        Some((start, end.max(start)))
    }

    /// Ensure `[first, first + count)` is parsed and cached. Parsing starts at
    /// the window's first line with a DEFAULT style rather than the file's
    /// true carried style: reconstructing that exactly would mean rescanning
    /// from byte zero on every jump, which is precisely the cost this view
    /// exists to avoid. In practice logs reset per line, and a window refill
    /// re-derives the carry within itself.
    pub fn ensure(&mut self, first: usize, count: usize) -> std::io::Result<()> {
        let last = (first + count).min(self.len());
        if first >= last {
            return Ok(());
        }
        if self.cache_start == first && self.cache.len() >= last - first {
            return Ok(());
        }
        let (start, _) = self.line_range(first).unwrap_or((0, 0));
        let (_, end) = self.line_range(last - 1).unwrap_or((0, 0));
        let span = (end - start) as usize;
        // Clamp the READ, but never cache a line whose bytes we did not fully
        // read: a window that stops mid-line would otherwise cache a truncated
        // line under a real line number, and the reader would see a cut-off
        // log line as if it were the whole thing. Lines wider than the window
        // are common (JSON logs, stack traces), so this is the normal case,
        // not an exotic one.
        let mut buf = vec![0u8; span.min(WINDOW_BYTES)];
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(start))?;
        let n = file.read(&mut buf)?;
        let complete = if (n as u64) < end - start {
            // Short read: keep only through the last newline, so the final
            // partial line is dropped rather than cached truncated.
            match buf[..n].iter().rposition(|&b| b == b'\n') {
                Some(nl) => &buf[..nl],
                // Not even one full line fits; cache nothing rather than lie.
                None => &buf[..0],
            }
        } else {
            &buf[..n]
        };
        let text = String::from_utf8_lossy(complete);

        self.cache.clear();
        self.cache_start = first;
        let mut style = AnsiStyle::default();
        for (k, raw) in text.split('\n').enumerate() {
            let idx = first + k;
            if idx >= last {
                break;
            }
            let parsed = parse_line(raw.strip_suffix('\r').unwrap_or(raw), &mut style);
            self.cache.insert(idx, parsed);
        }
        Ok(())
    }

    /// The parsed line at `idx`, if the current window covers it.
    pub fn line(&self, idx: usize) -> Option<&AnsiLine> {
        self.cache.get(&idx)
    }

    /// Read the raw bytes of `[first, first + count)` WITHOUT touching the
    /// viewport cache, so a find sweep does not evict the window the reader
    /// is looking at. Returns the window text and how many COMPLETE lines it
    /// covers; zero means not even one whole line fit.
    ///
    /// Style carry works like [`Self::ensure`]: each read restarts from the
    /// default style rather than rescanning from byte zero. Find only reads
    /// the stripped text, which no style affects, so that is invisible here.
    fn read_window(&self, first: usize, count: usize) -> std::io::Result<Window> {
        let last = (first + count).min(self.len());
        if first >= last {
            return Ok(Window::default());
        }
        let (start, _) = self.line_range(first).unwrap_or((0, 0));
        let (_, end) = self.line_range(last - 1).unwrap_or((0, 0));
        let span = (end - start) as usize;
        let mut buf = vec![0u8; span.min(WINDOW_BYTES)];
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(start))?;
        let n = file.read(&mut buf)?;
        let short = (n as u64) < end - start;
        // A line wider than the window is the JSON-log / stack-trace case, not
        // an exotic one. Rather than stall the sweep on it, search the prefix
        // that DID fit and let the caller step past the line, the same bounded
        // posture as the trigger scanner's `LINE_CAP`. A match living only in
        // the dropped tail is missed, which is why the budget is reported.
        let complete = match (short, buf[..n].iter().rposition(|&b| b == b'\n')) {
            (true, Some(nl)) => &buf[..nl],
            (true, None) => &buf[..n],
            (false, _) => &buf[..n],
        };
        let text = String::from_utf8_lossy(complete).into_owned();
        // A partial single line yields text but no COMPLETE line; report zero
        // so the caller steps rather than looping on the same offset forever.
        let consumed = if short && !text.contains('\n') {
            0
        } else {
            text.split('\n').count().min(last - first)
        };
        Ok(Window {
            text,
            consumed,
            // A window that stopped mid-line searched only the prefix that
            // fit. Saying so is the whole contract: an answer computed from
            // part of the file must never come back looking complete.
            partial: short,
        })
    }

    /// Walk `[first, last)` in bounded chunks, handing each stripped line to
    /// `visit`. Stops when `visit` returns `false`, the budget runs out, or
    /// the range ends.
    ///
    /// Returns whether the sweep was INCOMPLETE: it ran out of budget, or a
    /// line wider than the read window meant only its prefix was searched.
    /// Both mean the same thing to a caller, and it is the thing a caller
    /// must not paper over: an answer drawn from part of the file has to be
    /// reported as partial, or "no matches" and "no matches I could reach"
    /// become indistinguishable.
    fn scan_range(
        &self,
        first: usize,
        last: usize,
        budget: &mut ScanBudget,
        mut visit: impl FnMut(usize, &str) -> bool,
    ) -> bool {
        let last = last.min(self.len());
        let mut idx = first;
        // One scratch line for the whole sweep: the parser writes into it and
        // it is cleared per line, so a megabyte-wide scan allocates per WINDOW
        // rather than per line. That is most of the sweep's cost.
        let mut scratch = AnsiLine::default();
        let mut incomplete = false;
        while idx < last {
            if budget.spent() {
                return true;
            }
            let want = SCAN_CHUNK_LINES.min(last - idx);
            let window = match self.read_window(idx, want) {
                Ok(v) => v,
                // An unreadable window ends the sweep rather than spinning on
                // it; a log being rotated underneath us is the normal cause.
                // The caller is told, since the answer is now partial.
                Err(_) => return true,
            };
            if window.text.is_empty() {
                return incomplete;
            }
            incomplete |= window.partial;
            let mut style = AnsiStyle::default();
            for (k, raw) in window.text.split('\n').enumerate() {
                if idx + k >= last {
                    break;
                }
                // Charge the RAW bytes, not the stripped text. The budget
                // exists to bound IO, and a densely coloured log is mostly
                // escape bytes: charging what survives stripping let a sweep
                // read many times its budget on exactly the files this view
                // is for.
                budget.charge(raw.len().max(1));
                parse_into(
                    raw.strip_suffix('\r').unwrap_or(raw),
                    &mut style,
                    &mut scratch,
                );
                if !visit(idx + k, &scratch.text) {
                    return incomplete;
                }
                if budget.spent() {
                    return true;
                }
            }
            idx += window.consumed.max(1);
        }
        incomplete
    }

    /// [`Self::scan_range`] to the end of the file.
    fn scan_from(
        &self,
        first: usize,
        budget: &mut ScanBudget,
        visit: impl FnMut(usize, &str) -> bool,
    ) -> bool {
        self.scan_range(first, self.len(), budget, visit)
    }

    /// First match at or after (`from_row`, `from_col_chars`), searching
    /// forward and wrapping to the top, like the editor's own find.
    ///
    /// Unlike the editor's, this one is BUDGETED, and says so: a match
    /// further away than [`FIND_SCAN_BYTES`] comes back as
    /// [`Step::OutOfReach`], never as "no match". The budget is per
    /// direction-leg, so the wrap gets its own.
    pub fn find_next(
        &self,
        needle: &str,
        opts: SearchOpts,
        from_row: usize,
        from_col_chars: usize,
        skip_current: bool,
    ) -> Step {
        if needle.is_empty() || self.is_empty() {
            return Step::Absent;
        }
        let mut hit = None;
        let mut budget = ScanBudget::new();
        let cut_short = self.scan_from(from_row, &mut budget, |row, text| {
            if !line_may_match(text, needle, opts) {
                return true;
            }
            for (col, len) in line_matches(text, opts, needle) {
                if row == from_row {
                    let before = if skip_current {
                        col <= from_col_chars
                    } else {
                        col < from_col_chars
                    };
                    if before {
                        continue;
                    }
                }
                hit = Some(MatchPos {
                    row,
                    col_chars: col,
                    len_chars: len,
                });
                return false;
            }
            true
        });
        if let Some(m) = hit {
            return Step::Found(m);
        }
        if cut_short {
            return Step::OutOfReach;
        }
        // Wrap, INCLUDING from row 0: with `skip_current` set, a lone match
        // earlier on the anchor row is skipped by the forward leg, and
        // stopping here would clear the highlight on a one-match log every
        // time the user pressed Enter.
        //
        // The wrap takes the FIRST match in the file with no column filter,
        // including one at the anchor itself, which is what an editor does
        // with a single match.
        let mut budget = ScanBudget::new();
        let cut_short = self.scan_range(0, from_row + 1, &mut budget, |row, text| {
            if row > from_row {
                return false;
            }
            if !line_may_match(text, needle, opts) {
                return true;
            }
            if let Some((col, len)) = line_matches(text, opts, needle).into_iter().next() {
                hit = Some(MatchPos {
                    row,
                    col_chars: col,
                    len_chars: len,
                });
                return false;
            }
            true
        });
        match (hit, cut_short) {
            (Some(m), _) => Step::Found(m),
            (None, true) => Step::OutOfReach,
            (None, false) => Step::Absent,
        }
    }

    /// Last match before (`from_row`, `from_col_chars`), for Shift+Enter.
    ///
    /// Walks BACKWARDS a chunk at a time from the anchor, rather than
    /// scanning forward from the head and keeping the last hit. The forward
    /// version is simpler and useless on the files this view exists for: on
    /// an 11 MiB log every Shift+Enter spent its whole budget re-reading the
    /// head and returned nothing. Cost here is proportional to the DISTANCE
    /// to the previous match, which is what the user is actually asking for.
    ///
    /// Each leg (backwards to the head, then the wrap) gets its own budget,
    /// matching [`Self::find_next`]: a long first leg must not silently
    /// starve the wrap.
    pub fn find_prev(
        &self,
        needle: &str,
        opts: SearchOpts,
        from_row: usize,
        from_col_chars: usize,
    ) -> Step {
        if needle.is_empty() || self.is_empty() {
            return Step::Absent;
        }
        // Leg one: backwards from the anchor to the head, honouring the
        // anchor column, since only matches BEFORE the caret are "previous".
        match self.walk_back(
            needle,
            opts,
            from_row + 1,
            Some((from_row, from_col_chars)),
            0,
        ) {
            Step::Absent => {}
            other => return other,
        }
        // Leg two: wrap to the end of the file and walk back to the anchor
        // row, with NO column filter. A match LATER on the anchor row is the
        // previous match once the search has wrapped, and filtering it out
        // made it unreachable on a file whose only matches share that row.
        self.walk_back(needle, opts, self.len(), None, from_row)
    }

    /// Scan `[stop, end)` backwards a chunk at a time, returning the LAST
    /// match found. `anchor` restricts matches on its own row to those
    /// strictly before its column.
    fn walk_back(
        &self,
        needle: &str,
        opts: SearchOpts,
        end: usize,
        anchor: Option<(usize, usize)>,
        stop: usize,
    ) -> Step {
        let mut budget = ScanBudget::new();
        let mut end = end.min(self.len());
        while end > stop {
            let start = end.saturating_sub(SCAN_CHUNK_LINES).max(stop);
            let mut best: Option<MatchPos> = None;
            let cut_short = self.scan_range(start, end, &mut budget, |row, text| {
                if !line_may_match(text, needle, opts) {
                    return true;
                }
                for (col, len) in line_matches(text, opts, needle) {
                    if anchor.is_some_and(|(arow, acol)| row == arow && col >= acol) {
                        break;
                    }
                    best = Some(MatchPos {
                        row,
                        col_chars: col,
                        len_chars: len,
                    });
                }
                true
            });
            if let Some(m) = best {
                return Step::Found(m);
            }
            if cut_short {
                // Out of budget, or bytes we could not read: say so rather
                // than reporting an absence we never established.
                return Step::OutOfReach;
            }
            end = start;
        }
        Step::Absent
    }

    /// How many matches the budget could see, and whether it ran out.
    ///
    /// The editor counts every match on every keystroke, which is fine for a
    /// buffer held in memory and wrong for the file this view exists to avoid
    /// loading. A truncated count is reported AS truncated: the one outcome
    /// worth ruling out is a partial count presented as a total.
    pub fn count_matches(&self, needle: &str, opts: SearchOpts) -> (usize, bool) {
        if needle.is_empty() || self.is_empty() {
            return (0, false);
        }
        let mut count = 0usize;
        let mut budget = ScanBudget::new();
        let stopped_early = self.scan_from(0, &mut budget, |_row, text| {
            if line_may_match(text, needle, opts) {
                count = count.saturating_add(line_matches(text, opts, needle).len());
            }
            true
        });
        (count, stopped_early)
    }

    /// Whether a selection covers anything at all.
    pub fn has_selection(&self) -> bool {
        self.selection
            .is_some_and(|(a, b)| a != b)
    }

    /// The selection with its endpoints in reading order.
    fn ordered_selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let (a, b) = self.selection?;
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// The selected text, as the user sees it: escapes stripped, no colour.
    ///
    /// Returns the text and whether it was clamped at [`MAX_COPY_BYTES`]. A
    /// selection can span a file this view exists precisely to avoid loading,
    /// so the copy is bounded, and the bound is reported rather than leaving
    /// the clipboard quietly holding less than was selected.
    pub fn selection_text(&self) -> (String, bool) {
        let Some(((sr, sc), (er, ec))) = self.ordered_selection() else {
            return (String::new(), false);
        };
        let mut out = String::new();
        let mut clamped = false;
        let mut budget = ScanBudget::new_with(MAX_COPY_BYTES);
        // Read through the sweep path rather than the viewport cache: a
        // selection routinely runs past the window the reader is looking at,
        // and evicting that window to serve a copy would blank the screen.
        self.scan_range(sr, er + 1, &mut budget, |row, text| {
            let chars: Vec<char> = text.chars().collect();
            let from = if row == sr { sc.min(chars.len()) } else { 0 };
            let to = if row == er { ec.min(chars.len()) } else { chars.len() };
            if from < to {
                out.extend(&chars[from..to]);
            }
            if row < er {
                out.push('\n');
            }
            if out.len() >= MAX_COPY_BYTES {
                clamped = true;
                return false;
            }
            true
        });
        if out.len() > MAX_COPY_BYTES {
            out.truncate(MAX_COPY_BYTES);
            clamped = true;
        }
        (out, clamped)
    }

    /// The escape-free text of `idx`, for find, selection, copy, and
    /// `path:line` scanning — every consumer that must see what the user
    /// sees rather than the raw bytes.
    pub fn visible_text(&self, idx: usize) -> Option<&str> {
        self.line(idx).map(|l| l.text.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(body: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("app.log");
        let mut f = File::create(&p).unwrap();
        f.write_all(body).unwrap();
        (dir, p)
    }

    /// Find runs on the STRIPPED text: a needle that spans an escape in the
    /// raw bytes still matches, and a needle matching the escape's own bytes
    /// never does.
    #[test]
    fn find_matches_what_the_reader_sees_not_the_raw_bytes() {
        let body = b"alpha\n\x1b[31mred alert\x1b[0m\ntail\n";
        let (_d, p) = write_tmp(body);
        let v = LogView::open(&p).unwrap();
        let opts = SearchOpts::default();

        let m = v.find_next("red alert", opts, 0, 0, false).found().unwrap();
        assert_eq!(
            (m.row, m.col_chars, m.len_chars),
            (1, 0, 9),
            "the match is at column 0 of the stripped line, not past the escape"
        );
        assert!(
            v.find_next("[31m", opts, 0, 0, false).found().is_none(),
            "escape bytes are not searchable text"
        );
    }

    /// Stepping walks forward, then wraps, matching the editor's own find.
    #[test]
    fn stepping_walks_matches_and_wraps_at_the_end() {
        let (_d, p) = write_tmp(b"hit one\nmiss\nhit two\nmiss\n");
        let v = LogView::open(&p).unwrap();
        let opts = SearchOpts::default();

        let first = v.find_next("hit", opts, 0, 0, false).found().unwrap();
        assert_eq!(first.row, 0);
        let second = v
            .find_next("hit", opts, first.row, first.col_chars, true)
            .found()
            .unwrap();
        assert_eq!(second.row, 2, "skip_current steps past the anchor");
        let wrapped = v
            .find_next("hit", opts, second.row, second.col_chars, true)
            .found()
            .unwrap();
        assert_eq!(wrapped.row, 0, "past the last match, find wraps to the top");

        let back = v.find_prev("hit", opts, 2, 0).found().unwrap();
        assert_eq!(back.row, 0, "prev walks backwards");
        let back_wrapped = v.find_prev("hit", opts, 0, 0).found().unwrap();
        assert_eq!(
            back_wrapped.row, 2,
            "with nothing before the anchor, prev wraps to the last match"
        );
    }

    /// Two matches on one line are distinct positions, so stepping does not
    /// stall on the first one.
    #[test]
    fn two_matches_on_one_line_are_stepped_through_separately() {
        let (_d, p) = write_tmp(b"err and err again\n");
        let v = LogView::open(&p).unwrap();
        let opts = SearchOpts::default();
        let a = v.find_next("err", opts, 0, 0, false).found().unwrap();
        let b = v
            .find_next("err", opts, a.row, a.col_chars, true)
            .found()
            .unwrap();
        assert_eq!((a.col_chars, b.col_chars), (0, 8));
        assert_eq!(v.count_matches("err", opts), (2, false));
    }

    /// The strongest check available: a log's find must agree with the
    /// EDITOR's find, position for position, over every anchor.
    ///
    /// The editor searches an in-memory buffer with well-tested code, so any
    /// disagreement on a file small enough for both is a bug in this module.
    ///
    /// The fixture list matters as much as the comparison. My first version
    /// of this harness used only the multi-line case and PASSED against a
    /// real bug the review had already found: the backward wrap kept the
    /// anchor-column filter, so a match later on the anchor row was
    /// unreachable, and with several lines the wrap found a match on some
    /// other row and hid it. The single-line and single-match fixtures are
    /// the ones where the anchor row is the only place a match can come
    /// from, which is exactly where that class of bug lives.
    #[test]
    fn find_agrees_with_the_editors_find_from_every_anchor() {
        let fixtures: [&[&str]; 5] = [
            &["hit zero hit"],
            &["only one hit here"],
            &[
                "hit zero hit",
                "nothing here",
                "hit again",
                "",
                "trailing hit",
            ],
            &["no matches at all", "still nothing"],
            &["hit", "hit", "hit"],
        ];
        let opts = SearchOpts::default();
        for lines in fixtures {
            let body = lines.join("\n") + "\n";
            let (_d, p) = write_tmp(body.as_bytes());
            let v = LogView::open(&p).unwrap();
            let buf: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();

            for row in 0..lines.len() {
                for col in 0..=lines[row].chars().count() {
                    for skip in [false, true] {
                        let ours = v.find_next("hit", opts, row, col, skip).found();
                        let theirs = crate::widgets::editor_find::find_next_match(
                            &buf, "hit", opts, row, col, skip,
                        );
                        assert_eq!(
                            ours, theirs,
                            "find_next disagreed at row {row} col {col} skip {skip} on {lines:?}"
                        );
                    }
                    let ours = v.find_prev("hit", opts, row, col).found();
                    let theirs = crate::widgets::editor_find::find_prev_match(
                        &buf, "hit", opts, row, col, true,
                    );
                    assert_eq!(
                        ours, theirs,
                        "find_prev disagreed at row {row} col {col} on {lines:?}"
                    );
                }
            }
        }
    }

    /// A search that ran out of budget must not answer "no match": the file
    /// may well contain one further in. The third answer is the whole point
    /// of [`Step`].
    #[test]
    fn a_search_that_runs_out_of_budget_says_so_rather_than_absent() {
        let filler = "x".repeat(200);
        let mut body = String::new();
        for _ in 0..(FIND_SCAN_BYTES / filler.len() + 4000) {
            body.push_str(&filler);
            body.push('\n');
        }
        let (_d, p) = write_tmp(body.as_bytes());
        let v = LogView::open(&p).unwrap();
        let opts = SearchOpts::default();

        assert_eq!(
            v.find_next("needle", opts, 0, 0, false),
            Step::OutOfReach,
            "a needle past the budget is out of reach, not absent"
        );
        assert_eq!(
            v.find_prev("needle", opts, v.len() - 1, 0),
            Step::OutOfReach,
            "and the same walking backwards"
        );

        // A small file IS searched end to end, so absence there is real.
        let (_d2, small) = write_tmp(b"nothing here\n");
        let v2 = LogView::open(&small).unwrap();
        assert_eq!(v2.find_next("needle", opts, 0, 0, false), Step::Absent);
        assert_eq!(v2.find_prev("needle", opts, 0, 0), Step::Absent);
    }

    /// Review finding: with `skip_current` set, a lone match on the anchor
    /// row was skipped by the forward leg and the wrap was suppressed for
    /// `from_row == 0`, so Enter on a one-match log cleared the highlight
    /// and never found it again.
    #[test]
    fn stepping_wraps_back_onto_the_only_match_in_the_file() {
        let (_d, p) = write_tmp(b"only hit here\nnothing\n");
        let v = LogView::open(&p).unwrap();
        let opts = SearchOpts::default();
        let first = v.find_next("hit", opts, 0, 0, false).found().unwrap();
        assert_eq!((first.row, first.col_chars), (0, 5));
        let again = v
            .find_next("hit", opts, first.row, first.col_chars, true)
            .found()
            .expect("Enter must land back on the only match, not clear it");
        assert_eq!((again.row, again.col_chars), (0, 5));
    }

    /// Review finding: `find_prev` scanned forward from the head and spent
    /// its whole budget before reaching the anchor, so Shift+Enter did
    /// nothing on a large log. It now walks backwards from the anchor, so
    /// cost tracks the DISTANCE to the previous match.
    #[test]
    fn stepping_back_works_past_the_budget_from_the_head() {
        // Comfortably more than one budget of filler before the matches.
        let filler = "x".repeat(200);
        let mut body = String::new();
        for _ in 0..(FIND_SCAN_BYTES / filler.len() + 2000) {
            body.push_str(&filler);
            body.push('\n');
        }
        let before_marks = body.lines().count();
        body.push_str("needle one\n");
        body.push_str("filler\n");
        body.push_str("needle two\n");
        let (_d, p) = write_tmp(body.as_bytes());
        let v = LogView::open(&p).unwrap();

        let anchor = before_marks + 2;
        let prev = v
            .find_prev("needle", SearchOpts::default(), anchor, 0)
            .found()
            .expect("the previous match is two lines back, budget or not");
        assert_eq!(prev.row, before_marks);
    }

    /// Blocker: a line wider than the read window is searched only as far as
    /// the window reaches, so an answer drawn from it MUST report itself as
    /// partial. It used to come back as a complete count of zero.
    #[test]
    fn a_match_beyond_the_window_makes_the_count_report_itself_partial() {
        let mut body = vec![b'a'; WINDOW_BYTES + 4096];
        body.extend_from_slice(b"needle\n");
        let (_d, p) = write_tmp(&body);
        let v = LogView::open(&p).unwrap();
        let (count, truncated) = v.count_matches("needle", SearchOpts::default());
        assert!(
            truncated,
            "the needle sits in the dropped tail, so the count is partial: got {count} \
             reported as complete"
        );
    }

    /// The budget bounds IO, so it charges the RAW bytes read. Charging the
    /// stripped text let a densely coloured log read many times its budget:
    /// escapes are most of such a file and survive stripping as nothing.
    #[test]
    fn the_budget_charges_raw_bytes_not_what_survives_stripping() {
        // Each line is ~99% escape bytes: tiny stripped, huge raw.
        let noisy = "\u{1b}[31m\u{1b}[0m".repeat(200) + "x\n";
        let lines = (FIND_SCAN_BYTES / noisy.len()) + 200;
        let body = noisy.repeat(lines);
        let (_d, p) = write_tmp(body.as_bytes());
        let v = LogView::open(&p).unwrap();

        let (_count, truncated) = v.count_matches("zzz-not-present", SearchOpts::default());
        assert!(
            truncated,
            "a file this much larger than the budget must exhaust it; charging only \
             the stripped text would have read the whole file and called it complete"
        );
    }

    /// The count is budgeted, and a budgeted count says so. A partial total
    /// presented as a whole one is the failure this flag exists to prevent.
    #[test]
    fn a_count_that_outruns_its_budget_is_reported_as_truncated() {
        // Comfortably past FIND_SCAN_BYTES, with a match on every line.
        let line = "needle ".to_string() + &"x".repeat(120) + "\n";
        let lines = (FIND_SCAN_BYTES / line.len()) + 500;
        let body = line.repeat(lines);
        let (_d, p) = write_tmp(body.as_bytes());
        let v = LogView::open(&p).unwrap();
        let opts = SearchOpts::default();

        let (count, truncated) = v.count_matches("needle", opts);
        assert!(truncated, "the sweep must report that it stopped early");
        assert!(count > 0, "it still reports what it did see");
        assert!(
            count < lines,
            "a truncated count is a PARTIAL count: {count} of {lines} lines"
        );

        // A small file is counted exhaustively, so the flag stays off.
        let (_d2, small) = write_tmp(b"needle\nnothing\nneedle\n");
        let v2 = LogView::open(&small).unwrap();
        assert_eq!(v2.count_matches("needle", opts), (2, false));
    }

    /// A line wider than the read window is the JSON-log case. The sweep
    /// searches the prefix that fit and steps on rather than stalling.
    #[test]
    fn a_line_wider_than_the_window_does_not_stall_the_sweep() {
        let mut body = vec![b'a'; WINDOW_BYTES * 2];
        body.extend_from_slice(b"\nfindme\n");
        let (_d, p) = write_tmp(&body);
        let v = LogView::open(&p).unwrap();
        let opts = SearchOpts::default();
        let m = v.find_next("findme", opts, 0, 0, false).found();
        assert_eq!(
            m.map(|m| m.row),
            Some(1),
            "the oversized first line must not swallow the sweep"
        );
    }

    /// Searching must not disturb the window the reader is looking at: the
    /// cache is what the renderer paints from, and a sweep that evicted it
    /// would blank the screen behind the find bar.
    #[test]
    fn a_find_sweep_leaves_the_viewport_window_intact() {
        let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let (_d, p) = write_tmp(body.as_bytes());
        let mut v = LogView::open(&p).unwrap();
        v.ensure(400, 10).unwrap();
        assert_eq!(v.visible_text(400), Some("line 400"));
        let _ = v.find_next("line 12", SearchOpts::default(), 0, 0, false);
        assert_eq!(
            v.visible_text(400),
            Some("line 400"),
            "the sweep must not evict the cached viewport"
        );
    }

    #[test]
    fn indexes_lines_without_reading_the_file_into_memory() {
        let (_d, p) = write_tmp(b"one\ntwo\nthree\n");
        let v = LogView::open(&p).unwrap();
        assert_eq!(v.len(), 3, "a trailing newline is not a fourth line");
        assert!(!v.truncated);
    }

    #[test]
    fn a_file_without_a_trailing_newline_keeps_its_last_line() {
        let (_d, p) = write_tmp(b"a\nb");
        let mut v = LogView::open(&p).unwrap();
        assert_eq!(v.len(), 2);
        v.ensure(0, 2).unwrap();
        assert_eq!(v.visible_text(1), Some("b"));
    }

    #[test]
    fn windowed_reads_return_the_right_lines_and_strip_escapes() {
        let body = b"plain\n\x1b[31mred\x1b[0m\ntail\n";
        let (_d, p) = write_tmp(body);
        let mut v = LogView::open(&p).unwrap();
        v.ensure(0, 3).unwrap();
        assert_eq!(v.visible_text(0), Some("plain"));
        assert_eq!(v.visible_text(1), Some("red"), "escapes are stripped");
        assert_eq!(v.visible_text(2), Some("tail"));
        let spans = &v.line(1).unwrap().spans;
        assert_eq!(
            spans[0].style.fg,
            Some(crate::ansi_text::AnsiColor::Indexed(1))
        );
    }

    #[test]
    fn seeking_to_a_later_window_does_not_require_the_earlier_ones() {
        let mut body = Vec::new();
        for i in 0..5_000 {
            body.extend_from_slice(format!("line{i}\n").as_bytes());
        }
        let (_d, p) = write_tmp(&body);
        let mut v = LogView::open(&p).unwrap();
        // Jump straight to the tail without ever touching the head.
        v.ensure(4_990, 10).unwrap();
        assert_eq!(v.visible_text(4_990), Some("line4990"));
        assert_eq!(v.visible_text(4_999), Some("line4999"));
        assert_eq!(
            v.visible_text(0),
            None,
            "lines outside the window are not resident"
        );
    }

    #[test]
    fn crlf_lines_lose_their_carriage_return() {
        let (_d, p) = write_tmp(b"a\r\nb\r\n");
        let mut v = LogView::open(&p).unwrap();
        v.ensure(0, 2).unwrap();
        assert_eq!(v.visible_text(0), Some("a"), "no stray CR in the text");
        assert_eq!(v.visible_text(1), Some("b"));
    }

    #[test]
    fn an_empty_file_is_empty_rather_than_one_blank_line() {
        let (_d, p) = write_tmp(b"");
        let v = LogView::open(&p).unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn invalid_utf8_does_not_panic_and_still_yields_lines() {
        // Logs pick up stray bytes from binary payloads; lossy decoding must
        // keep the view usable rather than failing the open.
        let (_d, p) = write_tmp(b"good\n\xff\xfe bad\ntail\n");
        let mut v = LogView::open(&p).unwrap();
        v.ensure(0, 3).unwrap();
        assert_eq!(v.visible_text(0), Some("good"));
        assert!(v.visible_text(1).is_some());
        assert_eq!(v.visible_text(2), Some("tail"));
    }
}
