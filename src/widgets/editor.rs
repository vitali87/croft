use anyhow::Result;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};

use crate::highlight::{
    compute_line_starts, highlight_text, lang_for_extension, HiSpan, LangKind, LangRegistry,
};

const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// Inclusive char-indexed range `(row, col)` anchor and head, where head
/// follows the cursor as the user drags / shift-arrows.  `normalised()` returns
/// the pair in row-major order so callers don't have to care which end came
/// first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorSelection {
    pub anchor: (usize, usize),
    pub head: (usize, usize),
}

impl EditorSelection {
    pub fn new(row: usize, col: usize) -> Self {
        Self { anchor: (row, col), head: (row, col) }
    }
    pub fn normalised(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
    pub fn has_area(&self) -> bool {
        self.anchor != self.head
    }
}

/// Coarse classification of the most recent edit, used so consecutive
/// `InsertChar` ops coalesce into a single undo step (typing burst) but
/// any other edit kind always opens a new step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKind {
    InsertChar,
    Newline,
    Backspace,
    DeleteForward,
    Paste,
    DeleteSelection,
}

#[derive(Clone, Debug)]
struct Snapshot {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    selection: Option<EditorSelection>,
    dirty: bool,
}

const UNDO_STACK_LIMIT: usize = 500;

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
    pub selection: Option<EditorSelection>,
    undo_stack: Vec<Snapshot>,
    last_edit_kind: Option<EditKind>,
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
            selection: None,
            undo_stack: Vec::new(),
            last_edit_kind: None,
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
        self.selection = None;
        self.undo_stack.clear();
        self.last_edit_kind = None;
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
        // Selection-replace counts as one logical edit (Replace), not two.
        // Coalesce subsequent typed chars onto the same step only when the
        // previous edit was also a single-char insert with no selection.
        let had_selection = self
            .selection
            .map(|s| s.has_area())
            .unwrap_or(false);
        let kind = if had_selection {
            EditKind::DeleteSelection
        } else {
            EditKind::InsertChar
        };
        self.push_undo(kind);
        self.delete_selection_inner();
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
        self.push_undo(EditKind::Paste);
        if self.selection.is_some() {
            self.delete_selection_inner();
        }
        for c in s.chars() {
            if c == '\n' {
                self.insert_newline_raw();
            } else {
                self.insert_char_raw(c);
            }
        }
        self.recompute_highlights();
    }

    fn insert_char_raw(&mut self, c: char) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        let byte = self.byte_index(row, col);
        self.lines[row].insert(byte, c);
        self.cursor_col += 1;
        self.dirty = true;
    }

    fn insert_newline_raw(&mut self) {
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
    }

    pub fn insert_newline(&mut self) {
        self.push_undo(EditKind::Newline);
        self.delete_selection_inner();
        self.insert_newline_raw();
        self.recompute_highlights();
    }

    pub fn backspace(&mut self) {
        self.push_undo(EditKind::Backspace);
        if self.delete_selection_inner() {
            self.recompute_highlights();
            return;
        }
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
        self.push_undo(EditKind::DeleteForward);
        if self.delete_selection_inner() {
            self.recompute_highlights();
            return;
        }
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

    pub fn start_selection_at_cursor(&mut self) {
        self.selection = Some(EditorSelection::new(self.cursor_row, self.cursor_col));
    }

    pub fn extend_selection_to_cursor(&mut self) {
        if let Some(sel) = self.selection.as_mut() {
            sel.head = (self.cursor_row, self.cursor_col);
        } else {
            self.start_selection_at_cursor();
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn select_all(&mut self) {
        if self.lines.is_empty() {
            self.selection = None;
            return;
        }
        let last_row = self.lines.len() - 1;
        let last_col = self.line_char_len(last_row);
        self.selection = Some(EditorSelection {
            anchor: (0, 0),
            head: (last_row, last_col),
        });
        self.cursor_row = last_row;
        self.cursor_col = last_col;
    }

    /// Extract the selection text (`\n`-joined across rows) using the
    /// editor's char-indexed coordinates.  Returns "" when there's no
    /// selection or the selection is zero-area.
    pub fn selection_text(&self) -> String {
        let Some(sel) = self.selection else { return String::new() };
        if !sel.has_area() {
            return String::new();
        }
        let ((sr, sc), (er, ec)) = sel.normalised();
        if sr == er {
            let line = &self.lines[sr];
            let from = char_byte(line, sc);
            let to = char_byte(line, ec);
            return line[from..to].to_string();
        }
        let mut out = String::new();
        // first row: from sc to end of line
        let first = &self.lines[sr];
        let from = char_byte(first, sc);
        out.push_str(&first[from..]);
        out.push('\n');
        // full middle rows
        for r in (sr + 1)..er {
            out.push_str(&self.lines[r]);
            out.push('\n');
        }
        // last row: from start to ec
        let last = &self.lines[er];
        let to = char_byte(last, ec);
        out.push_str(&last[..to]);
        out
    }

    /// Delete the current selection if it has area.  Returns true iff content
    /// was removed.  Cursor lands at the start of the deleted range and the
    /// selection is cleared.  Pushes an undo step.
    pub fn delete_selection(&mut self) -> bool {
        if !self
            .selection
            .map(|s| s.has_area())
            .unwrap_or(false)
        {
            self.selection = None;
            return false;
        }
        self.push_undo(EditKind::DeleteSelection);
        let removed = self.delete_selection_inner();
        if removed {
            self.recompute_highlights();
        }
        removed
    }

    /// Same as `delete_selection` but does NOT push an undo step or
    /// recompute highlights — used by other public mutators that have
    /// already snapshotted state and will recompute themselves.
    fn delete_selection_inner(&mut self) -> bool {
        let Some(sel) = self.selection else { return false };
        if !sel.has_area() {
            self.selection = None;
            return false;
        }
        let ((sr, sc), (er, ec)) = sel.normalised();
        if sr == er {
            let line = &mut self.lines[sr];
            let from = char_byte(line, sc);
            let to = char_byte(line, ec);
            line.replace_range(from..to, "");
        } else {
            let last = self.lines.remove(er);
            for _ in (sr + 1)..er {
                self.lines.remove(sr + 1);
            }
            let first = &mut self.lines[sr];
            let from = char_byte(first, sc);
            first.truncate(from);
            let to = char_byte(&last, ec);
            first.push_str(&last[to..]);
        }
        self.cursor_row = sr;
        self.cursor_col = sc;
        self.selection = None;
        self.dirty = true;
        true
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
            selection: self.selection,
            dirty: self.dirty,
        }
    }

    /// Push an undo entry tagged with the kind of edit about to happen.
    /// Coalesces consecutive `InsertChar` ops into one step so a typing
    /// burst is undone as one unit; everything else opens a new step.
    fn push_undo(&mut self, kind: EditKind) {
        let coalesce = kind == EditKind::InsertChar
            && self.last_edit_kind == Some(EditKind::InsertChar);
        if !coalesce {
            self.undo_stack.push(self.snapshot());
            if self.undo_stack.len() > UNDO_STACK_LIMIT {
                self.undo_stack.remove(0);
            }
        }
        self.last_edit_kind = Some(kind);
    }

    /// Undo the most recent edit step. Returns true iff state was changed.
    pub fn undo(&mut self) -> bool {
        let Some(snap) = self.undo_stack.pop() else { return false };
        self.lines = snap.lines;
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = snap
            .cursor_row
            .min(self.lines.len().saturating_sub(1));
        self.cursor_col = snap
            .cursor_col
            .min(self.line_char_len(self.cursor_row));
        self.selection = snap.selection;
        self.dirty = snap.dirty;
        self.last_edit_kind = None;
        self.recompute_highlights();
        true
    }

    /// Open a new undo step for the next edit (so a typing run doesn't
    /// merge with whatever comes after a movement / mouse / focus change).
    pub fn break_undo_coalescing(&mut self) {
        self.last_edit_kind = None;
    }

    /// Mouse-down: position the cursor at the click point and start a
    /// fresh zero-area selection there. A subsequent drag widens it.
    pub fn mouse_down(&mut self, col: u16, row: u16) {
        self.click(col, row);
        self.start_selection_at_cursor();
    }

    /// Mouse-drag: move the cursor to the drag point and extend the selection
    /// head to the new cursor.  Anchors at the current cursor if no prior
    /// selection exists.
    pub fn mouse_drag(&mut self, col: u16, row: u16) {
        if self.selection.is_none() {
            self.start_selection_at_cursor();
        }
        self.click(col, row);
        self.extend_selection_to_cursor();
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
        self.last_edit_kind = None;
    }

    pub fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        }
        self.last_edit_kind = None;
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.line_char_len(self.cursor_row);
        }
        self.last_edit_kind = None;
    }

    pub fn move_right(&mut self) {
        if self.cursor_col < self.line_char_len(self.cursor_row) {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
        self.last_edit_kind = None;
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
        self.last_edit_kind = None;
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
        self.last_edit_kind = None;
    }

    pub fn home_line(&mut self) {
        self.cursor_col = 0;
        self.last_edit_kind = None;
    }

    pub fn end_line(&mut self) {
        self.cursor_col = self.line_char_len(self.cursor_row);
        self.last_edit_kind = None;
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.cursor_row = self.cursor_row.saturating_sub(n);
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.cursor_row = (self.cursor_row + n).min(self.lines.len().saturating_sub(1));
        self.cursor_col = self.cursor_col.min(self.line_char_len(self.cursor_row));
        self.last_edit_kind = None;
    }

    /// Double-click word selection: place the cursor at the click point, then
    /// select the maximal run of word characters covering it and leave the
    /// caret at the right edge of that run (VS Code parity). A click on
    /// whitespace or past the end of an empty line clears any selection.
    pub fn select_word_at(&mut self, col: u16, row: u16) {
        self.click(col, row);
        if self.lines.is_empty() {
            self.selection = None;
            return;
        }
        let r = self.cursor_row;
        let chars: Vec<char> = self.lines[r].chars().collect();
        let c = self.cursor_col;
        let pivot = if c < chars.len() && is_word_char(chars[c]) {
            Some(c)
        } else if c == chars.len() && c > 0 && is_word_char(chars[c - 1]) {
            Some(c - 1)
        } else {
            None
        };
        let Some(p) = pivot else {
            self.selection = None;
            return;
        };
        let mut start = p;
        while start > 0 && is_word_char(chars[start - 1]) {
            start -= 1;
        }
        let mut end = p + 1;
        while end < chars.len() && is_word_char(chars[end]) {
            end += 1;
        }
        self.cursor_col = end;
        self.selection = Some(EditorSelection {
            anchor: (r, start),
            head: (r, end),
        });
        self.last_edit_kind = None;
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
        self.last_edit_kind = None;
    }
}

