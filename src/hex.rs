//! Read-only hex viewer core: the fallback view every file the text
//! heuristic rejects lands in, instead of a "Binary file" error dead-end
//! (#172). Pure state + windowed file IO; the editor widget paints it and
//! the app routes keys, exactly like the sheet viewer split.
//!
//! The file is NEVER loaded whole: a small window around the viewport is
//! read on demand (seek + read), so a multi-gigabyte file opens instantly
//! and scrolling costs one bounded read per refill. Find streams the file
//! in chunks with an overlap, capped so a huge file cannot wedge the UI.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Widest layout: 16 bytes per row, VS Code hex-editor style. The render
/// drops to 8 when the pane is too narrow and writes the choice back into
/// [`HexView::bytes_per_row`], which navigation then uses (the
/// `last_wrap_rows` source-of-truth pattern).
pub const MAX_BYTES_PER_ROW: u64 = 16;

/// One window refill: enough for many screens, small enough to be
/// instant. Aligned down to a row boundary.
const WINDOW_BYTES: usize = 256 * 1024;

/// Find scans at most this many bytes per invocation, so a search over a
/// pathological file stays bounded; the caller reports the cap so the
/// result is never silently wrong.
pub const FIND_SCAN_CAP: u64 = 64 * 1024 * 1024;

/// Streaming find chunk size.
const FIND_CHUNK: usize = 1024 * 1024;

pub enum FindOutcome {
    Found(u64),
    NotFound,
    /// The scan hit [`FIND_SCAN_CAP`] before finding a match or covering
    /// the whole file.
    Capped,
}

/// Geometry of the last painted frame, written by the render and read by
/// mouse hit-testing. Frame truth: refreshed every paint.
#[derive(Clone, Copy, Default)]
pub struct HexLayout {
    pub data_top: u16,
    pub data_rows: u16,
    pub hex_x: u16,
    pub ascii_x: u16,
}

pub struct HexView {
    pub file_len: u64,
    /// First visible 16-byte (or 8-byte) row.
    pub top_row: u64,
    /// Cursor byte offset; kept `< file_len.max(1)`.
    pub cursor: u64,
    /// Selection anchor offset (Shift-extend / drag start). The selection
    /// is the inclusive byte span between the anchor and the cursor.
    pub sel_anchor: Option<u64>,
    /// Bytes-per-row the render actually used; navigation follows it.
    pub bytes_per_row: u64,
    /// Last submitted find query, for "find next".
    pub last_find: Option<Vec<u8>>,
    pub layout: HexLayout,
    /// Overwrite-mode edits not yet written to disk (#173): offset →
    /// replacement byte, sparse over the windowed reader so a
    /// multi-gigabyte file carries kilobytes of state. No inserts or
    /// deletes — offsets never shift.
    pub edits: std::collections::BTreeMap<u64, u8>,
    /// The half-typed high nibble at the cursor in the hex pane.
    pub pending_nibble: Option<u8>,
    /// Which pane receives typed input: the hex grid (false) or the
    /// ASCII gutter (true). Tab toggles.
    pub ascii_focus: bool,
    /// Filesystem write permission, probed at open/refresh: typing into
    /// a read-only file refuses up front instead of failing at save.
    pub read_only: bool,
    undo: Vec<(u64, Option<u8>)>,
    redo: Vec<(u64, Option<u8>)>,
    window_start: u64,
    window: Vec<u8>,
    handle: File,
}

