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

use crate::ansi_text::{AnsiLine, AnsiStyle, parse_line};

/// One window refill: many screens' worth, still instant to read.
const WINDOW_BYTES: usize = 256 * 1024;

/// Index at most this many lines. At 8 bytes each this caps index memory
/// around 80 MB, which a 2 GB log of short lines could otherwise blow past.
/// Beyond it the view is explicitly truncated, never silently short.
pub const MAX_INDEXED_LINES: usize = 10_000_000;

/// Bytes read while building the index, per chunk.
const INDEX_CHUNK: usize = 1024 * 1024;

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