/// Convert a char index within `s` to a byte index, saturating at `s.len()`.
fn char_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(i, _)| i).unwrap_or(s.len())
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
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

    #[test]
    fn editor_selection_normalised_handles_anchor_after_head() {
        let s = EditorSelection { anchor: (5, 4), head: (2, 1) };
        assert_eq!(s.normalised(), ((2, 1), (5, 4)));
    }

    #[test]
    fn editor_selection_normalised_handles_same_row() {
        let s = EditorSelection { anchor: (3, 9), head: (3, 2) };
        assert_eq!(s.normalised(), ((3, 2), (3, 9)));
    }

    #[test]
    fn editor_selection_has_area_only_when_endpoints_differ() {
        let s = EditorSelection::new(2, 5);
        assert!(!s.has_area());
        let s2 = EditorSelection { anchor: (2, 5), head: (2, 6) };
        assert!(s2.has_area());
    }

    #[test]
    fn start_selection_at_cursor_creates_zero_area_selection() {
        let mut e = editor_with("hello");
        e.cursor_row = 0;
        e.cursor_col = 2;
        e.start_selection_at_cursor();
        let sel = e.selection.expect("selection should exist");
        assert_eq!(sel.anchor, (0, 2));
        assert_eq!(sel.head, (0, 2));
        assert!(!sel.has_area());
    }

    #[test]
    fn extend_selection_to_cursor_updates_head_only() {
        let mut e = editor_with("abcdef");
        e.cursor_col = 1;
        e.start_selection_at_cursor();
        e.cursor_col = 4;
        e.extend_selection_to_cursor();
        let sel = e.selection.unwrap();
        assert_eq!(sel.anchor, (0, 1));
        assert_eq!(sel.head, (0, 4));
        assert!(sel.has_area());
    }

    #[test]
    fn selection_text_single_line() {
        let mut e = editor_with("hello world");
        e.cursor_col = 6;
        e.start_selection_at_cursor();
        e.cursor_col = 11;
        e.extend_selection_to_cursor();
        assert_eq!(e.selection_text(), "world");
    }

    #[test]
    fn selection_text_handles_reversed_endpoints() {
        let mut e = editor_with("hello world");
        e.cursor_col = 11;
        e.start_selection_at_cursor();
        e.cursor_col = 6;
        e.extend_selection_to_cursor();
        assert_eq!(e.selection_text(), "world");
    }

    #[test]
    fn selection_text_multi_line_includes_newlines() {
        let mut e = editor_with("first line\nsecond line\nthird");
        e.cursor_row = 0;
        e.cursor_col = 6;
        e.start_selection_at_cursor();
        e.cursor_row = 1;
        e.cursor_col = 6;
        e.extend_selection_to_cursor();
        assert_eq!(e.selection_text(), "line\nsecond");
    }

    #[test]
    fn selection_text_multibyte_chars() {
        let mut e = editor_with("héllo");
        e.cursor_col = 1;
        e.start_selection_at_cursor();
        e.cursor_col = 3;
        e.extend_selection_to_cursor();
        assert_eq!(e.selection_text(), "él");
    }

    #[test]
    fn clear_selection_removes_it() {
        let mut e = editor_with("abc");
        e.start_selection_at_cursor();
        assert!(e.selection.is_some());
        e.clear_selection();
        assert!(e.selection.is_none());
    }

    #[test]
    fn delete_selection_removes_range_within_one_line() {
        let mut e = editor_with("hello world");
        e.cursor_col = 5;
        e.start_selection_at_cursor();
        e.cursor_col = 11;
        e.extend_selection_to_cursor();
        assert!(e.delete_selection());
        assert_eq!(e.lines, vec!["hello".to_string()]);
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 5);
        assert!(e.selection.is_none());
        assert!(e.dirty);
    }

    #[test]
    fn delete_selection_collapses_multiple_lines() {
        let mut e = editor_with("first\nsecond\nthird");
        e.cursor_row = 0;
        e.cursor_col = 2;
        e.start_selection_at_cursor();
        e.cursor_row = 2;
        e.cursor_col = 2;
        e.extend_selection_to_cursor();
        assert!(e.delete_selection());
        assert_eq!(e.lines, vec!["fiird".to_string()]);
        assert_eq!(e.cursor_row, 0);
        assert_eq!(e.cursor_col, 2);
    }

    #[test]
    fn delete_selection_returns_false_when_no_selection() {
        let mut e = editor_with("abc");
        assert!(!e.delete_selection());
        assert_eq!(e.lines, vec!["abc".to_string()]);
    }

    #[test]
    fn delete_selection_returns_false_when_zero_area() {
        let mut e = editor_with("abc");
        e.cursor_col = 1;
        e.start_selection_at_cursor();
        assert!(!e.delete_selection());
        assert_eq!(e.lines, vec!["abc".to_string()]);
    }

    #[test]
    fn insert_char_replaces_selection_when_active() {
        let mut e = editor_with("hello world");
        e.cursor_col = 6;
        e.start_selection_at_cursor();
        e.cursor_col = 11;
        e.extend_selection_to_cursor();
        e.insert_char('X');
        assert_eq!(e.lines, vec!["hello X".to_string()]);
        assert_eq!(e.cursor_col, 7);
        assert!(e.selection.is_none());
    }

    #[test]
    fn backspace_deletes_selection_when_active() {
        let mut e = editor_with("hello world");
        e.cursor_col = 6;
        e.start_selection_at_cursor();
        e.cursor_col = 11;
        e.extend_selection_to_cursor();
        e.backspace();
        assert_eq!(e.lines, vec!["hello ".to_string()]);
        assert!(e.selection.is_none());
    }

    #[test]
    fn delete_forward_deletes_selection_when_active() {
        let mut e = editor_with("hello world");
        e.cursor_col = 6;
        e.start_selection_at_cursor();
        e.cursor_col = 11;
        e.extend_selection_to_cursor();
        e.delete_forward();
        assert_eq!(e.lines, vec!["hello ".to_string()]);
        assert!(e.selection.is_none());
    }

    #[test]
    fn insert_str_replaces_selection_when_active() {
        let mut e = editor_with("hello world");
        e.cursor_col = 6;
        e.start_selection_at_cursor();
        e.cursor_col = 11;
        e.extend_selection_to_cursor();
        e.insert_str("everyone");
        assert_eq!(e.lines, vec!["hello everyone".to_string()]);
        assert!(e.selection.is_none());
    }

    #[test]
    fn select_all_spans_entire_buffer() {
        let mut e = editor_with("a\nbc\nd");
        e.select_all();
        let sel = e.selection.unwrap();
        let (start, end) = sel.normalised();
        assert_eq!(start, (0, 0));
        assert_eq!(end, (2, 1));
        assert_eq!(e.selection_text(), "a\nbc\nd");
    }

    #[test]
    fn mouse_down_starts_zero_area_selection_at_click() {
        let mut e = editor_with("hello");
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
        e.last_gutter_width = 2;
        e.mouse_down(3 + 0, 0); // text_x = 0 + 2 + 1 = 3, click col 3 → editor col 0
        assert_eq!(e.cursor_col, 0);
        let sel = e.selection.expect("anchor created on mouse down");
        assert_eq!(sel.anchor, (0, 0));
        assert!(!sel.has_area());
    }

    #[test]
    fn render_never_replaces_character_at_cursor() {
        // The hardware caret (DECSCUSR BlinkingBar) overlays the cell at the
        // cursor position; the editor's own render must NEVER change the
        // symbol there or the blink would visibly swallow the letter.
        use ratatui::buffer::Buffer;
        let mut e = editor_with("hello");
        e.cursor_col = 2;
        e.focused = true;

        let area = Rect { x: 0, y: 0, width: 30, height: 5 };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);

        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let cell = &buf[(text_x + 2, e.last_inner.y)];
        assert_eq!(cell.symbol(), "l", "editor render must leave the underlying glyph alone");
    }

    #[test]
    fn cursor_screen_pos_inside_viewport() {
        let mut e = editor_with("hello\nworld");
        e.last_inner = Rect { x: 5, y: 7, width: 80, height: 25 };
        e.last_gutter_width = 2;
        e.cursor_row = 1;
        e.cursor_col = 3;
        // text_x = inner.x + gutter + 1 = 5 + 2 + 1 = 8
        // cy = inner.y + (cursor_row - scroll) = 7 + 1 = 8
        assert_eq!(e.cursor_screen_pos(), Some((8 + 3, 8)));
    }

    #[test]
    fn cursor_screen_pos_returns_none_when_scrolled_off() {
        let mut e = editor_with_lines(50);
        e.last_inner = Rect { x: 0, y: 0, width: 40, height: 10 };
        e.last_gutter_width = 3;
        e.scroll = 30;
        e.cursor_row = 5; // above viewport
        assert_eq!(e.cursor_screen_pos(), None);
    }

    #[test]
    fn undo_restores_previous_buffer_and_cursor() {
        let mut e = editor_with("abc");
        e.cursor_col = 3;
        e.insert_char('d');
        assert_eq!(e.lines[0], "abcd");
        assert!(e.undo());
        assert_eq!(e.lines[0], "abc");
        assert_eq!(e.cursor_col, 3);
    }

    #[test]
    fn undo_on_empty_stack_returns_false() {
        let mut e = editor_with("abc");
        assert!(!e.undo());
        assert_eq!(e.lines[0], "abc");
    }

    #[test]
    fn undo_coalesces_consecutive_typed_chars_into_one_step() {
        let mut e = editor_with("");
        e.insert_char('h');
        e.insert_char('i');
        e.insert_char('!');
        // One undo undoes the whole typed run "hi!".
        assert!(e.undo());
        assert_eq!(e.lines[0], "");
    }

    #[test]
    fn undo_does_not_coalesce_across_movement() {
        let mut e = editor_with("abc");
        e.cursor_col = 3;
        e.insert_char('d');
        e.move_left();
        e.insert_char('z');
        // First undo removes 'z', second undo removes 'd'.
        assert!(e.undo());
        assert_eq!(e.lines[0], "abcd");
        assert!(e.undo());
        assert_eq!(e.lines[0], "abc");
    }

    #[test]
    fn undo_does_not_coalesce_across_backspace() {
        let mut e = editor_with("abc");
        e.cursor_col = 3;
        e.insert_char('d');
        e.backspace();
        e.insert_char('e');
        assert!(e.undo());
        assert_eq!(e.lines[0], "abc");
        assert!(e.undo());
        assert_eq!(e.lines[0], "abcd");
        assert!(e.undo());
        assert_eq!(e.lines[0], "abc");
    }

    #[test]
    fn undo_paste_is_one_step() {
        let mut e = editor_with("abc");
        e.cursor_col = 3;
        e.insert_str("XYZ");
        assert_eq!(e.lines[0], "abcXYZ");
        assert!(e.undo());
        assert_eq!(e.lines[0], "abc");
    }

    #[test]
    fn undo_after_replace_selection_restores_original() {
        let mut e = editor_with("hello world");
        e.cursor_col = 6;
        e.start_selection_at_cursor();
        e.cursor_col = 11;
        e.extend_selection_to_cursor();
        e.insert_char('X');
        assert_eq!(e.lines[0], "hello X");
        assert!(e.undo());
        assert_eq!(e.lines[0], "hello world");
    }

    #[test]
    fn undo_restores_dirty_flag() {
        let mut e = editor_with("abc");
        assert!(!e.dirty);
        e.cursor_col = 3;
        e.insert_char('d');
        assert!(e.dirty);
        e.undo();
        assert!(!e.dirty, "undoing the only edit restores the clean state");
    }

    #[test]
    fn render_paints_selection_band_on_selected_cells() {
        use ratatui::buffer::Buffer;
        let mut e = editor_with("hello world");
        e.cursor_col = 0;
        e.start_selection_at_cursor();
        e.cursor_col = 5;
        e.extend_selection_to_cursor();
        e.focused = true;

        let area = Rect { x: 0, y: 0, width: 30, height: 5 };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);

        // gutter for 1 line: "1 ".len() = 2 → text_x = 0+1+2+1 = 4
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let selected_bg = Color::Rgb(0x26, 0x4f, 0x78);
        // chars 0..5 should be highlighted
        for col in 0..5u16 {
            let bg = buf[(text_x + col, e.last_inner.y)].bg;
            assert_eq!(
                bg, selected_bg,
                "cell at col {col} should have selection bg, got {bg:?}"
            );
        }
        // char 5 (the space) is OUTSIDE the selection (head exclusive end)
        let bg5 = buf[(text_x + 5, e.last_inner.y)].bg;
        assert_ne!(bg5, selected_bg, "cell at col 5 should NOT be highlighted");
    }

    #[test]
    fn render_paints_selection_band_across_multiple_lines() {
        use ratatui::buffer::Buffer;
        let mut e = editor_with("first\nsecond\nthird");
        e.cursor_row = 0;
        e.cursor_col = 2;
        e.start_selection_at_cursor();
        e.cursor_row = 2;
        e.cursor_col = 2;
        e.extend_selection_to_cursor();
        e.focused = true;

        let area = Rect { x: 0, y: 0, width: 30, height: 6 };
        let mut buf = Buffer::empty(area);
        (&mut e).render(area, &mut buf);

        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let selected_bg = Color::Rgb(0x26, 0x4f, 0x78);

        // Row 0: cols 2..end (all the way past the end of "first")
        let row0_y = e.last_inner.y;
        assert_eq!(buf[(text_x + 2, row0_y)].bg, selected_bg, "row 0 col 2");
        assert_eq!(buf[(text_x + 4, row0_y)].bg, selected_bg, "row 0 col 4");
        assert_ne!(buf[(text_x, row0_y)].bg, selected_bg, "row 0 col 0 not selected");

        // Row 1 (full line "second"): all cells in selection
        let row1_y = e.last_inner.y + 1;
        assert_eq!(buf[(text_x, row1_y)].bg, selected_bg, "row 1 col 0");
        assert_eq!(buf[(text_x + 5, row1_y)].bg, selected_bg, "row 1 col 5");

        // Row 2 (last line "third"): cols 0..2 in selection
        let row2_y = e.last_inner.y + 2;
        assert_eq!(buf[(text_x, row2_y)].bg, selected_bg, "row 2 col 0");
        assert_eq!(buf[(text_x + 1, row2_y)].bg, selected_bg, "row 2 col 1");
        assert_ne!(buf[(text_x + 2, row2_y)].bg, selected_bg, "row 2 col 2 not selected");
    }

    #[test]
    fn double_click_selects_word_and_moves_cursor_to_end() {
        let mut e = editor_with("hello world");
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
        e.last_gutter_width = 2;
        let text_x: u16 = 3;
        e.select_word_at(text_x + 7, 0);
        let sel = e.selection.expect("word selection created");
        assert_eq!(sel.normalised(), ((0, 6), (0, 11)));
        assert_eq!(e.cursor_col, 11);
    }

    #[test]
    fn double_click_on_first_word_selects_it_from_column_zero() {
        let mut e = editor_with("foo bar");
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
        e.last_gutter_width = 2;
        let text_x: u16 = 3;
        e.select_word_at(text_x + 1, 0);
        let sel = e.selection.unwrap();
        assert_eq!(sel.normalised(), ((0, 0), (0, 3)));
        assert_eq!(e.cursor_col, 3);
    }

    #[test]
    fn double_click_on_whitespace_does_not_create_a_selection() {
        let mut e = editor_with("hello world");
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
        e.last_gutter_width = 2;
        let text_x: u16 = 3;
        e.select_word_at(text_x + 5, 0);
        assert!(
            e.selection.map(|s| !s.has_area()).unwrap_or(true),
            "whitespace double-click must not start a non-empty selection"
        );
    }

    #[test]
    fn double_click_past_end_of_line_extends_last_word() {
        let mut e = editor_with("foo");
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
        e.last_gutter_width = 2;
        let text_x: u16 = 3;
        e.select_word_at(text_x + 12, 0);
        let sel = e.selection.unwrap();
        assert_eq!(sel.normalised(), ((0, 0), (0, 3)));
        assert_eq!(e.cursor_col, 3);
    }

    #[test]
    fn editor_tabs_starts_with_one_empty_editor() {
        let t = EditorTabs::new();
        assert_eq!(t.tab_count(), 1);
        assert_eq!(t.active_index(), 0);
        assert!(t.path.is_none());
    }

    #[test]
    fn editor_tabs_open_in_new_tab_appends_and_activates() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/tmp/a.rs"));
        t.add_tab_with_path(std::path::PathBuf::from("/tmp/b.rs"));
        assert_eq!(t.tab_count(), 2);
        assert_eq!(t.active_index(), 1);
        assert_eq!(t.path.as_deref(), Some(std::path::Path::new("/tmp/b.rs")));
    }

    #[test]
    fn editor_tabs_new_tab_lands_immediately_after_active() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c"));
        // active is now 2 (c). Switch back to a (idx 0), open d → should be at idx 1.
        t.select(0);
        t.add_tab_with_path(std::path::PathBuf::from("/d"));
        let labels: Vec<_> = t
            .iter_tabs()
            .map(|e| e.path.as_ref().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(labels, vec!["/a", "/d", "/b", "/c"]);
        assert_eq!(t.active_index(), 1);
    }

    #[test]
    fn editor_tabs_close_active_drops_tab_and_reselects() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c"));
        t.select(1); // active = b
        assert!(t.close_active());
        assert_eq!(t.tab_count(), 2);
        // After closing the middle tab, the next tab takes its slot, which is c.
        assert_eq!(t.path.as_deref(), Some(std::path::Path::new("/c")));
    }

    #[test]
    fn editor_tabs_close_last_tab_keeps_one_empty_editor() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        assert!(!t.close_active(), "must not drop the only tab");
        assert_eq!(t.tab_count(), 1);
    }

    #[test]
    fn editor_tabs_tab_at_x_returns_index_of_clicked_tab() {
        use ratatui::buffer::Buffer;
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/long_name.rs"));
        t.add_tab_with_path(std::path::PathBuf::from("/b.rs"));
        let area = Rect { x: 0, y: 0, width: 60, height: 10 };
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let first = t.tab_screen_x(0).expect("tab 0 laid out");
        let second = t.tab_screen_x(1).expect("tab 1 laid out");
        assert_eq!(t.tab_at(first.0, area.y), Some(0));
        assert_eq!(t.tab_at(second.0, area.y), Some(1));
        assert_eq!(t.tab_at(area.x + area.width - 1, area.y + 5), None);
    }

    #[test]
    fn editor_tabs_find_tab_with_path_returns_index_when_open() {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c"));
        assert_eq!(t.find_tab_with_path(std::path::Path::new("/a")), Some(0));
        assert_eq!(t.find_tab_with_path(std::path::Path::new("/b")), Some(1));
        assert_eq!(t.find_tab_with_path(std::path::Path::new("/c")), Some(2));
        assert_eq!(t.find_tab_with_path(std::path::Path::new("/missing")), None);
    }

    #[test]
    fn editor_tabs_deref_exposes_active_editor_state() {
        let mut t = EditorTabs::new();
        t.lines = vec!["abc".to_string()];
        t.cursor_col = 3;
        // Field access reaches active editor via DerefMut.
        assert_eq!(t.lines, vec!["abc".to_string()]);
        assert_eq!(t.cursor_col, 3);
    }

    #[test]
    fn mouse_drag_extends_selection() {
        let mut e = editor_with("hello world");
        e.last_inner = Rect { x: 0, y: 0, width: 80, height: 25 };
        e.last_gutter_width = 2;
        let text_x: u16 = 3;
        e.mouse_down(text_x + 0, 0);
        e.mouse_drag(text_x + 5, 0);
        let sel = e.selection.unwrap();
        assert_eq!(sel.anchor, (0, 0));
        assert_eq!(sel.head, (0, 5));
        assert_eq!(e.cursor_col, 5);
    }
}

