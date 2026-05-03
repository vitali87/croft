use anyhow::Result;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
use std::path::{Path, PathBuf};

use crate::highlight::{
    compute_line_starts, highlight_text, lang_for_extension, HiSpan, LangKind, LangRegistry,
};

const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

pub struct Editor {
    pub path: Option<PathBuf>,
    pub lines: Vec<String>,
    pub scroll: usize,
    /// Cursor column as a CHARACTER index (not bytes), for the current line.
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub focused: bool,
    pub dirty: bool,
    pub status: String,
    pub last_area: Rect,
    pub last_inner: Rect,
    pub last_gutter_width: u16,
    lang: Option<LangKind>,
    highlights: Vec<Vec<HiSpan>>,
    registry: LangRegistry,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            path: None,
            lines: Vec::new(),
            scroll: 0,
            cursor_row: 0,
            cursor_col: 0,
            focused: false,
            dirty: false,
            status: String::from("No file open"),
            last_area: Rect::default(),
            last_inner: Rect::default(),
            last_gutter_width: 0,
            lang: None,
            highlights: Vec::new(),
            registry: LangRegistry::new(),
        }
    }

    pub fn open(&mut self, path: &Path) -> Result<()> {
        let meta = std::fs::metadata(path)?;
        if meta.len() > MAX_FILE_BYTES {
            anyhow::bail!("File too large ({} bytes)", meta.len());
        }
        let bytes = std::fs::read(path)?;
        if is_binary(&bytes) {
            anyhow::bail!("Binary file");
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.lines = text.lines().map(|s| s.to_string()).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.path = Some(path.to_path_buf());
        self.lang = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(lang_for_extension);
        self.scroll = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.dirty = false;
        self.status = format!("Opened {}", path.display());
        self.recompute_highlights();
        Ok(())
    }

    fn recompute_highlights(&mut self) {
        match self.lang {
            Some(kind) => {
                let text = self.lines.join("\n");
                let bytes = text.as_bytes();
                let line_starts = compute_line_starts(bytes);
                self.highlights = highlight_text(&mut self.registry, kind, bytes, &line_starts);
            }
            None => {
                self.highlights = vec![Vec::new(); self.lines.len()];
            }
        }
    }

    fn line_char_len(&self, row: usize) -> usize {
        self.lines.get(row).map(|s| s.chars().count()).unwrap_or(0)
    }

    fn byte_index(&self, row: usize, col: usize) -> usize {
        let line = &self.lines[row];
        line.char_indices()
            .nth(col)
            .map(|(i, _)| i)
            .unwrap_or(line.len())
    }

    pub fn insert_char(&mut self, c: char) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        let byte = self.byte_index(row, col);
        self.lines[row].insert(byte, c);
        self.cursor_col += 1;
        self.dirty = true;
        self.recompute_highlights();
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            if c == '\n' {
                self.insert_newline();
            } else {
                self.insert_char(c);
            }
        }
    }

    pub fn insert_newline(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        let byte = self.byte_index(row, col);
        let right = self.lines[row].split_off(byte);
        self.lines.insert(row + 1, right);
        self.cursor_row += 1;
        self.cursor_col = 0;
        self.dirty = true;
        self.recompute_highlights();
    }

    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            let row = self.cursor_row;
            let col = self.cursor_col - 1;
            let from = self.byte_index(row, col);
            let to = self.byte_index(row, col + 1);
            self.lines[row].replace_range(from..to, "");
            self.cursor_col -= 1;
            self.dirty = true;
        } else if self.cursor_row > 0 {
            let cur = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.line_char_len(self.cursor_row);
            self.lines[self.cursor_row].push_str(&cur);
            self.dirty = true;
        }
        self.recompute_highlights();
    }

    pub fn delete_forward(&mut self) {
        let row = self.cursor_row;
        let len = self.line_char_len(row);
        if self.cursor_col < len {
            let from = self.byte_index(row, self.cursor_col);
            let to = self.byte_index(row, self.cursor_col + 1);
            self.lines[row].replace_range(from..to, "");
            self.dirty = true;
        } else if row + 1 < self.lines.len() {
            let next = self.lines.remove(row + 1);
            self.lines[row].push_str(&next);
            self.dirty = true;
        }
        self.recompute_highlights();
    }

    /// Returns true if the on-disk file at `event_path` is the file currently
    /// open in this editor (used by the filesystem watcher to decide whether
    /// to reload).
    pub fn matches_open_path(&self, event_path: &Path) -> bool {
        let Some(open) = self.path.as_ref() else {
            return false;
        };
        if open == event_path {
            return true;
        }
        if let (Ok(a), Ok(b)) = (open.canonicalize(), event_path.canonicalize()) {
            return a == b;
        }
        false
    }

    /// Reload from disk *only if* there are no unsaved local edits. Returns
    /// `Some(Ok(()))` if a reload happened, `Some(Err(_))` if reload failed,
    /// `None` if reload was skipped because the buffer is dirty (caller
    /// should surface a "file changed on disk" warning instead).
    pub fn reload_if_clean(&mut self) -> Option<Result<()>> {
        if self.dirty {
            return None;
        }
        let path = self.path.as_ref().cloned()?;
        let prev_row = self.cursor_row;
        let prev_col = self.cursor_col;
        let prev_scroll = self.scroll;
        let result = self.open(&path);
        // Clamp the restored cursor to the new contents so it stays valid
        // even if the file shrank.
        self.cursor_row = prev_row.min(self.lines.len().saturating_sub(1));
        self.cursor_col = prev_col.min(self.line_char_len(self.cursor_row));
        self.scroll = prev_scroll.min(self.lines.len().saturating_sub(1));
        Some(result)
    }

    pub fn save_to_disk(&mut self) -> Result<()> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No file open"))?
            .clone();
        let content = self.lines.join("\n");
        std::fs::write(&path, content)?;
        self.dirty = false;
        self.status = format!("Saved {}", path.display());
        Ok(())
    }

    pub fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.line_char_len(self.cursor_row);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor_col < self.line_char_len(self.cursor_row) {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    /// One screen worth of rows, derived from the editor's last rendered
    /// inner height.  Falls back to a sensible default before the first
    /// render (when `last_inner.height` is still 0).
    pub fn page_size(&self) -> usize {
        let from_inner = self.last_inner.height as usize;
        if from_inner > 0 {
            from_inner
        } else {
            20
        }
    }

    /// Move the viewport down by exactly one screen so the first
    /// previously-unseen row becomes the new top of the viewport, and place
    /// the cursor on that new top row.  Clamps at end of file.
    pub fn page_down_one_screen(&mut self) {
        let page = self.page_size();
        let max_row = self.lines.len().saturating_sub(1);
        let new_top = (self.scroll + page).min(max_row);
        self.scroll = new_top;
        self.cursor_row = new_top;
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
    }

    /// Move the viewport up by exactly one screen so the new top is `page`
    /// rows above the previous top.  Cursor lands on the new top row.
    /// Clamps at the start of file.
    pub fn page_up_one_screen(&mut self) {
        let page = self.page_size();
        let new_top = self.scroll.saturating_sub(page);
        self.scroll = new_top;
        self.cursor_row = new_top;
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
    }

    pub fn home_line(&mut self) {
        self.cursor_col = 0;
    }

    pub fn end_line(&mut self) {
        self.cursor_col = self.line_char_len(self.cursor_row);
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.cursor_row = self.cursor_row.saturating_sub(n);
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.cursor_row = (self.cursor_row + n).min(self.lines.len().saturating_sub(1));
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
    }

    /// Move the cursor to the screen coordinates (col, row). Used for mouse clicks.
    pub fn click(&mut self, col: u16, row: u16) {
        if self.lines.is_empty() || self.last_inner.height == 0 {
            return;
        }
        if row < self.last_inner.y || row >= self.last_inner.y + self.last_inner.height {
            return;
        }
        let row_idx = (row - self.last_inner.y) as usize;
        let target_line = (self.scroll + row_idx).min(self.lines.len().saturating_sub(1));
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        let target_col = if col < text_x {
            0
        } else {
            (col - text_x) as usize
        };
        self.cursor_row = target_line;
        self.cursor_col = target_col.min(self.line_char_len(target_line));
    }
}