impl HexView {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let handle = File::open(path)?;
        let meta = handle.metadata()?;
        let file_len = meta.len();
        let read_only = meta.permissions().readonly();
        let mut view = Self {
            file_len,
            top_row: 0,
            cursor: 0,
            sel_anchor: None,
            bytes_per_row: MAX_BYTES_PER_ROW,
            last_find: None,
            layout: HexLayout::default(),
            edits: std::collections::BTreeMap::new(),
            pending_nibble: None,
            ascii_focus: false,
            read_only,
            undo: Vec::new(),
            redo: Vec::new(),
            window_start: 0,
            window: Vec::new(),
            handle,
        };
        view.fill_window(0)?;
        Ok(view)
    }

    /// Total rows at the current `bytes_per_row` (at least one, so an
    /// empty file still paints a frame the cursor can live on).
    pub fn total_rows(&self) -> u64 {
        self.file_len.div_ceil(self.bytes_per_row).max(1)
    }

    /// Offset-column width in hex digits: at least 8, growing for files
    /// whose last offset does not fit (10+ GB).
    pub fn offset_digits(&self) -> usize {
        let need = format!("{:x}", self.file_len.saturating_sub(1)).len();
        need.max(8)
    }

    fn fill_window(&mut self, start: u64) -> std::io::Result<()> {
        let aligned = start - (start % (MAX_BYTES_PER_ROW * 64));
        let len = (self.file_len.saturating_sub(aligned)).min(WINDOW_BYTES as u64) as usize;
        let mut buf = vec![0u8; len];
        self.handle.seek(SeekFrom::Start(aligned))?;
        self.handle.read_exact(&mut buf)?;
        self.window_start = aligned;
        self.window = buf;
        Ok(())
    }

    /// Make sure the span `[first_row, first_row + rows)` is resident.
    /// IO errors leave the previous window in place (the render paints
    /// blanks for missing bytes rather than the app crashing on a file
    /// truncated underneath us; the FS-sync sweep reconciles).
    pub fn ensure_visible(&mut self, first_row: u64, rows: usize) {
        let start = first_row * self.bytes_per_row;
        let end = ((first_row + rows as u64) * self.bytes_per_row).min(self.file_len);
        if start >= self.window_start && end <= self.window_start + self.window.len() as u64 {
            return;
        }
        // Center the window on the viewport so small scrolls in either
        // direction stay resident.
        let half = (WINDOW_BYTES as u64) / 2;
        let centered = start.saturating_sub(half);
        let _ = self.fill_window(centered.min(self.file_len.saturating_sub(1)));
    }

    pub fn byte(&self, off: u64) -> Option<u8> {
        if off < self.window_start || off >= self.file_len {
            return None;
        }
        self.window.get((off - self.window_start) as usize).copied()
    }

    /// Selection as a half-open byte range, cursor-inclusive.
    pub fn selection(&self) -> Option<(u64, u64)> {
        let anchor = self.sel_anchor?;
        let (a, b) = if anchor <= self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        };
        Some((a, (b + 1).min(self.file_len.max(1))))
    }

    /// Move the cursor by a signed byte delta, extending the selection
    /// when `select` (Shift) is held, and scroll the viewport to keep the
    /// cursor visible within `viewport_rows`.
    pub fn move_cursor(&mut self, delta: i64, select: bool, viewport_rows: usize) {
        if select {
            if self.sel_anchor.is_none() {
                self.sel_anchor = Some(self.cursor);
            }
        } else {
            self.sel_anchor = None;
        }
        let max = self.file_len.saturating_sub(1);
        self.cursor = if delta.is_negative() {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            self.cursor.saturating_add(delta as u64).min(max)
        };
        self.scroll_cursor_into_view(viewport_rows);
    }

    pub fn set_cursor(&mut self, off: u64, select: bool, viewport_rows: usize) {
        if select {
            if self.sel_anchor.is_none() {
                self.sel_anchor = Some(self.cursor);
            }
        } else {
            self.sel_anchor = None;
        }
        self.cursor = off.min(self.file_len.saturating_sub(1));
        self.scroll_cursor_into_view(viewport_rows);
    }

    fn scroll_cursor_into_view(&mut self, viewport_rows: usize) {
        let rows = viewport_rows.max(1) as u64;
        let cursor_row = self.cursor / self.bytes_per_row;
        if cursor_row < self.top_row {
            self.top_row = cursor_row;
        } else if cursor_row >= self.top_row + rows {
            self.top_row = cursor_row - rows + 1;
        }
        self.clamp_scroll(viewport_rows);
        self.ensure_visible(self.top_row, viewport_rows.max(1));
    }

    /// Scroll without moving the cursor (mouse wheel).
    pub fn scroll_by(&mut self, delta: i64, viewport_rows: usize) {
        self.top_row = if delta.is_negative() {
            self.top_row.saturating_sub(delta.unsigned_abs())
        } else {
            self.top_row.saturating_add(delta as u64)
        };
        self.clamp_scroll(viewport_rows);
        self.ensure_visible(self.top_row, viewport_rows.max(1));
    }

    fn clamp_scroll(&mut self, viewport_rows: usize) {
        let rows = viewport_rows.max(1) as u64;
        let max_top = self.total_rows().saturating_sub(rows);
        if self.top_row > max_top {
            self.top_row = max_top;
        }
    }

    /// FS-sync reload: pick up a new length, drop the stale window, keep
    /// (clamped) the reader's place. The handle is reopened so a file
    /// replaced by rename (the common atomic-write) is followed.
    pub fn refresh_from_disk(&mut self, path: &Path) -> std::io::Result<()> {
        self.handle = File::open(path)?;
        let meta = self.handle.metadata()?;
        self.file_len = meta.len();
        self.read_only = meta.permissions().readonly();
        self.window.clear();
        self.window_start = 0;
        self.cursor = self.cursor.min(self.file_len.saturating_sub(1));
        if let Some(a) = self.sel_anchor {
            self.sel_anchor = Some(a.min(self.file_len.saturating_sub(1)));
        }
        // The sweep only reloads a CLEAN tab, but belt-and-braces: a
        // reload means disk changed, and pending overwrites measured
        // against the old bytes must not survive onto the new ones.
        self.edits.clear();
        self.undo.clear();
        self.redo.clear();
        self.pending_nibble = None;
        self.fill_window(self.cursor)?;
        Ok(())
    }

    /// Parse a find query: whitespace-separated hex byte pairs when the
    /// whole query reads as hex ("de ad be ef", "DEADBEEF"), else the
    /// query's literal UTF-8 bytes.
    pub fn parse_find_query(q: &str) -> Option<Vec<u8>> {
        let stripped: String = q.chars().filter(|c| !c.is_whitespace()).collect();
        if !stripped.is_empty()
            && stripped.len().is_multiple_of(2)
            && stripped.chars().all(|c| c.is_ascii_hexdigit())
        {
            let bytes = (0..stripped.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&stripped[i..i + 2], 16).unwrap())
                .collect();
            return Some(bytes);
        }
        if q.is_empty() {
            return None;
        }
        Some(q.as_bytes().to_vec())
    }

    /// Streaming forward search from `from` (exclusive of a match AT
    /// `from` only when wrapping lands back on it), wrapping at EOF, byte
    /// budget capped at [`FIND_SCAN_CAP`].
    pub fn find_forward(&mut self, needle: &[u8], from: u64) -> std::io::Result<FindOutcome> {
        if needle.is_empty() || self.file_len == 0 || needle.len() as u64 > self.file_len {
            return Ok(FindOutcome::NotFound);
        }
        let mut scanned = 0u64;
        // Two passes: [from, EOF) then [0, from + needle - 1) for matches
        // straddling the wrap point's left side.
        let spans = [
            (from, self.file_len),
            (0, (from + needle.len() as u64 - 1).min(self.file_len)),
        ];
        for (mut pos, end) in spans {
            let mut carry: Vec<u8> = Vec::new();
            while pos < end {
                if scanned >= FIND_SCAN_CAP {
                    return Ok(FindOutcome::Capped);
                }
                let want = ((end - pos).min(FIND_CHUNK as u64)) as usize;
                let mut chunk = vec![0u8; want];
                self.handle.seek(SeekFrom::Start(pos))?;
                self.handle.read_exact(&mut chunk)?;
                scanned += want as u64;
                let hay = if carry.is_empty() {
                    chunk.clone()
                } else {
                    let mut joined = carry.clone();
                    joined.extend_from_slice(&chunk);
                    joined
                };
                if let Some(i) = hay.windows(needle.len()).position(|w| w == needle) {
                    let base = pos - carry.len() as u64;
                    return Ok(FindOutcome::Found(base + i as u64));
                }
                carry = hay[hay.len().saturating_sub(needle.len() - 1)..].to_vec();
                pos += want as u64;
            }
        }
        Ok(FindOutcome::NotFound)
    }

    /// The byte the user SEES at `off`: a pending edit wins over the
    /// disk window.
    pub fn effective_byte(&self, off: u64) -> Option<u8> {
        self.edits.get(&off).copied().or_else(|| self.byte(off))
    }

    /// True when unwritten edits exist — the tab's dirty state.
    pub fn has_edits(&self) -> bool {
        !self.edits.is_empty()
    }

    /// Record an overwrite at `off` (undoable; a new edit clears the
    /// redo branch, text-editor style). Offsets past EOF are ignored —
    /// overwrite mode never grows the file.
    pub fn apply_edit(&mut self, off: u64, b: u8) {
        if off >= self.file_len {
            return;
        }
        self.undo.push((off, self.edits.get(&off).copied()));
        self.redo.clear();
        self.edits.insert(off, b);
    }

    /// Discard every pending edit and the whole undo history — the
    /// "Revert" half of a conflict resolution, called BEFORE the reload
    /// so the refresh guard in `open_hex` lets the reload through.
    pub fn discard_edits(&mut self) {
        self.edits.clear();
        self.undo.clear();
        self.redo.clear();
        self.pending_nibble = None;
    }

    /// Drop the pending edit at `off` (Delete: revert one byte to disk).
    pub fn revert_edit(&mut self, off: u64) {
        if let Some(prev) = self.edits.remove(&off) {
            self.undo.push((off, Some(prev)));
            self.redo.clear();
        }
    }

    pub fn undo_edit(&mut self) -> bool {
        let Some((off, prev)) = self.undo.pop() else {
            return false;
        };
        let current = self.edits.get(&off).copied();
        match prev {
            Some(b) => {
                self.edits.insert(off, b);
            }
            None => {
                self.edits.remove(&off);
            }
        }
        self.redo.push((off, current));
        true
    }

    pub fn redo_edit(&mut self) -> bool {
        let Some((off, value)) = self.redo.pop() else {
            return false;
        };
        let current = self.edits.get(&off).copied();
        match value {
            Some(b) => {
                self.edits.insert(off, b);
            }
            None => {
                self.edits.remove(&off);
            }
        }
        self.undo.push((off, current));
        true
    }

    /// Feed one typed hex digit at the cursor. Two digits complete a
    /// byte (high nibble first, the universal hex-editor convention),
    /// which is applied and reported so the caller advances the cursor.
    pub fn type_hex_digit(&mut self, digit: u8) -> bool {
        match self.pending_nibble.take() {
            Some(hi) => {
                self.apply_edit(self.cursor, (hi << 4) | digit);
                true
            }
            None => {
                self.pending_nibble = Some(digit);
                false
            }
        }
    }

    /// Write every pending edit into the file IN PLACE (the hex-editor
    /// semantic: an atomic-rename would mean copying a possibly
    /// multi-gigabyte file for a two-byte change), clear the edit state,
    /// and refresh the window so the view reads what disk now holds.
    /// Returns how many bytes were written.
    pub fn save_edits(&mut self, path: &Path) -> std::io::Result<usize> {
        use std::io::Write as _;
        if self.edits.is_empty() {
            return Ok(0);
        }
        let mut f = std::fs::OpenOptions::new().write(true).open(path)?;
        let mut written = 0usize;
        for (&off, &b) in self.edits.iter() {
            f.seek(SeekFrom::Start(off))?;
            f.write_all(&[b])?;
            written += 1;
        }
        f.flush()?;
        drop(f);
        self.edits.clear();
        self.undo.clear();
        self.redo.clear();
        self.pending_nibble = None;
        // Re-read through the ORIGINAL handle's window so the view shows
        // disk truth (the write went through a second handle).
        let refill = self.window_start;
        self.fill_window(refill)?;
        Ok(written)
    }

    /// Map a screen cell to the byte offset it paints, using the last
    /// frame's layout (frame truth). Both the hex grid and the ASCII
    /// gutter are live click targets; cells past EOF miss.
    pub fn hit_test(&self, col: u16, row: u16) -> Option<u64> {
        let l = self.layout;
        if l.data_rows == 0 || row < l.data_top || row >= l.data_top + l.data_rows {
            return None;
        }
        let r = self.top_row + (row - l.data_top) as u64;
        let base = r.checked_mul(self.bytes_per_row)?;
        let mid: u16 = if self.bytes_per_row == 16 { 1 } else { 0 };
        let i = if col >= l.ascii_x {
            let i = (col - l.ascii_x) as u64;
            (i < self.bytes_per_row).then_some(i)?
        } else if col >= l.hex_x {
            let rel = col - l.hex_x;
            let rel = if mid == 1 && rel >= 8 * 3 {
                rel - 1
            } else {
                rel
            };
            let i = (rel / 3) as u64;
            (i < self.bytes_per_row).then_some(i)?
        } else {
            return None;
        };
        let off = base + i;
        (off < self.file_len).then_some(off)
    }

    /// Status-bar segment: cursor offset (hex + decimal), selection
    /// length when one exists, the file size, the active input pane,
    /// and the unsaved-edit / read-only state.
    pub fn status_line(&self) -> String {
        let sel = match self.selection() {
            Some((a, b)) if b > a => format!("  ·  {} bytes selected", b - a),
            _ => String::new(),
        };
        let pane = if self.ascii_focus {
            "  ·  text input (Tab: hex)"
        } else {
            "  ·  hex input (Tab: text)"
        };
        let dirty = if self.read_only {
            String::from("  ·  read-only")
        } else if !self.edits.is_empty() {
            format!("  ·  {} unsaved (Cmd+S)", self.edits.len())
        } else {
            String::new()
        };
        format!(
            "0x{:0w$X} ({}){}  ·  {} bytes{}{}",
            self.cursor,
            self.cursor,
            sel,
            self.file_len,
            pane,
            dirty,
            w = self.offset_digits(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn view_over(bytes: &[u8]) -> (tempfile::TempDir, HexView) {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("blob.bin");
        let mut f = File::create(&p).unwrap();
        f.write_all(bytes).unwrap();
        drop(f);
        (tmp, HexView::open(&p).unwrap())
    }

    #[test]
    fn edits_overlay_disk_bytes_and_undo_redo_walk_the_history() {
        let (_t, mut v) = view_over(&[10, 20, 30, 40]);
        assert_eq!(v.effective_byte(1), Some(20));
        v.apply_edit(1, 99);
        v.apply_edit(1, 77);
        assert_eq!(v.effective_byte(1), Some(77), "latest edit wins");
        assert!(v.has_edits());
        assert!(v.undo_edit());
        assert_eq!(
            v.effective_byte(1),
            Some(99),
            "undo steps to the prior edit"
        );
        assert!(v.undo_edit());
        assert_eq!(v.effective_byte(1), Some(20), "second undo restores disk");
        assert!(!v.has_edits());
        assert!(!v.undo_edit(), "history exhausted");
        assert!(v.redo_edit());
        assert_eq!(v.effective_byte(1), Some(99));
        v.apply_edit(2, 5);
        assert!(!v.redo_edit(), "a new edit clears the redo branch");
        v.revert_edit(2);
        assert_eq!(
            v.effective_byte(2),
            Some(30),
            "revert drops the pending byte"
        );
        assert!(v.undo_edit());
        assert_eq!(v.effective_byte(2), Some(5), "revert itself is undoable");
    }

    #[test]
    fn overwrite_mode_never_grows_the_file() {
        let (_t, mut v) = view_over(&[1, 2, 3]);
        v.apply_edit(3, 9);
        v.apply_edit(100, 9);
        assert!(!v.has_edits(), "past-EOF edits are refused");
    }

    #[test]
    fn hex_digit_typing_pairs_nibbles_high_first() {
        let (_t, mut v) = view_over(&[0u8; 8]);
        assert!(!v.type_hex_digit(0x4), "first nibble only latches");
        assert_eq!(v.pending_nibble, Some(0x4));
        assert!(v.type_hex_digit(0x1), "second nibble completes the byte");
        assert_eq!(v.effective_byte(0), Some(0x41));
        assert_eq!(v.pending_nibble, None);
    }

    #[test]
    fn save_edits_writes_in_place_and_the_view_reads_disk_truth() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("blob.bin");
        std::fs::write(&p, vec![0u8; 64]).unwrap();
        let mut v = HexView::open(&p).unwrap();
        v.apply_edit(0, 0xAA);
        v.apply_edit(63, 0xBB);
        let written = v.save_edits(&p).unwrap();
        assert_eq!(written, 2);
        assert!(!v.has_edits());
        let disk = std::fs::read(&p).unwrap();
        assert_eq!(disk[0], 0xAA);
        assert_eq!(disk[63], 0xBB);
        assert_eq!(disk.len(), 64, "in-place: length untouched");
        assert_eq!(v.byte(0), Some(0xAA), "window refreshed from disk");
    }

    #[test]
    fn opens_reading_only_a_window_of_a_large_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("big.bin");
        let f = File::create(&p).unwrap();
        f.set_len(1 << 30).unwrap(); // 1 GiB sparse: instant, no data IO
        drop(f);
        let v = HexView::open(&p).unwrap();
        assert_eq!(v.file_len, 1 << 30);
        assert!(v.window.len() <= WINDOW_BYTES);
        assert_eq!(v.byte(0), Some(0));
    }

    #[test]
    fn scrolling_refills_the_window_around_the_viewport() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("big.bin");
        let mut f = File::create(&p).unwrap();
        // 2 MiB patterned so bytes identify their offset.
        let data: Vec<u8> = (0..2 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
        f.write_all(&data).unwrap();
        drop(f);
        let mut v = HexView::open(&p).unwrap();
        let far_row = (1_500_000u64) / MAX_BYTES_PER_ROW;
        v.ensure_visible(far_row, 40);
        let off = far_row * MAX_BYTES_PER_ROW;
        assert_eq!(v.byte(off), Some((off % 251) as u8), "far byte resident");
    }

    #[test]
    fn cursor_moves_clamp_and_scroll_the_viewport() {
        let (_t, mut v) = view_over(&[0u8; 100]);
        v.move_cursor(-5, false, 4);
        assert_eq!(v.cursor, 0, "clamped at start");
        v.move_cursor(1000, false, 4);
        assert_eq!(v.cursor, 99, "clamped at last byte");
        let cursor_row = v.cursor / v.bytes_per_row;
        assert!(
            (v.top_row..v.top_row + 4).contains(&cursor_row),
            "viewport follows the cursor"
        );
    }

    #[test]
    fn shift_extends_a_selection_and_plain_moves_drop_it() {
        let (_t, mut v) = view_over(&[0u8; 64]);
        v.move_cursor(10, false, 8);
        v.move_cursor(5, true, 8);
        assert_eq!(v.selection(), Some((10, 16)), "anchor..cursor inclusive");
        v.move_cursor(-20, true, 8);
        assert_eq!(
            v.selection(),
            Some((0, 11)),
            "backward selection swaps ends"
        );
        v.move_cursor(1, false, 8);
        assert_eq!(v.selection(), None, "plain move clears");
    }

    #[test]
    fn parse_find_query_reads_hex_pairs_else_literal_ascii() {
        assert_eq!(
            HexView::parse_find_query("de ad BE ef"),
            Some(vec![0xde, 0xad, 0xbe, 0xef])
        );
        assert_eq!(HexView::parse_find_query("cafe"), Some(vec![0xca, 0xfe]));
        assert_eq!(
            HexView::parse_find_query("hello!"),
            Some(b"hello!".to_vec()),
            "odd/non-hex falls back to ASCII"
        );
        assert_eq!(
            HexView::parse_find_query("abz"),
            Some(b"abz".to_vec()),
            "non-hex chars force ASCII even at even length"
        );
        assert_eq!(HexView::parse_find_query(""), None);
    }

    #[test]
    fn find_streams_across_chunk_boundaries_and_wraps() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("hay.bin");
        let mut data = vec![0u8; 3 * FIND_CHUNK / 2];
        // Straddle the first chunk boundary exactly.
        let at = FIND_CHUNK - 2;
        data[at..at + 4].copy_from_slice(b"NEED");
        let mut f = File::create(&p).unwrap();
        f.write_all(&data).unwrap();
        drop(f);
        let mut v = HexView::open(&p).unwrap();
        match v.find_forward(b"NEED", 0).unwrap() {
            FindOutcome::Found(off) => assert_eq!(off, at as u64),
            _ => panic!("must find across the chunk boundary"),
        }
        // Wrap: searching from past the match circles around to it.
        match v.find_forward(b"NEED", (at + 10) as u64).unwrap() {
            FindOutcome::Found(off) => assert_eq!(off, at as u64),
            _ => panic!("must wrap to a match before the start offset"),
        }
        match v.find_forward(b"ABSENT", 0).unwrap() {
            FindOutcome::NotFound => {}
            _ => panic!("absent needle reports NotFound"),
        }
    }

    #[test]
    fn refresh_from_disk_follows_truncation_and_rename() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("blob.bin");
        std::fs::write(&p, vec![7u8; 4096]).unwrap();
        let mut v = HexView::open(&p).unwrap();
        v.set_cursor(4000, false, 10);
        // Atomic-write style replace: new inode, shorter file.
        let tmp2 = tmp.path().join("blob.new");
        std::fs::write(&tmp2, vec![9u8; 100]).unwrap();
        std::fs::rename(&tmp2, &p).unwrap();
        v.refresh_from_disk(&p).unwrap();
        assert_eq!(v.file_len, 100);
        assert_eq!(v.cursor, 99, "cursor clamped into the new length");
        assert_eq!(v.byte(0), Some(9), "window reads the NEW inode's bytes");
    }

    #[test]
    fn empty_file_still_has_one_row_and_a_parked_cursor() {
        let (_t, mut v) = view_over(&[]);
        assert_eq!(v.total_rows(), 1);
        v.move_cursor(5, false, 4);
        assert_eq!(v.cursor, 0);
        assert!(v.status_line().contains("0 bytes"));
    }

    #[test]
    fn offset_digits_grow_past_4gb() {
        let (_t, v) = view_over(&[0u8; 16]);
        assert_eq!(v.offset_digits(), 8);
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("huge.bin");
        let f = File::create(&p).unwrap();
        f.set_len((1u64 << 32) + 5).unwrap();
        drop(f);
        let v = HexView::open(&p).unwrap();
        assert_eq!(v.offset_digits(), 9);
    }
}