impl Widget for &mut Editor {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block_style = if self.focused {
            Style::default().fg(Color::Rgb(0x4e, 0x9a, 0xff))
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(block_style);
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

        let sel_norm = self
            .selection
            .filter(|s| s.has_area())
            .map(|s| s.normalised());

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

            if let Some(((sr, sc), (er, ec))) = sel_norm {
                if line_idx >= sr && line_idx <= er {
                    let line_chars = self.line_char_len(line_idx);
                    let row_start = if line_idx == sr { sc } else { 0 };
                    // For non-final selected rows, paint past the line content
                    // by one cell to make the trailing newline visible.
                    let row_end = if line_idx == er {
                        ec
                    } else {
                        line_chars + 1
                    };
                    paint_selection_band(
                        buf,
                        text_x,
                        y,
                        text_width,
                        row_start,
                        row_end,
                    );
                }
            }

            // The cursor itself is drawn by the host terminal as a hardware
            // caret (DECSCUSR `BlinkingBar`); App calls
            // `frame.set_cursor_position(...)` so the blink/overlay never
            // hides the underlying character.
        }
    }
}

impl Editor {
    /// Absolute (column, row) of the editor's cursor in screen coordinates,
    /// or `None` if the cursor is outside the visible viewport. Used by
    /// `App::render` to position the host terminal's hardware caret.
    pub fn cursor_screen_pos(&self) -> Option<(u16, u16)> {
        if self.last_inner.height == 0 {
            return None;
        }
        if self.cursor_row < self.scroll {
            return None;
        }
        let row_in_view = self.cursor_row - self.scroll;
        if (row_in_view as u16) >= self.last_inner.height {
            return None;
        }
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        let text_width = self
            .last_inner
            .width
            .saturating_sub(self.last_gutter_width + 2);
        if text_width == 0 {
            return None;
        }
        let col = (self.cursor_col as u16).min(text_width.saturating_sub(1));
        let cx = text_x + col;
        let cy = self.last_inner.y + row_in_view as u16;
        Some((cx, cy))
    }
}