fn is_binary(data: &[u8]) -> bool {
    let sample = &data[..data.len().min(4096)];
    if sample.contains(&0) {
        return true;
    }
    if sample.is_empty() {
        return false;
    }
    let nontext = sample
        .iter()
        .filter(|&&b| !(b >= 0x20 || matches!(b, b'\n' | b'\r' | b'\t' | 0x0c | 0x08)))
        .count();
    (nontext as f32 / sample.len() as f32) > 0.30
}

/// Build a Vec<Span> from a line and its byte-range highlight spans.
fn build_line_spans<'a>(line: &'a str, spans: &[HiSpan]) -> Vec<Span<'a>> {
    if spans.is_empty() {
        return vec![Span::raw(line)];
    }
    let mut out: Vec<Span> = Vec::with_capacity(spans.len() * 2);
    let mut cursor = 0usize;
    for sp in spans {
        if sp.start > cursor && sp.start <= line.len() {
            let slice = &line[cursor..sp.start];
            if !slice.is_empty() {
                out.push(Span::raw(slice));
            }
        }
        let s = sp.start.min(line.len());
        let e = sp.end.min(line.len());
        if e > s {
            out.push(Span::styled(&line[s..e], sp.style));
            cursor = e;
        }
    }
    if cursor < line.len() {
        out.push(Span::raw(&line[cursor..]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn editor_with(text: &str) -> Editor {
        let mut e = Editor::new();
        e.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.lines().map(|s| s.to_string()).collect()
        };
        if e.lines.is_empty() {
            e.lines.push(String::new());
        }
        e
    }

    #[test]
    fn is_binary_detects_nul() {
        assert!(is_binary(b"hello\0world"));
        assert!(!is_binary(b"hello world"));
        assert!(!is_binary(b""));
    }

    #[test]
    fn is_binary_detects_high_nontext_ratio() {
        let mut data = vec![0x01u8; 100];
        data.extend_from_slice(b"abc");
        assert!(is_binary(&data));
    }

    #[test]
    fn is_binary_accepts_normal_text() {
        let txt = "fn main() { println!(\"hello\"); }\n// this is fine\n";
        assert!(!is_binary(txt.as_bytes()));
    }

    #[test]
    fn line_char_len_counts_chars_not_bytes() {
        let mut e = editor_with("héllo");
        assert_eq!(e.line_char_len(0), 5);
        e.lines[0] = String::from("日本語");
        assert_eq!(e.line_char_len(0), 3);
    }

    #[test]
    fn byte_index_ascii() {
        let e = editor_with("abcdef");
        assert_eq!(e.byte_index(0, 0), 0);
        assert_eq!(e.byte_index(0, 3), 3);
        assert_eq!(e.byte_index(0, 6), 6);
        assert_eq!(e.byte_index(0, 99), 6); // saturates at end
    }

    #[test]
    fn byte_index_multibyte() {
        let e = editor_with("héllo");
        // 'h'=1 byte, 'é'=2 bytes, 'l'=1 byte
        assert_eq!(e.byte_index(0, 0), 0);
        assert_eq!(e.byte_index(0, 1), 1); // before 'é'
        assert_eq!(e.byte_index(0, 2), 3); // after 'é'
        assert_eq!(e.byte_index(0, 3), 4);
    }

    #[test]
    fn insert_char_at_end() {
        let mut e = editor_with("abc");
        e.cursor_col = 3;
        e.insert_char('d');
        assert_eq!(e.lines[0], "abcd");
        assert_eq!(e.cursor_col, 4);
        assert!(e.dirty);
    }

    #[test]
    fn insert_char_at_start() {
        let mut e = editor_with("bc");
        e.cursor_col = 0;
        e.insert_char('a');
        assert_eq!(e.lines[0], "abc");
        assert_eq!(e.cursor_col, 1);
    }

    #[test]
    fn insert_char_in_middle() {
        let mut e = editor_with("ac");
        e.cursor_col = 1;
        e.insert_char('b');
        assert_eq!(e.lines[0], "abc");
        assert_eq!(e.cursor_col, 2);
    }

    #[test]
    fn insert_char_multibyte_position() {
        let mut e = editor_with("aé");
        e.cursor_col = 2; // after 'é'
        e.insert_char('z');
        assert_eq!(e.lines[0], "aéz");
    }

    #[test]
    fn insert_newline_splits_line() {
        let mut e = editor_with("hello world");
        e.cursor_col = 5;
        e.insert_newline();
        assert_eq!(e.lines, vec!["hello".to_string(), " world".to_string()]);
        assert_eq!(e.cursor_row, 1);
        assert_eq!(e.cursor_col, 0);
    }

    #[test]
    fn insert_newline_at_end_creates_blank_line() {
        let mut e = editor_with("abc");
        e.cursor_col = 3;
        e.insert_newline();
        assert_eq!(e.lines, vec!["abc".to_string(), String::new()]);
        assert_eq!(e.cursor_row, 1);
    }

    #[test]
    fn backspace_mid_line() {
        let mut e = editor_with("abcd");
        e.cursor_col = 3;
        e.backspace();
        assert_eq!(e.lines[0], "abd");
        assert_eq!(e.cursor_col, 2);
    }

    #[test]
    fn backspace_at_col_zero_joins_with_previous_line() {
        let mut e = editor_with("hello\nworld");
        e.cursor_row = 1;
        e.cursor_col = 0;
        e.backspace();
        assert_eq!(e.lines, vec!["helloworld".to_string()]);
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 5);
    }

    #[test]
    fn backspace_at_origin_does_nothing_destructive() {
        let mut e = editor_with("abc");
        e.cursor_row = 0;
        e.cursor_col = 0;
        e.backspace();
        assert_eq!(e.lines, vec!["abc".to_string()]);
    }

    #[test]
    fn delete_forward_mid_line() {
        let mut e = editor_with("abcd");
        e.cursor_col = 1;
        e.delete_forward();
        assert_eq!(e.lines[0], "acd");
        assert_eq!(e.cursor_col, 1);
    }

    #[test]
    fn delete_forward_at_end_joins_with_next_line() {
        let mut e = editor_with("hello\nworld");
        e.cursor_row = 0;
        e.cursor_col = 5;
        e.delete_forward();
        assert_eq!(e.lines, vec!["helloworld".to_string()]);
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 5);
    }

    #[test]
    fn page_down_advances_one_full_viewport_and_puts_first_unseen_line_at_top() {
        // Simulate a 100-line file with the editor's viewport rendering 25
        // lines. After PageDown the cursor should land on row 25 (line 26 in
        // 1-indexed terms) and that row should be the new top of the view.
        let mut e = editor_with_lines(100);
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
        assert_eq!(e.scroll, 0);
        assert_eq!(e.cursor_row, 0);
        e.page_down_one_screen();
        assert_eq!(e.cursor_row, 25, "cursor should jump to first previously-unseen row");
        assert_eq!(e.scroll, 25, "scroll should align with new cursor at top of viewport");
    }

    #[test]
    fn page_down_repeats_advance_one_viewport_at_a_time() {
        let mut e = editor_with_lines(100);
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 20 };
        e.page_down_one_screen();
        e.page_down_one_screen();
        assert_eq!(e.cursor_row, 40);
        assert_eq!(e.scroll, 40);
    }

    #[test]
    fn page_down_clamps_at_end_of_file() {
        let mut e = editor_with_lines(30);
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
        // Realistic state: scroll = 4 means rows 4..=28 are on screen, with
        // line 29 (cursor_row 28) visible at the bottom.
        e.scroll = 4;
        e.cursor_row = 28;
        e.page_down_one_screen();
        // 4 + 25 = 29 → last row.  Cursor and scroll land there.
        assert_eq!(e.cursor_row, 29);
        assert_eq!(e.scroll, 29);
    }

    #[test]
    fn page_up_rewinds_one_full_viewport() {
        let mut e = editor_with_lines(200);
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
        e.scroll = 100;
        e.cursor_row = 100;
        e.page_up_one_screen();
        assert_eq!(e.cursor_row, 75);
        assert_eq!(e.scroll, 75);
    }

    #[test]
    fn page_up_clamps_at_top_of_file() {
        let mut e = editor_with_lines(50);
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
        e.scroll = 5;
        e.cursor_row = 5;
        e.page_up_one_screen();
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.scroll, 0);
    }

    #[test]
    fn page_size_falls_back_when_viewport_is_unknown() {
        // Before the first render last_inner is zero-sized; PageDown should
        // still advance by some sensible default rather than no-op.
        let mut e = editor_with_lines(100);
        e.last_inner = Rect::default();
        e.page_down_one_screen();
        assert!(e.cursor_row > 0, "should advance even with zero last_inner");
    }

    fn editor_with_lines(n: usize) -> Editor {
        let mut e = Editor::new();
        e.lines = (0..n).map(|i| format!("line {i}")).collect();
        e
    }

    #[test]
    fn move_left_crosses_line_boundary() {
        let mut e = editor_with("abc\ndef");
        e.cursor_row = 1;
        e.cursor_col = 0;
        e.move_left();
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 3);
    }

    #[test]
    fn move_right_crosses_line_boundary() {
        let mut e = editor_with("abc\ndef");
        e.cursor_row = 0;
        e.cursor_col = 3;
        e.move_right();
        assert_eq!(e.cursor_row, 1);
        assert_eq!(e.cursor_col, 0);
    }

    #[test]
    fn move_up_clamps_column() {
        let mut e = editor_with("ab\nlongline");
        e.cursor_row = 1;
        e.cursor_col = 7;
        e.move_up();
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 2);
    }

    #[test]
    fn home_and_end() {
        let mut e = editor_with("hello world");
        e.cursor_col = 5;
        e.home_line();
        assert_eq!(e.cursor_col, 0);
        e.end_line();
        assert_eq!(e.cursor_col, 11);
    }

    #[test]
    fn open_reads_file_and_splits_lines() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "alpha").unwrap();
        writeln!(tmp, "beta").unwrap();
        write!(tmp, "gamma").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert_eq!(e.lines, vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]);
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 0);
        assert!(!e.dirty);
    }

    #[test]
    fn open_rejects_binary_files() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"\x00\x01\x02binary garbage").unwrap();
        let mut e = Editor::new();
        let err = e.open(tmp.path()).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("binary"));
    }

    #[test]
    fn matches_open_path_handles_canonical_difference() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "x").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert!(e.matches_open_path(tmp.path()));
        let bogus = std::path::Path::new("/definitely/not/the/same/path.txt");
        assert!(!e.matches_open_path(bogus));
    }

    #[test]
    fn reload_if_clean_picks_up_external_changes() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "def hello():").unwrap();
        writeln!(tmp, "    print(\"hi\")").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert!(e.lines[0].contains("hello"));

        // Simulate an external edit (vim, git pull, etc.).
        std::fs::write(tmp.path(), "def hi():\n    print(\"hi\")\n").unwrap();
        let outcome = e.reload_if_clean();
        assert!(matches!(outcome, Some(Ok(()))));
        assert_eq!(e.lines[0], "def hi():");
        assert!(!e.dirty);
    }

    #[test]
    fn reload_if_clean_refuses_when_dirty() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "original\n").unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.insert_str("local edit");
        assert!(e.dirty);

        std::fs::write(tmp.path(), "external change\n").unwrap();
        let outcome = e.reload_if_clean();
        assert!(outcome.is_none(), "should refuse to reload over dirty buffer");
        assert!(e.lines[0].contains("local edit"));
    }

    #[test]
    fn save_round_trips_content() {
        let tmp = NamedTempFile::new().unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.insert_str("hello\nworld");
        assert!(e.dirty);
        e.save_to_disk().unwrap();
        assert!(!e.dirty);
        let written = std::fs::read_to_string(tmp.path()).unwrap();
        assert_eq!(written, "hello\nworld");
    }

    #[test]
    fn dirty_flag_lifecycle() {
        let mut e = editor_with("abc");
        assert!(!e.dirty);
        e.insert_char('z');
        assert!(e.dirty);
    }

    #[test]
    fn insert_str_inserts_newlines() {
        let mut e = editor_with("");
        e.insert_str("a\nb\nc");
        assert_eq!(e.lines, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(e.cursor_row, 2);
        assert_eq!(e.cursor_col, 1);
    }

    #[test]
    fn build_line_spans_no_highlights() {
        let spans = build_line_spans("hello", &[]);
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn build_line_spans_full_line_highlighted() {
        let hi = vec![HiSpan { start: 0, end: 5, style: Style::default() }];
        let spans = build_line_spans("hello", &hi);
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn build_line_spans_partial_highlights() {
        let hi = vec![HiSpan { start: 1, end: 3, style: Style::default() }];
        let spans = build_line_spans("abcde", &hi);
        // Expect: "a", "bc", "de"
        assert_eq!(spans.len(), 3);
    }
}

impl Widget for &mut Editor {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block_style = if self.focused {
            Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let title = match &self.path {
            Some(p) => {
                let mark = if self.dirty { "● " } else { "" };
                format!(
                    " {}{} ",
                    mark,
                    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                )
            }
            None => String::from(" EDITOR "),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style)
            .title(Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(0x1e, 0x3a, 0x6e))
                    .add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        self.last_area = area;
        self.last_inner = inner;

        let height = inner.height as usize;
        if self.cursor_row < self.scroll {
            self.scroll = self.cursor_row;
        } else if self.cursor_row >= self.scroll + height {
            self.scroll = self.cursor_row + 1 - height;
        }

        let gutter_width = (self.lines.len() + 1).to_string().len() as u16 + 1;
        self.last_gutter_width = gutter_width;
        let text_x = inner.x + gutter_width + 1;
        let text_width = inner.width.saturating_sub(gutter_width + 2);

        let end = (self.scroll + height).min(self.lines.len());
        for (row_idx, line_idx) in (self.scroll..end).enumerate() {
            let y = inner.y + row_idx as u16;
            let line_no = format!("{:>width$} ", line_idx + 1, width = gutter_width as usize - 1);
            let gutter = Line::from(Span::styled(line_no, Style::default().fg(Color::DarkGray)));
            buf.set_line(inner.x, y, &gutter, gutter_width);

            let raw = &self.lines[line_idx];
            let empty: Vec<HiSpan> = Vec::new();
            let line_spans = self.highlights.get(line_idx).unwrap_or(&empty);
            let spans = build_line_spans(raw, line_spans);
            let line = Line::from(spans);
            buf.set_line(text_x, y, &line, text_width);

            if self.focused && line_idx == self.cursor_row {
                let col = (self.cursor_col as u16).min(text_width.saturating_sub(1));
                let cx = text_x + col;
                if cx < inner.x + inner.width {
                    let cell = &mut buf[(cx, y)];
                    cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
                }
            }
        }
    }
}