/// Apply the selection background colour to columns `[start_char..end_char)`
/// of row `y`, where columns are character indices within the editor's text
/// area.  Clamps to the visible width.
fn paint_selection_band(
    buf: &mut Buffer,
    text_x: u16,
    y: u16,
    text_width: u16,
    start_char: usize,
    end_char: usize,
) {
    let bg = Color::Rgb(0x26, 0x4f, 0x78);
    let s = start_char.min(text_width as usize);
    let e = end_char.min(text_width as usize);
    if e <= s {
        return;
    }
    for col in s..e {
        let x = text_x + col as u16;
        let cell = &mut buf[(x, y)];
        cell.set_style(cell.style().bg(bg));
    }
}

/// Multi-buffer editor: a stack of `Editor` instances with a single active
/// one, plus a 1-row clickable tab strip rendered above the active editor.
/// `Deref`/`DerefMut` aim at the active editor so existing call sites that
/// were written for a single `Editor` continue to work without rewrites.
pub struct EditorTabs {
    pub editors: Vec<Editor>,
    active: usize,
    /// Per-tab on-screen `(x_start, width)` recorded by the most recent
    /// render. `tab_at(col, row)` reads this to map mouse clicks to tab
    /// indices.
    tab_screen_ranges: Vec<(u16, u16)>,
    tab_strip_y: u16,
    /// The full pane area (tab strip + body) from the most recent render.
    /// Used by `App::handle_mouse` for hit-testing — the active editor's
    /// own `last_area` only covers the body below the strip.
    pub last_full_area: Rect,
}

impl EditorTabs {
    pub fn new() -> Self {
        Self {
            editors: vec![Editor::new()],
            active: 0,
            tab_screen_ranges: Vec::new(),
            tab_strip_y: 0,
            last_full_area: Rect::default(),
        }
    }

    pub fn tab_count(&self) -> usize {
        self.editors.len()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn iter_tabs(&self) -> impl Iterator<Item = &Editor> {
        self.editors.iter()
    }

    pub fn select(&mut self, idx: usize) -> bool {
        if idx >= self.editors.len() {
            return false;
        }
        self.editors[self.active].focused = false;
        self.active = idx;
        self.editors[self.active].focused = true;
        true
    }

    /// Open `path` in a brand-new tab inserted directly after the active
    /// one, then make that new tab active. Returns the result of the
    /// underlying `Editor::open` so the caller can surface errors.
    pub fn open_in_new_tab(&mut self, path: &Path) -> Result<()> {
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.open(path)?;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Test-only / disk-less helper: insert a tab whose path is set but
    /// whose contents are empty. Production code should call
    /// `open_in_new_tab` so the file is actually loaded from disk.
    pub fn add_tab_with_path(&mut self, path: PathBuf) {
        let mut e = Editor::new();
        e.path = Some(path);
        e.focused = self.editors[self.active].focused;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
    }

    /// Close the currently active tab. Refuses (returns false) when only one
    /// tab remains — closing the last would leave the editor pane empty.
    pub fn close_active(&mut self) -> bool {
        if self.editors.len() <= 1 {
            return false;
        }
        self.editors.remove(self.active);
        if self.active >= self.editors.len() {
            self.active = self.editors.len() - 1;
        }
        self.editors[self.active].focused = true;
        true
    }

    /// Map a mouse cell `(col, row)` to a tab index, or `None` if the click
    /// missed every tab. Uses the on-screen ranges captured during the most
    /// recent render.
    pub fn tab_at(&self, col: u16, row: u16) -> Option<usize> {
        if row != self.tab_strip_y {
            return None;
        }
        for (i, &(x, w)) in self.tab_screen_ranges.iter().enumerate() {
            if col >= x && col < x.saturating_add(w) {
                return Some(i);
            }
        }
        None
    }

    pub fn tab_screen_x(&self, idx: usize) -> Option<(u16, u16)> {
        self.tab_screen_ranges.get(idx).copied()
    }

    /// Index of the first tab whose `path` matches `target` either by
    /// literal equality or by canonicalised equality (so symlink + relative
    /// path aliases dedupe to the same tab). Returns `None` if no tab is
    /// currently holding that file.
    pub fn find_tab_with_path(&self, target: &Path) -> Option<usize> {
        let canon_target = target.canonicalize().ok();
        self.editors.iter().position(|e| {
            let Some(p) = e.path.as_ref() else { return false };
            if p == target {
                return true;
            }
            match (canon_target.as_ref(), p.canonicalize().ok()) {
                (Some(a), Some(b)) => *a == b,
                _ => false,
            }
        })
    }

    /// Either switch to the tab already holding `path`, or open `path` in a
    /// brand-new tab next to the active one. Used by Ctrl+Enter so the user
    /// never gets two tabs pointing at the same file.
    pub fn open_in_new_tab_or_switch(&mut self, path: &Path) -> Result<()> {
        if let Some(idx) = self.find_tab_with_path(path) {
            self.select(idx);
            return Ok(());
        }
        self.open_in_new_tab(path)
    }

    /// Either switch to an existing tab holding `path`, or replace the
    /// active tab's contents with `path`. Used by plain Enter / mouse click /
    /// search-result open so opening a file already on screen never creates
    /// a second tab for it.
    pub fn open_or_switch(&mut self, path: &Path) -> Result<()> {
        if let Some(idx) = self.find_tab_with_path(path) {
            self.select(idx);
            return Ok(());
        }
        self.open(path)
    }
}

impl Default for EditorTabs {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for EditorTabs {
    type Target = Editor;
    fn deref(&self) -> &Editor {
        &self.editors[self.active]
    }
}

impl DerefMut for EditorTabs {
    fn deref_mut(&mut self) -> &mut Editor {
        &mut self.editors[self.active]
    }
}

const TAB_STRIP_BG: Color = Color::Rgb(0x1f, 0x24, 0x36);
const TAB_INACTIVE_BG: Color = Color::Rgb(0x2a, 0x2f, 0x3e);
const TAB_ACTIVE_BG: Color = Color::Rgb(0x1e, 0x3a, 0x6e);
const TAB_INACTIVE_FG: Color = Color::Rgb(0x9d, 0xa5, 0xb4);
const TAB_ACTIVE_FG: Color = Color::White;

impl Widget for &mut EditorTabs {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.last_full_area = area;
        if area.height == 0 || area.width == 0 {
            return;
        }
        let strip_h: u16 = 1;
        let strip = Rect { x: area.x, y: area.y, width: area.width, height: strip_h };
        let body = Rect {
            x: area.x,
            y: area.y + strip_h,
            width: area.width,
            height: area.height - strip_h,
        };

        // Paint strip background first so the gap to the right of the last
        // tab still reads as the tab-strip colour rather than terminal default.
        let strip_bg_style = Style::default().bg(TAB_STRIP_BG);
        for x in strip.x..strip.x + strip.width {
            buf[(x, strip.y)].set_style(strip_bg_style);
            buf[(x, strip.y)].set_symbol(" ");
        }

        self.tab_strip_y = strip.y;
        self.tab_screen_ranges.clear();
        let mut cursor_x = strip.x;
        let active = self.active;
        for (i, ed) in self.editors.iter().enumerate() {
            let label_text = tab_label(ed);
            let label_chars = label_text.chars().count() as u16;
            let pad: u16 = 1;
            let width = label_chars.saturating_add(pad * 2);
            if cursor_x.saturating_add(width) > strip.x + strip.width {
                self.tab_screen_ranges.push((cursor_x, 0));
                continue;
            }
            let is_active = i == active;
            let bg = if is_active { TAB_ACTIVE_BG } else { TAB_INACTIVE_BG };
            let fg = if is_active { TAB_ACTIVE_FG } else { TAB_INACTIVE_FG };
            let style = Style::default().fg(fg).bg(bg).add_modifier(
                if is_active { Modifier::BOLD } else { Modifier::empty() },
            );
            let padded = format!(" {label_text} ");
            buf.set_string(cursor_x, strip.y, &padded, style);
            self.tab_screen_ranges.push((cursor_x, width));
            cursor_x = cursor_x.saturating_add(width);
        }

        let active_editor = &mut self.editors[active];
        Widget::render(active_editor, body, buf);
    }
}

fn tab_label(e: &Editor) -> String {
    let name = match &e.path {
        Some(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| String::from("untitled")),
        None => String::from("untitled"),
    };
    if e.dirty {
        format!("\u{25cf} {name}")
    } else {
        name
    }
}
