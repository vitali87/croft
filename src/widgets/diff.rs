use std::path::{Path, PathBuf};

use crate::widgets::editor_find::line_matches;
use crate::widgets::search::SearchOpts;

/// Max selected-text length that still drives occurrence highlighting,
/// matching VS Code's default `editor.selectionHighlightMaxLength`. Kept in
/// sync with the plain editor's constant of the same purpose.
const SELECTION_HIGHLIGHT_MAX_LEN: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffData {
    pub left_path: PathBuf,
    pub right_path: PathBuf,
    /// True only when `left_path` is a real file on disk (a two-file
    /// `open_diff`), so Enter on a `Removed` row can open it at that line.
    /// False for synthetic-left diffs (the HEAD-vs-working view, where the
    /// left side is a `"foo (HEAD)"` label) so a `Removed` row falls back to
    /// the real working file on the right instead of a path that ENOENTs.
    pub left_is_real_file: bool,
    pub left_lines: Vec<String>,
    pub right_lines: Vec<String>,
    pub rows: Vec<DiffRow>,
    /// Top-most row index visible in the viewport. Mirrors `Editor.scroll_y`
    /// for non-diff tabs.
    pub scroll: usize,
    /// Number of leading characters to skip on each rendered row so the
    /// user can pan horizontally past long lines. Same value applies to
    /// both columns so the side-by-side rows stay aligned.
    pub scroll_x: usize,
    /// True when the raw byte contents differ but `build_diff_rows`
    /// produced zero non-Equal rows — meaning the difference is invisible
    /// at the line level (trailing newline, CRLF↔LF, BOM, or whitespace
    /// the `.lines()` splitter normalises). Surfaces in the diff header
    /// so the user understands why no red/green bands are painted even
    /// though git reports the file as modified.
    pub bytes_differ_but_lines_equal: bool,
    /// True when the diff should render as a single-column unified view
    /// instead of the two-column side-by-side. Set by
    /// `build_unified_deletion` so a tombstone for a file the source-
    /// control panel reports as deleted can show every removed line with
    /// a `-` sign and a red band — visually identical to `git diff` for a
    /// removed file.
    pub unified: bool,
    /// Active drag-select on one of the two columns. None when no
    /// selection is in flight; anchor==head is a zero-area selection
    /// (just a click). Side stays fixed for the lifetime of the
    /// selection so dragging across the seam doesn't flip sides
    /// mid-stream.
    pub selection: Option<DiffSelection>,
    /// Inline-find state for the diff view. Holds the active needle, its
    /// search options, and the currently-selected match so the renderer
    /// can light up every occurrence and paint the active one in a
    /// stronger colour, mirroring the plain-text editor's Cmd+F highlight.
    pub find: DiffFindState,
    /// Row of the hunk the last `‹`/`›` (or Shift+F7 / F7) jump landed on.
    /// Hunk-to-hunk navigation anchors on THIS, not on `scroll`, because
    /// `render_diff` clamps `scroll` to `max_scroll` and writes it back:
    /// once the last hunk sits in the bottom-clamped region, a
    /// scroll-derived anchor lands below it and the forward arrow re-selects
    /// the same hunk forever instead of wrapping. Cleared on any manual
    /// vertical scroll so the next jump re-anchors on what the user is
    /// actually looking at.
    pub nav_anchor: Option<usize>,
    /// True only for the Source Control HEAD-vs-working-tree view, the one
    /// diff whose left side is the git blob hunk patches are verified
    /// against. Gates stage/unstage/revert: a Timeline snapshot diff is
    /// built by the same opener and must not reach the git index.
    pub left_is_git_head: bool,
    /// Per-line CRLF flags and final-newline facts for byte-exact hunk
    /// patches: the display lines are `str::lines()`-cleaned, which erases
    /// both. Empty/false when raw text was not supplied (hunk actions are
    /// gated to the builder that supplies it).
    pub left_crlf: Vec<bool>,
    pub right_crlf: Vec<bool>,
    pub left_no_final_nl: bool,
    pub right_no_final_nl: bool,
}

/// A single occurrence of the find needle inside a diff, located by the
/// visual row index (into `rows`), the column it lives in, and the
/// character span it covers. `col_chars`/`len_chars` are character
/// offsets into that side's source line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffMatch {
    pub row: usize,
    pub side: DiffSide,
    pub col_chars: usize,
    pub len_chars: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffFindState {
    pub needle: Option<String>,
    pub opts: SearchOpts,
    pub active: Option<DiffMatch>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffSide {
    Left,
    Right,
}

/// The file and one-based line a diff row points at, used by Enter in the
/// diff view to open the underlying file at the right place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffJump {
    pub path: PathBuf,
    pub line: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffSelection {
    pub side: DiffSide,
    pub anchor_row: usize,
    pub anchor_col: usize,
    pub head_row: usize,
    pub head_col: usize,
}

impl DiffSelection {
    pub fn has_area(&self) -> bool {
        !(self.anchor_row == self.head_row && self.anchor_col == self.head_col)
    }

    pub fn normalized(&self) -> ((usize, usize), (usize, usize)) {
        let a = (self.anchor_row, self.anchor_col);
        let h = (self.head_row, self.head_col);
        if a <= h { (a, h) } else { (h, a) }
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// One visual row in a side-by-side diff view. The left column shows
/// `left_lines[left]` (or blank when `Added`) and the right column shows
/// `right_lines[right]` (or blank when `Removed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffRow {
    Equal {
        left: usize,
        right: usize,
    },
    Removed {
        left: usize,
    },
    Added {
        right: usize,
    },
    /// A line that was replaced: both columns paint, left in `left_lines`
    /// and right in `right_lines`. Produced by pairing consecutive
    /// Delete + Insert hunks so the visual rows align without forcing a
    /// pure-add or pure-remove zigzag.
    Replaced {
        left: usize,
        right: usize,
    },
}

impl DiffData {
    pub fn build(
        left_path: PathBuf,
        right_path: PathBuf,
        left_lines: Vec<String>,
        right_lines: Vec<String>,
    ) -> Self {
        Self::build_with_byte_check(left_path, right_path, left_lines, right_lines, None, None)
    }

    /// Like `build`, but also takes the raw byte contents so the
    /// constructor can detect the "byte-different but line-identical"
    /// case (trailing newline, CRLF, BOM, etc.) and set
    /// `bytes_differ_but_lines_equal` accordingly. Pass `None` for the
    /// raw texts when the caller doesn't have them (synthetic / test
    /// diffs); the flag stays false and the renderer behaves as before.
    pub fn build_with_byte_check(
        left_path: PathBuf,
        right_path: PathBuf,
        left_lines: Vec<String>,
        right_lines: Vec<String>,
        left_raw: Option<&str>,
        right_raw: Option<&str>,
    ) -> Self {
        let rows = build_diff_rows(&left_lines, &right_lines);
        let bytes_differ_but_lines_equal = match (left_raw, right_raw) {
            (Some(l), Some(r)) => l != r && rows.iter().all(|r| matches!(r, DiffRow::Equal { .. })),
            _ => false,
        };
        // `str::lines()` (the display split) strips `\r` and forgets whether
        // the file ended in a newline; record both so `hunk_patch` can emit
        // byte-exact lines and the `\ No newline at end of file` marker.
        let side_meta = |raw: Option<&str>, lines: usize| -> (Vec<bool>, bool) {
            match raw {
                Some(t) => (
                    t.split('\n')
                        .take(lines)
                        .map(|l| l.ends_with('\r'))
                        .collect(),
                    !t.is_empty() && !t.ends_with('\n'),
                ),
                None => (Vec::new(), false),
            }
        };
        let (left_crlf, left_no_final_nl) = side_meta(left_raw, left_lines.len());
        let (right_crlf, right_no_final_nl) = side_meta(right_raw, right_lines.len());
        Self {
            left_path,
            right_path,
            left_is_real_file: false,
            left_lines,
            right_lines,
            rows,
            scroll: 0,
            scroll_x: 0,
            bytes_differ_but_lines_equal,
            unified: false,
            selection: None,
            find: DiffFindState::default(),
            nav_anchor: None,
            left_is_git_head: false,
            left_crlf,
            right_crlf,
            left_no_final_nl,
            right_no_final_nl,
        }
    }

    /// Build a single-column unified diff that represents a fully-deleted
    /// file: every line of `text` becomes a `Removed` row keyed off
    /// `left_lines`. The right side is empty; the renderer keys off
    /// `unified == true` to draw a one-column view instead of two.
    pub fn build_unified_deletion(label: PathBuf, text: &str) -> Self {
        let left_lines: Vec<String> = text.lines().map(str::to_string).collect();
        let rows: Vec<DiffRow> = (0..left_lines.len())
            .map(|i| DiffRow::Removed { left: i })
            .collect();
        Self {
            left_path: label,
            right_path: PathBuf::from("/dev/null"),
            left_is_real_file: false,
            left_lines,
            right_lines: Vec::new(),
            rows,
            scroll: 0,
            scroll_x: 0,
            bytes_differ_but_lines_equal: false,
            unified: true,
            selection: None,
            find: DiffFindState::default(),
            nav_anchor: None,
            left_is_git_head: false,
            left_crlf: Vec::new(),
            right_crlf: Vec::new(),
            left_no_final_nl: false,
            right_no_final_nl: false,
        }
    }

    /// Build a side-by-side diff from the raw stdout of `git diff` (or
    /// `git diff --staged`, etc.) by separating the parsed text into a
    /// left (old) and right (new) stream and emitting the same
    /// Equal/Removed/Added/Replaced rows the two-column renderer uses
    /// for file-vs-file diffs. The result has `unified = false` so the
    /// editor pane renders it through the standard side-by-side path.
    ///
    /// Parsing rules per line of `raw`:
    /// * `diff --git …` and `@@ … @@` → Equal row in both columns
    ///   (acts as a visible file / hunk header).
    /// * `index …`, `--- …`, `+++ …`, `\ No newline at end of file`
    ///   → skipped (redundant after the `diff --git` header).
    /// * `-xxx` → push body to `left_lines`, buffer as pending removed.
    /// * `+xxx` → push body to `right_lines`, buffer as pending added.
    /// * any other line (context, blank) → flush buffered runs (pairing
    ///   removed+added into Replaced rows), then push the body to both
    ///   sides as an Equal row.
    ///
    /// Empty `raw` produces a single Equal row carrying "(no changes)"
    /// so the renderer always has something to paint.
    pub fn build_side_by_side_from_git_text(label: PathBuf, raw: &str) -> Self {
        let mut left_lines: Vec<String> = Vec::new();
        let mut right_lines: Vec<String> = Vec::new();
        let mut rows: Vec<DiffRow> = Vec::new();
        if raw.trim().is_empty() {
            left_lines.push(String::from("(no changes)"));
            right_lines.push(String::from("(no changes)"));
            rows.push(DiffRow::Equal { left: 0, right: 0 });
            return Self {
                left_path: label,
                right_path: PathBuf::new(),
                left_is_real_file: false,
                left_lines,
                right_lines,
                rows,
                scroll: 0,
                scroll_x: 0,
                bytes_differ_but_lines_equal: false,
                unified: false,
                selection: None,
                find: DiffFindState::default(),
                nav_anchor: None,
                left_is_git_head: false,
                left_crlf: Vec::new(),
                right_crlf: Vec::new(),
                left_no_final_nl: false,
                right_no_final_nl: false,
            };
        }
        let mut pending_remove: Vec<usize> = Vec::new();
        let mut pending_add: Vec<usize> = Vec::new();
        let flush = |pending_remove: &mut Vec<usize>,
                     pending_add: &mut Vec<usize>,
                     rows: &mut Vec<DiffRow>| {
            let pair = pending_remove.len().min(pending_add.len());
            for k in 0..pair {
                rows.push(DiffRow::Replaced {
                    left: pending_remove[k],
                    right: pending_add[k],
                });
            }
            for &left in pending_remove.iter().skip(pair) {
                rows.push(DiffRow::Removed { left });
            }
            for &right in pending_add.iter().skip(pair) {
                rows.push(DiffRow::Added { right });
            }
            pending_remove.clear();
            pending_add.clear();
        };
        for line in raw.split('\n') {
            if line.starts_with("diff --git") || line.starts_with("@@") {
                flush(&mut pending_remove, &mut pending_add, &mut rows);
                let i = left_lines.len();
                let j = right_lines.len();
                left_lines.push(line.to_string());
                right_lines.push(line.to_string());
                rows.push(DiffRow::Equal { left: i, right: j });
            } else if line.starts_with("index ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
                || line == "---"
                || line == "+++"
                || line.starts_with("\\ No newline")
                || line.starts_with("new file mode")
                || line.starts_with("deleted file mode")
                || line.starts_with("similarity index")
                || line.starts_with("rename from")
                || line.starts_with("rename to")
            {
                // Header / metadata noise — skip; the `diff --git` line
                // already names the file pair.
                continue;
            } else if let Some(rest) = line.strip_prefix('-') {
                let i = left_lines.len();
                left_lines.push(rest.to_string());
                pending_remove.push(i);
            } else if let Some(rest) = line.strip_prefix('+') {
                let j = right_lines.len();
                right_lines.push(rest.to_string());
                pending_add.push(j);
            } else {
                flush(&mut pending_remove, &mut pending_add, &mut rows);
                let body = line.strip_prefix(' ').unwrap_or(line);
                let i = left_lines.len();
                let j = right_lines.len();
                left_lines.push(body.to_string());
                right_lines.push(body.to_string());
                rows.push(DiffRow::Equal { left: i, right: j });
            }
        }
        flush(&mut pending_remove, &mut pending_add, &mut rows);
        Self {
            left_path: label,
            right_path: PathBuf::new(),
            left_is_real_file: false,
            left_lines,
            right_lines,
            rows,
            scroll: 0,
            scroll_x: 0,
            bytes_differ_but_lines_equal: false,
            unified: false,
            selection: None,
            find: DiffFindState::default(),
            nav_anchor: None,
            left_is_git_head: false,
            left_crlf: Vec::new(),
            right_crlf: Vec::new(),
            left_no_final_nl: false,
            right_no_final_nl: false,
        }
    }

    /// Every occurrence of `needle` across both columns, in reading order
    /// (top to bottom, left column before right within a row). An Equal
    /// row carries the same text on both sides, so a hit there is reported
    /// twice — once per visible column — matching what the user sees.
    pub fn find_matches(&self, needle: &str, opts: SearchOpts) -> Vec<DiffMatch> {
        let mut out: Vec<DiffMatch> = Vec::new();
        if needle.is_empty() {
            return out;
        }
        for (row_idx, row) in self.rows.iter().enumerate() {
            let left_idx = match row {
                DiffRow::Equal { left, .. }
                | DiffRow::Removed { left }
                | DiffRow::Replaced { left, .. } => Some(*left),
                DiffRow::Added { .. } => None,
            };
            if let Some(line) = left_idx.and_then(|i| self.left_lines.get(i)) {
                for (col, len) in line_matches(line, opts, needle) {
                    out.push(DiffMatch {
                        row: row_idx,
                        side: DiffSide::Left,
                        col_chars: col,
                        len_chars: len,
                    });
                }
            }
            let right_idx = match row {
                DiffRow::Equal { right, .. }
                | DiffRow::Added { right }
                | DiffRow::Replaced { right, .. } => Some(*right),
                DiffRow::Removed { .. } => None,
            };
            if let Some(line) = right_idx.and_then(|i| self.right_lines.get(i)) {
                for (col, len) in line_matches(line, opts, needle) {
                    out.push(DiffMatch {
                        row: row_idx,
                        side: DiffSide::Right,
                        col_chars: col,
                        len_chars: len,
                    });
                }
            }
        }
        out
    }

    /// Scroll the viewport so `m` is visible both vertically (its row lands
    /// inside `viewport_rows`) and horizontally (its char span lands inside
    /// `text_cols`). Mirrors the plain-editor `jump_editor_to_match` logic.
    pub fn scroll_to_match(&mut self, m: DiffMatch, viewport_rows: usize, text_cols: usize) {
        // A find jump relocates the viewport independently of hunk
        // navigation, so the next ‹/› should re-anchor on the match, not
        // on the stale hunk we last stepped to.
        self.nav_anchor = None;
        if viewport_rows > 0 {
            if m.row < self.scroll {
                self.scroll = m.row;
            } else if m.row >= self.scroll + viewport_rows {
                self.scroll = m.row + 1 - viewport_rows;
            }
        }
        if text_cols > 0 {
            let end = m.col_chars + m.len_chars;
            if m.col_chars < self.scroll_x {
                self.scroll_x = m.col_chars;
            } else if end > self.scroll_x + text_cols {
                self.scroll_x = end - text_cols;
            }
        }
    }

    pub fn start_selection(&mut self, side: DiffSide, row: usize, col: usize) {
        let row = row.min(self.rows.len().saturating_sub(1));
        // A click is also a hunk-action/jump anchor, like a landed jump; a
        // later jump or scroll then supersedes it (see `action_row`).
        self.nav_anchor = Some(row);
        self.selection = Some(DiffSelection {
            side,
            anchor_row: row,
            anchor_col: col,
            head_row: row,
            head_col: col,
        });
    }

    pub fn extend_selection_to(&mut self, row: usize, col: usize) {
        if let Some(sel) = self.selection.as_mut() {
            sel.head_row = row.min(self.rows.len().saturating_sub(1));
            sel.head_col = col;
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Double-click word selection. Snaps the selection to the word
    /// boundaries containing `col` on the source line that backs
    /// `row` on the chosen side. Same word-boundary semantics as the
    /// regular text editor: a "word" is a maximal run of
    /// alphanumeric-or-underscore characters. A click on whitespace
    /// or punctuation clears the selection and returns without anchoring
    /// anything, matching VS Code / standard text-editor behaviour.
    pub fn select_word_at(&mut self, side: DiffSide, row: usize, col: usize) {
        let Some(row_data) = self.rows.get(row).copied() else {
            self.selection = None;
            return;
        };
        let line_idx = match (side, row_data) {
            (DiffSide::Left, DiffRow::Equal { left, .. })
            | (DiffSide::Left, DiffRow::Removed { left })
            | (DiffSide::Left, DiffRow::Replaced { left, .. }) => Some(left),
            (DiffSide::Right, DiffRow::Equal { right, .. })
            | (DiffSide::Right, DiffRow::Added { right })
            | (DiffSide::Right, DiffRow::Replaced { right, .. }) => Some(right),
            _ => None,
        };
        let Some(line_idx) = line_idx else {
            self.selection = None;
            return;
        };
        let lines = match side {
            DiffSide::Left => &self.left_lines,
            DiffSide::Right => &self.right_lines,
        };
        let Some(text) = lines.get(line_idx) else {
            self.selection = None;
            return;
        };
        let chars: Vec<char> = text.chars().collect();
        if chars.is_empty() {
            self.selection = None;
            return;
        }
        let c = col.min(chars.len());
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
        self.nav_anchor = Some(row);
        self.selection = Some(DiffSelection {
            side,
            anchor_row: row,
            anchor_col: start,
            head_row: row,
            head_col: end,
        });
    }

    /// Linearise the active selection into a copyable string. Walks the
    /// chosen side's source-line texts for the selected `diff.rows` range,
    /// clipping the first and last lines to the selection's char bounds.
    /// Rows that are blank on the selected side (Added rows when Left is
    /// selected, or Removed rows when Right is selected) contribute an
    /// empty line so the row geometry the user sees matches the copied
    /// payload.
    pub fn selection_text(&self) -> Option<String> {
        let sel = self.selection?;
        if !sel.has_area() {
            return None;
        }
        let (start, end) = sel.normalized();
        let lines = match sel.side {
            DiffSide::Left => &self.left_lines,
            DiffSide::Right => &self.right_lines,
        };
        let mut out = String::new();
        for (emitted, row_idx) in (start.0..=end.0).enumerate() {
            let Some(row) = self.rows.get(row_idx).copied() else {
                break;
            };
            let line_idx = match (sel.side, row) {
                (DiffSide::Left, DiffRow::Equal { left, .. })
                | (DiffSide::Left, DiffRow::Removed { left })
                | (DiffSide::Left, DiffRow::Replaced { left, .. }) => Some(left),
                (DiffSide::Right, DiffRow::Equal { right, .. })
                | (DiffSide::Right, DiffRow::Added { right })
                | (DiffSide::Right, DiffRow::Replaced { right, .. }) => Some(right),
                _ => None,
            };
            let text = line_idx
                .and_then(|i| lines.get(i).cloned())
                .unwrap_or_default();
            let chars: Vec<char> = text.chars().collect();
            let (cs, ce) = if start.0 == end.0 {
                (start.1.min(chars.len()), end.1.min(chars.len()))
            } else if row_idx == start.0 {
                (start.1.min(chars.len()), chars.len())
            } else if row_idx == end.0 {
                (0, end.1.min(chars.len()))
            } else {
                (0, chars.len())
            };
            if emitted > 0 {
                out.push('\n');
            }
            out.extend(chars[cs..ce].iter());
        }
        if out.is_empty() { None } else { Some(out) }
    }

    /// The literal substring an active single-row selection covers, used to
    /// drive the "highlight every other occurrence" box (VS Code's
    /// `editor.selectionHighlight`). Returns `None` for a zero-area click,
    /// a multi-row selection (VS Code only highlights occurrences for
    /// single-line selections), a whitespace-only selection, or one past the
    /// `editor.selectionHighlightMaxLength` ceiling — mirroring the plain
    /// editor's `selection_occurrence_needle` exactly.
    pub fn selection_occurrence_needle(&self) -> Option<String> {
        let sel = self.selection?;
        if !sel.has_area() {
            return None;
        }
        let (start, end) = sel.normalized();
        if start.0 != end.0 {
            return None;
        }
        let line_idx = match (sel.side, *self.rows.get(start.0)?) {
            (DiffSide::Left, DiffRow::Equal { left, .. })
            | (DiffSide::Left, DiffRow::Removed { left })
            | (DiffSide::Left, DiffRow::Replaced { left, .. }) => left,
            (DiffSide::Right, DiffRow::Equal { right, .. })
            | (DiffSide::Right, DiffRow::Added { right })
            | (DiffSide::Right, DiffRow::Replaced { right, .. }) => right,
            _ => return None,
        };
        let lines = match sel.side {
            DiffSide::Left => &self.left_lines,
            DiffSide::Right => &self.right_lines,
        };
        let chars: Vec<char> = lines.get(line_idx)?.chars().collect();
        let cs = start.1.min(chars.len());
        let ce = end.1.min(chars.len());
        if ce <= cs {
            return None;
        }
        let text: String = chars[cs..ce].iter().collect();
        if text.trim().is_empty() || text.chars().count() > SELECTION_HIGHLIGHT_MAX_LEN {
            return None;
        }
        Some(text)
    }

    /// Length (in chars) of the longest line across both files. Used to
    /// clamp horizontal scrolling so the user can't pan into empty space
    /// past the end of the longest content.
    pub fn longest_line_chars(&self) -> usize {
        self.left_lines
            .iter()
            .chain(self.right_lines.iter())
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0)
    }

    pub fn total_rows(&self) -> usize {
        self.rows.len()
    }

    /// The caret cell, i.e. the head of the active selection. A single
    /// click leaves a zero-area selection whose head is exactly where the
    /// user clicked, so this doubles as a read-only text cursor the
    /// renderer can blink.
    pub fn caret(&self) -> Option<(DiffSide, usize, usize)> {
        self.selection.map(|s| (s.side, s.head_row, s.head_col))
    }

    pub fn caret_row(&self) -> Option<usize> {
        self.selection.map(|s| s.head_row)
    }

    fn row_left_text(&self, row_idx: usize) -> Option<&str> {
        match self.rows.get(row_idx)? {
            DiffRow::Equal { left, .. }
            | DiffRow::Removed { left }
            | DiffRow::Replaced { left, .. } => self.left_lines.get(*left).map(String::as_str),
            DiffRow::Added { .. } => None,
        }
    }

    /// Resolve a diff row to the file and one-based line it represents so
    /// Enter can open the working-tree file there.
    ///
    /// A single-file diff (built from two real paths) stores real file
    /// line numbers in each row, so the answer is `right_path` plus the
    /// row's new-side index. A multi-file `git diff` view stores the
    /// `diff --git` and `@@` headers as ordinary rows and indexes into a
    /// synthetic line array, so the file comes from the nearest preceding
    /// `diff --git` header and the line from the nearest `@@` hunk header
    /// plus a count of the right-bearing rows down to `row_idx`.
    fn nearest_right_line0(&self, row_idx: usize) -> Option<usize> {
        let right_of = |r: &DiffRow| match r {
            DiffRow::Equal { right, .. }
            | DiffRow::Added { right }
            | DiffRow::Replaced { right, .. } => Some(*right),
            DiffRow::Removed { .. } => None,
        };
        (row_idx + 1..self.rows.len())
            .find_map(|i| self.rows.get(i).and_then(right_of))
            .or_else(|| {
                (0..row_idx)
                    .rev()
                    .find_map(|i| self.rows.get(i).and_then(right_of))
            })
    }

    pub fn jump_target(&self, row_idx: usize, root: &Path) -> Option<DiffJump> {
        let row = *self.rows.get(row_idx)?;
        if !self.right_path.as_os_str().is_empty() {
            let (path, line0) = match row {
                DiffRow::Equal { right, .. }
                | DiffRow::Added { right }
                | DiffRow::Replaced { right, .. } => (self.right_path.clone(), right),
                // A removed line lives only on the left. When the left side
                // is a real file (two-file compare) open it there; when it's
                // a synthetic HEAD label the line is gone from the working
                // tree, so open the real right file at the nearest surviving
                // line instead of a label path that ENOENTs.
                DiffRow::Removed { left } => {
                    if self.left_is_real_file {
                        (self.left_path.clone(), left)
                    } else if self.right_path != Path::new("/dev/null") {
                        (self.right_path.clone(), self.nearest_right_line0(row_idx)?)
                    } else {
                        return None;
                    }
                }
            };
            return Some(DiffJump {
                path,
                line: line0 + 1,
            });
        }
        let mut file: Option<PathBuf> = None;
        let mut hunk_row: Option<usize> = None;
        let mut hunk_start: usize = 1;
        for i in (0..=row_idx).rev() {
            let Some(text) = self.row_left_text(i) else {
                continue;
            };
            if hunk_row.is_none() && text.starts_with("@@") {
                hunk_row = Some(i);
                if let Some(n) = parse_hunk_new_start(text) {
                    hunk_start = n;
                }
            }
            if let Some(rel) = parse_diff_git_new_path(text) {
                file = Some(root.join(rel));
                break;
            }
        }
        let path = file?;
        let start = hunk_row.unwrap_or(row_idx);
        let right_count = (start + 1..=row_idx)
            .filter(|&i| self.rows.get(i).is_some_and(row_has_right))
            .count();
        let line = if right_count == 0 {
            hunk_start
        } else {
            hunk_start + right_count - 1
        };
        Some(DiffJump { path, line })
    }

    pub fn scroll_up_by(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
        self.nav_anchor = None;
    }

    pub fn scroll_down_by(&mut self, n: usize) {
        let max = self.rows.len();
        // Clamp loosely here; render_diff also re-clamps against the live
        // viewport, so we don't need to know it at scroll time.
        self.scroll = (self.scroll + n).min(max);
        self.nav_anchor = None;
    }

    pub fn scroll_home(&mut self) {
        self.scroll = 0;
        self.nav_anchor = None;
    }

    pub fn scroll_end(&mut self) {
        self.scroll = self.rows.len();
        self.nav_anchor = None;
    }

    pub fn scroll_left_by(&mut self, n: usize) {
        self.scroll_x = self.scroll_x.saturating_sub(n);
    }

    pub fn scroll_right_by(&mut self, n: usize) {
        let max = self.longest_line_chars();
        self.scroll_x = (self.scroll_x + n).min(max);
    }

    /// Indices into `rows` where each contiguous change hunk begins (an
    /// Added / Removed / Replaced row preceded by either start-of-file or
    /// an Equal row). Drives Next / Prev change navigation in the diff
    /// view and the "jump to first change on open" behaviour the Source
    /// Control panel relies on.
    pub fn hunk_starts(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut in_hunk = false;
        for (i, r) in self.rows.iter().enumerate() {
            let is_change = !matches!(r, DiffRow::Equal { .. });
            if is_change && !in_hunk {
                out.push(i);
                in_hunk = true;
            } else if !is_change {
                in_hunk = false;
            }
        }
        out
    }

    /// First-row index of the first change hunk, or `None` when the two
    /// sides are identical.
    pub fn first_change_row(&self) -> Option<usize> {
        self.hunk_starts().into_iter().next()
    }

    /// First-row index of the next hunk strictly past `current`, or
    /// `None` when `current` is already in/past the last hunk.
    pub fn next_change_row(&self, current: usize) -> Option<usize> {
        self.hunk_starts().into_iter().find(|&s| s > current)
    }

    /// First-row index of the hunk that starts strictly before `current`,
    /// or `None` when `current` is already at/before the first hunk.
    pub fn prev_change_row(&self, current: usize) -> Option<usize> {
        self.hunk_starts().into_iter().rev().find(|&s| s < current)
    }

    /// Like `next_change_row`, but wraps around to the first hunk when
    /// `current` is at/past the last one. Used by the diff-pane ›
    /// arrow and F7 so a user reading bottom-to-top can keep clicking
    /// the same arrow to cycle through every hunk. Returns `None` only
    /// when the diff has no change rows at all.
    pub fn next_change_row_wrap(&self, current: usize) -> Option<usize> {
        let starts = self.hunk_starts();
        if starts.is_empty() {
            return None;
        }
        starts
            .iter()
            .find(|&&s| s > current)
            .copied()
            .or_else(|| starts.first().copied())
    }

    /// Mirror of `next_change_row_wrap` going the other way: when
    /// `current` sits at/before the first hunk, the next call wraps to
    /// the last hunk instead of stalling.
    pub fn prev_change_row_wrap(&self, current: usize) -> Option<usize> {
        let starts = self.hunk_starts();
        if starts.is_empty() {
            return None;
        }
        starts
            .iter()
            .rev()
            .find(|&&s| s < current)
            .copied()
            .or_else(|| starts.last().copied())
    }

    /// Park `scroll` two rows above `target` so the change row lands with
    /// a slice of context above it, the way users read diffs.
    pub fn scroll_to_row(&mut self, target: usize) {
        self.scroll = target.saturating_sub(2);
    }

    /// Row to anchor the next hunk-to-hunk jump on. Prefers `nav_anchor`
    /// (the row the previous jump landed on), which is immune to the
    /// `render_diff` scroll clamp. Falls back to the row currently sitting
    /// two below the top of the viewport (the hunk `scroll_to_row` parks in
    /// view) when no jump is in flight, e.g. right after the diff opens or
    /// after a manual scroll cleared the anchor.
    fn jump_anchor(&self) -> usize {
        self.nav_anchor
            .unwrap_or_else(|| self.scroll.saturating_add(2))
    }

    /// Scroll to the next change hunk, wrapping from the last hunk back to
    /// the first. Tracks the landed row in `nav_anchor` so repeated forward
    /// jumps keep advancing even after the renderer clamps `scroll`. The
    /// shared backend for the diff-pane › arrow and the F7 keybinding.
    pub fn jump_next_change(&mut self) {
        if let Some(row) = self.next_change_row_wrap(self.jump_anchor()) {
            self.scroll_to_row(row);
            self.nav_anchor = Some(row);
        }
    }

    /// Mirror of `jump_next_change`: previous hunk, wrapping from the first
    /// back to the last. Backs the ‹ arrow and Shift+F7.
    pub fn jump_prev_change(&mut self) {
        if let Some(row) = self.prev_change_row_wrap(self.jump_anchor()) {
            self.scroll_to_row(row);
            self.nav_anchor = Some(row);
        }
    }

    /// The row hunk actions (stage / unstage / revert) anchor on: the most
    /// recent click or hunk jump (both set `nav_anchor`, so the newer one
    /// wins), else the top of the viewport — `hunk_range_at`'s fall-forward
    /// then lands on the first visible hunk. A manual scroll clears the
    /// anchor, so the action always targets something the user has seen:
    /// a stale off-screen caret must never pick the hunk `R` destroys.
    pub fn action_row(&self) -> usize {
        self.nav_anchor.unwrap_or(self.scroll)
    }

    /// Inclusive row range of the contiguous change hunk containing
    /// `row`. A `row` sitting on context falls forward to the next hunk,
    /// the same forgiving anchor Next Change uses. `None` when no change
    /// hunk exists at or after `row`.
    pub fn hunk_range_at(&self, row: usize) -> Option<(usize, usize)> {
        if self.rows.is_empty() {
            return None;
        }
        let is_change = |i: usize| !matches!(self.rows.get(i), Some(DiffRow::Equal { .. }) | None);
        let anchor = row.min(self.rows.len() - 1);
        let start = if is_change(anchor) {
            anchor
        } else {
            (anchor..self.rows.len()).find(|&i| is_change(i))?
        };
        let mut s = start;
        while s > 0 && is_change(s - 1) {
            s -= 1;
        }
        let mut e = start;
        while e + 1 < self.rows.len() && is_change(e + 1) {
            e += 1;
        }
        Some((s, e))
    }

    /// A minimal unified diff containing only the hunk at `range` plus up
    /// to three lines of surrounding context, in the exact shape
    /// `git apply` accepts on stdin (`--- a/rel`, `+++ b/rel`, one `@@`
    /// header). Because `git apply` verifies a hunk against its context
    /// (with offset search), the same patch stages (`--cached`), unstages
    /// (`--cached -R`) or reverts (`-R`) that one hunk.
    /// A patch that stages ONLY the rows in `sel` (VS Code's "Stage
    /// Selected Ranges"), with every other row of the surrounding hunk
    /// represented as the index already has it: an unselected `+` is
    /// dropped (it is not being staged) and an unselected `-` becomes a
    /// context line (that line still exists in HEAD). Applied with `-R`
    /// the same patch unstages exactly those lines.
    ///
    /// `None` when the range covers no changed row, so the caller can
    /// fall back to the whole-hunk action.
    pub fn selected_lines_patch(&self, rel_path: &str, sel: (usize, usize)) -> Option<String> {
        const CTX: usize = 3;
        let (sel_start, sel_end) = (sel.0.min(sel.1), sel.0.max(sel.1));
        let selected = |i: usize| i >= sel_start && i <= sel_end;
        let is_change = |i: usize| !matches!(self.rows.get(i), Some(DiffRow::Equal { .. }) | None);
        if !(sel_start..=sel_end.min(self.rows.len().saturating_sub(1))).any(is_change) {
            return None;
        }
        // Widen to the enclosing change run so the patch carries the whole
        // hunk (with the unselected rows neutralised), then add context.
        let mut hs = sel_start;
        while hs > 0 && is_change(hs - 1) {
            hs -= 1;
        }
        let mut he = sel_end.min(self.rows.len().saturating_sub(1));
        while he + 1 < self.rows.len() && is_change(he + 1) {
            he += 1;
        }
        let s = hs.saturating_sub(CTX);
        let e = (he + CTX).min(self.rows.len().saturating_sub(1));
        const NO_NL: &str = "\\ No newline at end of file";
        let left_line = |sig: char, i: usize| {
            let cr = if self.left_crlf.get(i).copied().unwrap_or(false) {
                "\r"
            } else {
                ""
            };
            format!("{sig}{}{cr}", self.left_lines[i])
        };
        let right_line = |sig: char, i: usize| {
            let cr = if self.right_crlf.get(i).copied().unwrap_or(false) {
                "\r"
            } else {
                ""
            };
            format!("{sig}{}{cr}", self.right_lines[i])
        };
        let left_last = |i: usize| i + 1 == self.left_lines.len() && self.left_no_final_nl;
        let right_last = |i: usize| i + 1 == self.right_lines.len() && self.right_no_final_nl;
        let mut old_start: Option<usize> = None;
        let mut new_start: Option<usize> = None;
        let mut old_count = 0usize;
        let mut new_count = 0usize;
        let mut body: Vec<String> = Vec::new();
        let mut minus: Vec<String> = Vec::new();
        let mut plus: Vec<String> = Vec::new();
        // Emit a HEAD line as context: it exists on both sides of this
        // patch, because the change touching it is not being staged.
        // Emit a HEAD line as context: the change touching it is not being
        // staged, so it exists unchanged on both sides of THIS patch.
        // Only the old-side start is recorded here; the new side's start
        // is derived below (an unselected change contributes no new-side
        // line number of its own).
        let push_context_left = |i: usize,
                                 body: &mut Vec<String>,
                                 old_start: &mut Option<usize>,
                                 old_count: &mut usize,
                                 new_count: &mut usize| {
            old_start.get_or_insert(i + 1);
            *old_count += 1;
            *new_count += 1;
            body.push(left_line(' ', i));
            if left_last(i) {
                body.push(NO_NL.into());
            }
        };
        for i in s..=e {
            match self.rows[i] {
                DiffRow::Equal { left, right } => {
                    body.append(&mut minus);
                    body.append(&mut plus);
                    old_start.get_or_insert(left + 1);
                    new_start.get_or_insert(right + 1);
                    old_count += 1;
                    new_count += 1;
                    body.push(left_line(' ', left));
                    if left_last(left) && right_last(right) {
                        body.push(NO_NL.into());
                    }
                }
                DiffRow::Removed { left } => {
                    if selected(i) {
                        old_start.get_or_insert(left + 1);
                        old_count += 1;
                        minus.push(left_line('-', left));
                        if left_last(left) {
                            minus.push(NO_NL.into());
                        }
                    } else {
                        body.append(&mut minus);
                        body.append(&mut plus);
                        push_context_left(
                            left,
                            &mut body,
                            &mut old_start,
                            &mut old_count,
                            &mut new_count,
                        );
                    }
                }
                DiffRow::Added { right } => {
                    if selected(i) {
                        new_start.get_or_insert(right + 1);
                        new_count += 1;
                        plus.push(right_line('+', right));
                        if right_last(right) {
                            plus.push(NO_NL.into());
                        }
                    }
                    // An unselected addition simply does not exist in the
                    // index-to-be: nothing to emit.
                }
                DiffRow::Replaced { left, right } => {
                    if selected(i) {
                        old_start.get_or_insert(left + 1);
                        new_start.get_or_insert(right + 1);
                        old_count += 1;
                        new_count += 1;
                        minus.push(left_line('-', left));
                        if left_last(left) {
                            minus.push(NO_NL.into());
                        }
                        plus.push(right_line('+', right));
                        if right_last(right) {
                            plus.push(NO_NL.into());
                        }
                    } else {
                        body.append(&mut minus);
                        body.append(&mut plus);
                        push_context_left(
                            left,
                            &mut body,
                            &mut old_start,
                            &mut old_count,
                            &mut new_count,
                        );
                    }
                }
            }
        }
        body.append(&mut minus);
        body.append(&mut plus);
        let old_start = old_start.unwrap_or(1);
        // No selected change contributed a new-side start, which happens
        // when the patch opens on context (including a neutralised
        // unselected change): the two sides are still in step there, so
        // the old-side number is also the new-side one.
        let new_start = new_start.unwrap_or(old_start);
        Some(format!(
            "--- a/{rel_path}\n+++ b/{rel_path}\n@@ -{old_start},{old_count} +{new_start},{new_count} @@\n{}\n",
            body.join("\n")
        ))
    }

    pub fn hunk_patch(&self, rel_path: &str, range: (usize, usize)) -> String {
        const CTX: usize = 3;
        let (hs, he) = range;
        let s = hs.saturating_sub(CTX).max(
            // Context must not swallow a neighbouring hunk: back up only
            // across Equal rows.
            (0..hs)
                .rev()
                .take(CTX)
                .take_while(|&i| matches!(self.rows.get(i), Some(DiffRow::Equal { .. })))
                .last()
                .unwrap_or(hs),
        );
        let e = {
            let mut e = he;
            for i in he + 1..self.rows.len() {
                if i > he + CTX || !matches!(self.rows.get(i), Some(DiffRow::Equal { .. })) {
                    break;
                }
                e = i;
            }
            e
        };
        let mut old_start: Option<usize> = None;
        let mut new_start: Option<usize> = None;
        let mut old_count = 0usize;
        let mut new_count = 0usize;
        let mut body: Vec<String> = Vec::new();
        // The display lines were `str::lines()`-cleaned; restore each line's
        // `\r` so the patch byte-matches a CRLF file, and mark a missing
        // final newline the way git spells it — otherwise `git apply`
        // rejects every hunk that reaches the end of such a file.
        const NO_NL: &str = "\\ No newline at end of file";
        let left_line = |sig: char, i: usize| {
            let cr = if self.left_crlf.get(i).copied().unwrap_or(false) {
                "\r"
            } else {
                ""
            };
            format!("{sig}{}{cr}", self.left_lines[i])
        };
        let right_line = |sig: char, i: usize| {
            let cr = if self.right_crlf.get(i).copied().unwrap_or(false) {
                "\r"
            } else {
                ""
            };
            format!("{sig}{}{cr}", self.right_lines[i])
        };
        let left_last = |i: usize| i + 1 == self.left_lines.len() && self.left_no_final_nl;
        let right_last = |i: usize| i + 1 == self.right_lines.len() && self.right_no_final_nl;
        // Within a change run, emit every `-` line before the `+` lines so
        // Replaced pairs read as a canonical delete-then-insert block.
        let mut minus: Vec<String> = Vec::new();
        let mut plus: Vec<String> = Vec::new();
        for i in s..=e {
            match self.rows[i] {
                DiffRow::Equal { left, right } => {
                    body.append(&mut minus);
                    body.append(&mut plus);
                    old_start.get_or_insert(left + 1);
                    new_start.get_or_insert(right + 1);
                    old_count += 1;
                    new_count += 1;
                    body.push(left_line(' ', left));
                    // A context marker asserts BOTH sides end unterminated;
                    // if only one does, the last "equal" line is not equal
                    // byte-wise and git rightly refuses the patch.
                    if left_last(left) && right_last(right) {
                        body.push(NO_NL.into());
                    }
                }
                DiffRow::Removed { left } => {
                    old_start.get_or_insert(left + 1);
                    old_count += 1;
                    minus.push(left_line('-', left));
                    if left_last(left) {
                        minus.push(NO_NL.into());
                    }
                }
                DiffRow::Added { right } => {
                    new_start.get_or_insert(right + 1);
                    new_count += 1;
                    plus.push(right_line('+', right));
                    if right_last(right) {
                        plus.push(NO_NL.into());
                    }
                }
                DiffRow::Replaced { left, right } => {
                    old_start.get_or_insert(left + 1);
                    new_start.get_or_insert(right + 1);
                    old_count += 1;
                    new_count += 1;
                    minus.push(left_line('-', left));
                    if left_last(left) {
                        minus.push(NO_NL.into());
                    }
                    plus.push(right_line('+', right));
                    if right_last(right) {
                        plus.push(NO_NL.into());
                    }
                }
            }
        }
        body.append(&mut minus);
        body.append(&mut plus);
        // A side with zero lines names the line BEFORE the change in its
        // `@@` field (`-N,0` / `+N,0`), per the unified-diff format. That
        // only happens with no context at all (edits touching an empty
        // side), so derive it from the other side's insertion point.
        let old_start = old_start.unwrap_or_else(|| {
            (e + 1..self.rows.len())
                .find_map(|i| match self.rows.get(i) {
                    Some(DiffRow::Equal { left, .. })
                    | Some(DiffRow::Removed { left })
                    | Some(DiffRow::Replaced { left, .. }) => Some(*left),
                    _ => None,
                })
                .unwrap_or(self.left_lines.len())
        });
        let new_start = new_start.unwrap_or_else(|| {
            (e + 1..self.rows.len())
                .find_map(|i| match self.rows.get(i) {
                    Some(DiffRow::Equal { right, .. })
                    | Some(DiffRow::Added { right })
                    | Some(DiffRow::Replaced { right, .. }) => Some(*right),
                    _ => None,
                })
                .unwrap_or(self.right_lines.len())
        });
        format!(
            "--- a/{rel_path}\n+++ b/{rel_path}\n@@ -{old_start},{old_count} +{new_start},{new_count} @@\n{}\n",
            body.join("\n")
        )
    }
}

fn row_has_right(row: &DiffRow) -> bool {
    matches!(
        row,
        DiffRow::Equal { .. } | DiffRow::Added { .. } | DiffRow::Replaced { .. }
    )
}

/// New-file start line from a `@@ -a,b +c,d @@` header, i.e. the `c`.
fn parse_hunk_new_start(text: &str) -> Option<usize> {
    let plus = text.find('+')?;
    let after = &text[plus + 1..];
    let end = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    after[..end].parse().ok()
}

/// The new-side path from a `diff --git a/<old> b/<new>` header line.
fn parse_diff_git_new_path(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("diff --git ")?;
    let idx = rest.find(" b/")?;
    Some(&rest[idx + 3..])
}

/// Run a line-level diff over `left` vs `right` and emit one DiffRow per
/// visual row. Adjacent Delete + Insert runs are paired (so a one-line
/// "edit" becomes a single Replaced row, not a Removed row above an Added
/// row), which is what makes the side-by-side alignment readable.
pub fn build_diff_rows(left: &[String], right: &[String]) -> Vec<DiffRow> {
    use similar::{ChangeTag, TextDiff};
    let l: Vec<&str> = left.iter().map(|s| s.as_str()).collect();
    let r: Vec<&str> = right.iter().map(|s| s.as_str()).collect();
    let diff = TextDiff::from_slices(&l, &r);
    let changes: Vec<_> = diff.iter_all_changes().collect();
    let mut rows = Vec::new();
    let mut li = 0usize;
    let mut ri = 0usize;
    let mut i = 0usize;
    while i < changes.len() {
        match changes[i].tag() {
            ChangeTag::Equal => {
                rows.push(DiffRow::Equal {
                    left: li,
                    right: ri,
                });
                li += 1;
                ri += 1;
                i += 1;
            }
            _ => {
                let mut removed: Vec<usize> = Vec::new();
                while i < changes.len() && changes[i].tag() == ChangeTag::Delete {
                    removed.push(li);
                    li += 1;
                    i += 1;
                }
                let mut added: Vec<usize> = Vec::new();
                while i < changes.len() && changes[i].tag() == ChangeTag::Insert {
                    added.push(ri);
                    ri += 1;
                    i += 1;
                }
                let pair = removed.len().min(added.len());
                for k in 0..pair {
                    rows.push(DiffRow::Replaced {
                        left: removed[k],
                        right: added[k],
                    });
                }
                for &left in removed.iter().skip(pair) {
                    rows.push(DiffRow::Removed { left });
                }
                for &right in added.iter().skip(pair) {
                    rows.push(DiffRow::Added { right });
                }
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn identical_inputs_yield_only_equal_rows() {
        let rows = build_diff_rows(
            &lines(&["alpha", "beta", "gamma"]),
            &lines(&["alpha", "beta", "gamma"]),
        );
        assert_eq!(rows.len(), 3);
        for r in rows {
            assert!(matches!(r, DiffRow::Equal { .. }));
        }
    }

    #[test]
    fn pure_addition_emits_added_rows() {
        let rows = build_diff_rows(&lines(&[]), &lines(&["new1", "new2"]));
        assert_eq!(
            rows,
            vec![DiffRow::Added { right: 0 }, DiffRow::Added { right: 1 },]
        );
    }

    #[test]
    fn pure_removal_emits_removed_rows() {
        let rows = build_diff_rows(&lines(&["old1", "old2"]), &lines(&[]));
        assert_eq!(
            rows,
            vec![DiffRow::Removed { left: 0 }, DiffRow::Removed { left: 1 },]
        );
    }

    #[test]
    fn one_line_edit_is_paired_into_a_single_replaced_row() {
        let rows = build_diff_rows(
            &lines(&["alpha", "beta", "gamma"]),
            &lines(&["alpha", "BETA", "gamma"]),
        );
        assert_eq!(
            rows,
            vec![
                DiffRow::Equal { left: 0, right: 0 },
                DiffRow::Replaced { left: 1, right: 1 },
                DiffRow::Equal { left: 2, right: 2 },
            ]
        );
    }

    #[test]
    fn unequal_run_lengths_pair_what_they_can_then_overflow() {
        // 3 removed + 1 added → 1 replaced + 2 removed (added-side runs out).
        let rows = build_diff_rows(&lines(&["a", "b", "c", "d"]), &lines(&["A"]));
        let kinds: Vec<&'static str> = rows
            .iter()
            .map(|r| match r {
                DiffRow::Equal { .. } => "eq",
                DiffRow::Replaced { .. } => "rep",
                DiffRow::Removed { .. } => "rm",
                DiffRow::Added { .. } => "add",
            })
            .collect();
        assert_eq!(kinds, vec!["rep", "rm", "rm", "rm"]);
    }

    #[test]
    fn horizontal_scroll_helpers_clamp_at_longest_line() {
        let mut d = DiffData::build(
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            lines(&["short", "medium length", "much-much-much-much-much-longer"]),
            lines(&["short", "medium length", "much-much-much-much-much-longer"]),
        );
        assert_eq!(d.scroll_x, 0);
        d.scroll_left_by(5);
        assert_eq!(d.scroll_x, 0, "saturating sub at 0");
        d.scroll_right_by(10);
        assert_eq!(d.scroll_x, 10);
        let longest = d.longest_line_chars();
        d.scroll_right_by(99_999);
        assert_eq!(d.scroll_x, longest, "right scroll clamps at longest line");
        d.scroll_left_by(longest);
        assert_eq!(d.scroll_x, 0);
    }

    #[test]
    fn scroll_helpers_clamp_at_zero_and_total() {
        let mut d = DiffData::build(
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            lines(&["1", "2", "3", "4", "5"]),
            lines(&["1", "2", "X", "4", "5"]),
        );
        assert_eq!(d.scroll, 0);
        d.scroll_up_by(3);
        assert_eq!(d.scroll, 0, "saturating sub at 0");
        d.scroll_down_by(2);
        assert_eq!(d.scroll, 2);
        d.scroll_down_by(99);
        assert_eq!(d.scroll, d.rows.len(), "clamp at total rows");
        d.scroll_home();
        assert_eq!(d.scroll, 0);
        d.scroll_end();
        assert_eq!(d.scroll, d.rows.len());
    }

    #[test]
    fn build_data_loads_paths_and_rows() {
        let d = DiffData::build(
            PathBuf::from("/tmp/a.txt"),
            PathBuf::from("/tmp/b.txt"),
            lines(&["x", "y"]),
            lines(&["x", "Y"]),
        );
        assert_eq!(d.total_rows(), 2);
        assert_eq!(d.left_lines, lines(&["x", "y"]));
        assert_eq!(d.right_lines, lines(&["x", "Y"]));
        assert_eq!(d.scroll, 0);
    }

    #[test]
    fn caret_reports_the_selection_head_side_row_and_col() {
        let mut d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["alpha", "bravo"]),
            lines(&["alpha", "BRAVO"]),
        );
        assert_eq!(d.caret(), None, "no selection means no caret");
        d.start_selection(DiffSide::Right, 1, 3);
        assert_eq!(d.caret(), Some((DiffSide::Right, 1, 3)));
        assert_eq!(d.caret_row(), Some(1));
    }

    #[test]
    fn jump_target_single_file_maps_row_to_right_path_and_one_based_line() {
        let d = DiffData::build(
            PathBuf::from("/tmp/a.txt"),
            PathBuf::from("/tmp/b.txt"),
            lines(&["x", "y"]),
            lines(&["x", "Y"]),
        );
        // Row 1 is the Replaced y -> Y row; its new-file line is index 1 + 1.
        let j = d.jump_target(1, std::path::Path::new("/repo")).unwrap();
        assert_eq!(j.path, PathBuf::from("/tmp/b.txt"));
        assert_eq!(j.line, 2);
    }

    #[test]
    fn jump_target_git_text_resolves_file_from_header_and_line_from_hunk() {
        let raw = concat!(
            "diff --git a/src/foo.rs b/src/foo.rs\n",
            "index 1111111..2222222 100644\n",
            "--- a/src/foo.rs\n",
            "+++ b/src/foo.rs\n",
            "@@ -10,3 +10,4 @@ fn x() {\n",
            " keep1\n",
            "-old\n",
            "+new1\n",
            "+new2\n",
            " keep2\n",
        );
        let d = DiffData::build_side_by_side_from_git_text(PathBuf::from("git diff"), raw);
        // Rows: 0 diff-header, 1 @@-header, 2 keep1(10), 3 old->new1(11),
        // 4 +new2(12), 5 keep2(13).
        let root = std::path::Path::new("/repo");
        let added = d.jump_target(4, root).unwrap();
        assert_eq!(added.path, PathBuf::from("/repo/src/foo.rs"));
        assert_eq!(added.line, 12, "the +new2 row is new-file line 12");
        let keep = d.jump_target(2, root).unwrap();
        assert_eq!(keep.line, 10, "the first kept row is new-file line 10");
        let replaced = d.jump_target(3, root).unwrap();
        assert_eq!(replaced.line, 11);
    }

    #[test]
    fn jump_target_removed_row_with_real_left_opens_the_left_file() {
        let mut d = DiffData::build(
            PathBuf::from("/tmp/a.txt"),
            PathBuf::from("/tmp/b.txt"),
            lines(&["keep", "gone"]),
            lines(&["keep"]),
        );
        d.left_is_real_file = true;
        // Rows: 0 Equal keep, 1 Removed gone (left index 1).
        let j = d.jump_target(1, std::path::Path::new("/repo")).unwrap();
        assert_eq!(j.path, PathBuf::from("/tmp/a.txt"));
        assert_eq!(j.line, 2, "the removed line is left-file line 2");
    }

    #[test]
    fn jump_target_removed_row_with_synthetic_left_opens_the_working_right_file() {
        // HEAD-vs-working diff: the left "path" is only a label, the right is
        // the real file. A removed line is gone from the working tree, so
        // Enter must land in the real right file, not the label that ENOENTs.
        let d = DiffData::build(
            PathBuf::from("foo.txt (HEAD)"),
            PathBuf::from("/tmp/foo.txt"),
            lines(&["keep", "gone"]),
            lines(&["keep"]),
        );
        let j = d.jump_target(1, std::path::Path::new("/repo")).unwrap();
        assert_eq!(
            j.path,
            PathBuf::from("/tmp/foo.txt"),
            "left_is_real_file is false for a HEAD label, so the removed row resolves to the working file on the right"
        );
    }

    #[test]
    fn first_change_row_skips_leading_equal_rows() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d"]),
            lines(&["a", "b", "X", "d"]),
        );
        assert_eq!(
            d.first_change_row(),
            Some(2),
            "first change must skip the two leading Equal rows"
        );
    }

    #[test]
    fn first_change_row_is_none_for_identical_inputs() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b"]),
            lines(&["a", "b"]),
        );
        assert_eq!(d.first_change_row(), None);
    }

    #[test]
    fn hunk_starts_lists_one_index_per_contiguous_change_block() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d", "e", "f"]),
            lines(&["a", "B", "c", "d", "E", "F"]),
        );
        // Hunk 1: row 1 (Replaced b→B), Hunk 2: rows 4-5 (Replaced e→E, f→F).
        let starts = d.hunk_starts();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0], 1);
        assert_eq!(starts[1], 4);
    }

    #[test]
    fn hunk_range_at_expands_to_the_contiguous_change_block() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d", "e", "f"]),
            lines(&["a", "B", "c", "d", "E", "F"]),
        );
        assert_eq!(d.hunk_range_at(1), Some((1, 1)));
        assert_eq!(d.hunk_range_at(4), Some((4, 5)));
        assert_eq!(d.hunk_range_at(5), Some((4, 5)));
        // A context row falls forward to the next hunk, like Next Change.
        assert_eq!(d.hunk_range_at(2), Some((4, 5)));
        assert_eq!(d.hunk_range_at(0), Some((1, 1)));
    }

    #[test]
    fn a_hunk_jump_after_a_click_moves_the_action_row() {
        // Two hunks far apart; click on the first, then jump to the second.
        // Stage/unstage/revert must act on the jump target, not the stale
        // click, or `R` destroys a hunk the user is no longer looking at.
        let left: Vec<String> = (1..=20).map(|i| format!("line{i}")).collect();
        let mut right = left.clone();
        right[1] = "LINE2".into();
        right[17] = "LINE18".into();
        let mut d = DiffData::build(PathBuf::new(), PathBuf::new(), left, right);
        d.start_selection(DiffSide::Right, 1, 0);
        assert_eq!(d.action_row(), 1, "a click targets the clicked row");
        d.jump_next_change();
        assert_eq!(
            d.action_row(),
            17,
            "the jump target must win over the stale click"
        );
    }

    /// Stage a croft-built hunk patch with real `git apply` — the only
    /// oracle that matters for the patch shape.
    fn git_apply_check(root: &std::path::Path, patch: &str) -> Result<(), String> {
        use std::io::Write;
        let mut child = std::process::Command::new("git")
            .args(["-C", root.to_str().unwrap(), "apply", "--cached", "--check"])
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(patch.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    }

    fn git_repo_with(root: &std::path::Path, content: &str) {
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(["-C", root.to_str().unwrap()])
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(root.join("f.txt"), content).unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
    }

    fn head_diff(root: &std::path::Path, head: &str, work: &str) -> DiffData {
        DiffData::build_with_byte_check(
            PathBuf::new(),
            root.join("f.txt"),
            head.lines().map(str::to_string).collect(),
            work.lines().map(str::to_string).collect(),
            Some(head),
            Some(work),
        )
    }

    #[test]
    fn hunk_patches_apply_to_files_without_a_final_newline() {
        // `str::lines()` erases the missing-newline fact, so the patch used
        // to claim `b\n` against a file ending in `b` and git rejected it:
        // every hunk within three context lines of EOF was dead.
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (head, work) = ("a\nb", "a\nB");
        git_repo_with(&root, head);
        std::fs::write(root.join("f.txt"), work).unwrap();
        let d = head_diff(&root, head, work);
        let range = d.hunk_range_at(0).unwrap();
        let patch = d.hunk_patch("f.txt", range);
        if let Err(e) = git_apply_check(&root, &patch) {
            panic!("git apply rejected the patch:\n{patch}\n{e}");
        }
    }

    #[test]
    fn hunk_patches_apply_to_crlf_files() {
        // `str::lines()` also strips the `\r`, so a patch against a CRLF
        // file never matched a single line of it.
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (head, work) = (
            "l1\r\nl2\r\nl3\r\nl4\r\nl5\r\n",
            "l1\r\nL2\r\nl3\r\nl4\r\nl5\r\n",
        );
        git_repo_with(&root, head);
        std::fs::write(root.join("f.txt"), work).unwrap();
        let d = head_diff(&root, head, work);
        let range = d.hunk_range_at(1).unwrap();
        let patch = d.hunk_patch("f.txt", range);
        if let Err(e) = git_apply_check(&root, &patch) {
            panic!("git apply rejected the patch:\n{patch}\n{e}");
        }
    }

    /// Issue #214: stage only the SELECTED lines. Rows outside the
    /// selection must be represented as the index already has them: an
    /// unselected `+` is dropped (it is not being staged), an unselected
    /// `-` becomes context (that line still exists in HEAD).
    /// The patch must not just LOOK right: git has to accept it and the
    /// index must end up holding exactly the selected line. Runs a real
    /// repo end to end (issue #214).
    /// The risky shape: the patch opens on an UNSELECTED change (no
    /// leading context to anchor the line numbers). git must still accept
    /// it and stage only the selected line.
    #[test]
    fn git_accepts_a_patch_that_opens_on_an_unselected_change() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let head = "a\nb\nc\n";
        let work = "X\nY\nc\n";
        git_repo_with(root, head);
        std::fs::write(root.join("f.txt"), work).unwrap();
        let d = head_diff(root, head, work);
        // Rows 0 and 1 are both changes; select only the SECOND.
        let patch = d.selected_lines_patch("f.txt", (1, 1)).unwrap();
        if let Err(e) = git_apply_check(root, &patch) {
            panic!("git rejected the patch:\n{patch}\n{e}");
        }
        let applied = std::process::Command::new("git")
            .args(["-C", root.to_str().unwrap(), "apply", "--cached"])
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                c.stdin.take().unwrap().write_all(patch.as_bytes())?;
                c.wait()
            })
            .map(|st| st.success())
            .unwrap_or(false);
        assert!(applied, "git apply --cached must succeed for:\n{patch}");
        let staged = std::process::Command::new("git")
            .args(["-C", root.to_str().unwrap(), "show", ":f.txt"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&staged.stdout),
            "a\nY\nc\n",
            "the unselected first change must stay out of the index"
        );
    }

    #[test]
    fn git_accepts_a_selected_lines_patch_and_stages_only_those_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let head = "a\nb\nc\nd\n";
        let work = "a\nX\nY\nd\n";
        git_repo_with(root, head);
        std::fs::write(root.join("f.txt"), work).unwrap();
        let d = head_diff(root, head, work);
        // Row 1 is b -> X; row 2 is c -> Y. Stage only the first.
        let patch = d.selected_lines_patch("f.txt", (1, 1)).unwrap();
        if let Err(e) = git_apply_check(root, &patch) {
            panic!("git rejected the selected-lines patch:\n{patch}\n{e}");
        }
        let ok = std::process::Command::new("git")
            .args(["-C", root.to_str().unwrap(), "apply", "--cached"])
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                c.stdin.take().unwrap().write_all(patch.as_bytes())?;
                c.wait()
            })
            .map(|st| st.success())
            .unwrap_or(false);
        assert!(ok, "git apply --cached must succeed for:\n{patch}");
        // The staged blob holds the FIRST change only: X applied, c intact.
        let staged = std::process::Command::new("git")
            .args(["-C", root.to_str().unwrap(), "show", ":f.txt"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&staged.stdout),
            "a\nX\nc\nd\n",
            "only the selected line may reach the index"
        );
    }

    #[test]
    fn selected_lines_patch_keeps_unselected_changes_out_of_the_index() {
        // HEAD: a b c d ; work: a X Y d  -> two independent changes.
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d"]),
            lines(&["a", "X", "Y", "d"]),
        );
        // Select just the first changed row (b -> X).
        let patch = d
            .selected_lines_patch("f.txt", (1, 1))
            .expect("the selection covers a change");
        assert_eq!(
            patch, "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n-b\n+X\n c\n d\n",
            "only the selected replacement is staged; the unselected c->Y \
             leaves c as context and drops Y"
        );
    }

    #[test]
    fn selected_lines_patch_stages_one_added_line_of_several() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b"]),
            lines(&["a", "x", "y", "b"]),
        );
        // Rows: 0 Equal(a), 1 Added(x), 2 Added(y), 3 Equal(b).
        let patch = d
            .selected_lines_patch("f.txt", (1, 1))
            .expect("the selection covers the first addition");
        assert_eq!(
            patch, "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,3 @@\n a\n+x\n b\n",
            "the unselected second addition must not reach the index"
        );
    }

    #[test]
    fn selected_lines_patch_stages_one_deletion_of_several() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d"]),
            lines(&["a", "d"]),
        );
        // Rows: 0 Equal(a), 1 Removed(b), 2 Removed(c), 3 Equal(d).
        let patch = d
            .selected_lines_patch("f.txt", (1, 1))
            .expect("the selection covers the first deletion");
        assert_eq!(
            patch, "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,3 @@\n a\n-b\n c\n d\n",
            "the unselected deletion stays in the index as context"
        );
    }

    #[test]
    fn selected_lines_patch_is_none_without_a_change_in_range() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c"]),
            lines(&["a", "B", "c"]),
        );
        // Row 0 is context only.
        assert!(d.selected_lines_patch("f.txt", (0, 0)).is_none());
    }

    #[test]
    fn hunk_patch_contains_only_the_requested_hunk() {
        // 15 untouched lines between the two edits so their context
        // windows cannot overlap.
        let left: Vec<String> = (1..=20).map(|i| format!("line{i}")).collect();
        let mut right = left.clone();
        right[1] = "LINE2".into();
        right[17] = "LINE18".into();
        let d = DiffData::build(PathBuf::new(), PathBuf::new(), left, right);
        let range = d.hunk_range_at(1).unwrap();
        assert_eq!(range, (1, 1));
        let patch = d.hunk_patch("src/x.rs", range);
        assert_eq!(
            patch,
            "--- a/src/x.rs\n+++ b/src/x.rs\n@@ -1,5 +1,5 @@\n line1\n-line2\n+LINE2\n line3\n line4\n line5\n",
            "the patch must carry only the first hunk with three context lines"
        );
    }

    #[test]
    fn hunk_patch_handles_a_pure_addition() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b"]),
            lines(&["a", "x", "b"]),
        );
        let range = d.hunk_range_at(0).unwrap();
        let patch = d.hunk_patch("f.txt", range);
        assert_eq!(
            patch,
            "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,3 @@\n a\n+x\n b\n"
        );
    }

    #[test]
    fn hunk_patch_handles_a_deletion_at_end_of_file() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c"]),
            lines(&["a", "b"]),
        );
        let range = d.hunk_range_at(0).unwrap();
        let patch = d.hunk_patch("f.txt", range);
        assert_eq!(
            patch,
            "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,2 @@\n a\n b\n-c\n"
        );
    }

    #[test]
    fn next_change_row_jumps_to_the_next_hunk_after_the_cursor() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d", "e"]),
            lines(&["a", "B", "c", "d", "E"]),
        );
        assert_eq!(d.next_change_row(0), Some(1));
        assert_eq!(
            d.next_change_row(1),
            Some(4),
            "must skip past the current hunk"
        );
        assert_eq!(d.next_change_row(4), None);
    }

    #[test]
    fn prev_change_row_jumps_to_the_previous_hunk_before_the_cursor() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d", "e"]),
            lines(&["a", "B", "c", "d", "E"]),
        );
        assert_eq!(d.prev_change_row(4), Some(1));
        assert_eq!(d.prev_change_row(1), None);
        assert_eq!(d.prev_change_row(0), None);
    }

    #[test]
    fn next_change_row_wrap_loops_from_last_hunk_back_to_first() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d", "e"]),
            lines(&["a", "B", "c", "d", "E"]),
        );
        assert_eq!(d.next_change_row_wrap(0), Some(1));
        assert_eq!(d.next_change_row_wrap(1), Some(4));
        // Past the last hunk: wrap to the first instead of stalling.
        assert_eq!(
            d.next_change_row_wrap(4),
            Some(1),
            "at/past the last hunk the next ⟶ click must loop back to the first"
        );
        assert_eq!(d.next_change_row_wrap(999), Some(1));
    }

    #[test]
    fn prev_change_row_wrap_loops_from_first_hunk_back_to_last() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d", "e"]),
            lines(&["a", "B", "c", "d", "E"]),
        );
        assert_eq!(d.prev_change_row_wrap(4), Some(1));
        assert_eq!(
            d.prev_change_row_wrap(1),
            Some(4),
            "at/before the first hunk the previous ⟵ click must loop to the last"
        );
        assert_eq!(d.prev_change_row_wrap(0), Some(4));
    }

    #[test]
    fn change_row_wrap_returns_none_when_diff_has_no_changes() {
        let d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c"]),
            lines(&["a", "b", "c"]),
        );
        assert_eq!(d.next_change_row_wrap(0), None);
        assert_eq!(d.prev_change_row_wrap(0), None);
    }

    #[test]
    fn scroll_to_row_parks_two_rows_of_context_above_the_target() {
        let mut d = DiffData::build(
            PathBuf::new(),
            PathBuf::new(),
            lines(&["a", "b", "c", "d", "e"]),
            lines(&["a", "b", "C", "d", "e"]),
        );
        d.scroll_to_row(2);
        assert_eq!(d.scroll, 0, "row 2 with 2-row context lands scroll at 0");
        d.scroll_to_row(7);
        assert_eq!(d.scroll, 5);
    }

    #[test]
    fn build_with_byte_check_flags_trailing_newline_only_difference() {
        // Same lines on both sides but the working copy lost its trailing
        // newline. `.lines()` collapses the difference; the byte check
        // catches it so the renderer can surface a banner.
        let left_text = "alpha\nbeta\n";
        let right_text = "alpha\nbeta";
        let left_lines: Vec<String> = left_text.lines().map(str::to_string).collect();
        let right_lines: Vec<String> = right_text.lines().map(str::to_string).collect();
        let d = DiffData::build_with_byte_check(
            PathBuf::from("/x"),
            PathBuf::from("/x"),
            left_lines,
            right_lines,
            Some(left_text),
            Some(right_text),
        );
        assert!(
            d.bytes_differ_but_lines_equal,
            "trailing-newline difference must set bytes_differ_but_lines_equal so the diff header explains why no red/green band paints"
        );
        assert!(
            d.rows.iter().all(|r| matches!(r, DiffRow::Equal { .. })),
            "the line-level diff is still entirely Equal rows — the flag is what tells the user the file is byte-different"
        );
    }

    #[test]
    fn build_with_byte_check_leaves_flag_false_when_a_real_line_changes() {
        let left_text = "alpha\nbeta\n";
        let right_text = "alpha\nBETA\n";
        let left_lines: Vec<String> = left_text.lines().map(str::to_string).collect();
        let right_lines: Vec<String> = right_text.lines().map(str::to_string).collect();
        let d = DiffData::build_with_byte_check(
            PathBuf::from("/x"),
            PathBuf::from("/x"),
            left_lines,
            right_lines,
            Some(left_text),
            Some(right_text),
        );
        assert!(
            !d.bytes_differ_but_lines_equal,
            "the flag must stay false when the diff has real Replaced rows — otherwise the header would lie that there's no line-level change"
        );
    }

    #[test]
    fn build_unified_deletion_emits_only_removed_rows_and_sets_unified_flag() {
        let d = DiffData::build_unified_deletion(
            PathBuf::from("doomed.rs"),
            "fn main() {\n    println!(\"bye\");\n}\n",
        );
        assert!(
            d.unified,
            "unified flag must be set so the renderer picks the single-column path"
        );
        assert_eq!(
            d.left_lines.len(),
            3,
            "three source lines must produce three rows"
        );
        assert!(d.right_lines.is_empty(), "deletion view has no right side");
        assert_eq!(
            d.rows,
            vec![
                DiffRow::Removed { left: 0 },
                DiffRow::Removed { left: 1 },
                DiffRow::Removed { left: 2 },
            ],
            "every row must be Removed so all lines paint red with a `-` sign"
        );
    }

    #[test]
    fn standard_diff_builders_leave_unified_flag_off() {
        let d = DiffData::build(PathBuf::new(), PathBuf::new(), lines(&["a"]), lines(&["b"]));
        assert!(!d.unified, "side-by-side diffs must stay non-unified");
    }

    #[test]
    fn build_with_byte_check_leaves_flag_false_when_bytes_are_identical() {
        let text = "alpha\nbeta\n";
        let lines_vec: Vec<String> = text.lines().map(str::to_string).collect();
        let d = DiffData::build_with_byte_check(
            PathBuf::from("/x"),
            PathBuf::from("/x"),
            lines_vec.clone(),
            lines_vec,
            Some(text),
            Some(text),
        );
        assert!(!d.bytes_differ_but_lines_equal);
    }

    #[test]
    fn build_side_by_side_from_git_text_pairs_remove_add_into_replaced() {
        // A one-line edit (- old / + new) must collapse into a single
        // Replaced row so the two sides align visually instead of
        // producing a zigzag of Removed-above-Added.
        let raw = "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1,3 +1,3 @@\n keep\n-old\n+new\n";
        let d = DiffData::build_side_by_side_from_git_text(PathBuf::from("staged"), raw);
        assert!(
            !d.unified,
            "side-by-side builder must clear the unified flag so the two-column renderer takes over"
        );
        let tags: Vec<&'static str> = d
            .rows
            .iter()
            .map(|r| match r {
                DiffRow::Equal { .. } => "eq",
                DiffRow::Added { .. } => "add",
                DiffRow::Removed { .. } => "rm",
                DiffRow::Replaced { .. } => "rep",
            })
            .collect();
        // Expected after skipping index/---/+++ noise:
        //   diff --git → Equal (file header)
        //   @@ ...     → Equal (hunk header)
        //    keep      → Equal (context)
        //   -old / +new → paired into Replaced
        //   trailing empty line → Equal
        assert_eq!(
            tags,
            vec!["eq", "eq", "eq", "rep", "eq"],
            "remove+add pair must collapse into one Replaced row; file/hunk headers stay as Equal markers"
        );
        assert!(
            d.left_lines.iter().any(|s| s == "old"),
            "removed body (without leading -) belongs in left_lines"
        );
        assert!(
            d.right_lines.iter().any(|s| s == "new"),
            "added body (without leading +) belongs in right_lines"
        );
    }

    #[test]
    fn build_side_by_side_from_git_text_overflow_runs_emit_extra_removed_or_added() {
        // 3 removed + 1 added → 1 Replaced + 2 Removed (added side runs out).
        let raw = "diff --git a/x b/x\n@@ -1,4 +1,2 @@\n keep\n-a\n-b\n-c\n+A\n other\n";
        let d = DiffData::build_side_by_side_from_git_text(PathBuf::from("staged"), raw);
        let change_tags: Vec<&'static str> = d
            .rows
            .iter()
            .filter_map(|r| match r {
                DiffRow::Replaced { .. } => Some("rep"),
                DiffRow::Removed { .. } => Some("rm"),
                DiffRow::Added { .. } => Some("add"),
                _ => None,
            })
            .collect();
        assert_eq!(
            change_tags,
            vec!["rep", "rm", "rm"],
            "unequal runs pair what they can then overflow with the longer side"
        );
    }

    #[test]
    fn build_side_by_side_from_git_text_skips_index_and_dashdash_headers() {
        let raw =
            "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n";
        let d = DiffData::build_side_by_side_from_git_text(PathBuf::from("staged"), raw);
        // No row may carry "index ", "--- a/x", or "+++ b/x" in either side.
        assert!(
            !d.left_lines.iter().any(|s| s.starts_with("index ")
                || s.starts_with("--- a/")
                || s.starts_with("+++ b/")),
            "index / --- / +++ noise must be skipped from left_lines: {:?}",
            d.left_lines
        );
        assert!(
            !d.right_lines.iter().any(|s| s.starts_with("index ")
                || s.starts_with("--- a/")
                || s.starts_with("+++ b/")),
            "index / --- / +++ noise must be skipped from right_lines: {:?}",
            d.right_lines
        );
    }

    #[test]
    fn build_side_by_side_from_git_text_empty_input_paints_placeholder_row() {
        let d = DiffData::build_side_by_side_from_git_text(PathBuf::from("staged"), "");
        assert_eq!(d.rows.len(), 1);
        assert!(matches!(d.rows[0], DiffRow::Equal { .. }));
        assert_eq!(d.left_lines, vec!["(no changes)".to_string()]);
        assert!(!d.unified);
    }

    #[test]
    fn selection_text_returns_substring_of_left_column_for_single_row_selection() {
        let mut d = DiffData::build(
            PathBuf::from("x"),
            PathBuf::from("y"),
            lines(&["alpha", "beta", "gamma"]),
            lines(&["alpha", "BETA", "gamma"]),
        );
        // Row 1 is Replaced { left: 1, right: 1 }.
        d.start_selection(DiffSide::Left, 1, 0);
        d.extend_selection_to(1, 4);
        assert_eq!(
            d.selection_text().as_deref(),
            Some("beta"),
            "selecting cols 0..4 on the left column of a Replaced row must return the left line's first 4 chars"
        );
    }

    #[test]
    fn selection_text_returns_right_column_lines_across_multiple_rows() {
        let mut d = DiffData::build(
            PathBuf::from("x"),
            PathBuf::from("y"),
            lines(&["alpha", "beta", "gamma"]),
            lines(&["alpha", "BETA", "GAMMA"]),
        );
        d.start_selection(DiffSide::Right, 0, 0);
        d.extend_selection_to(2, 5);
        assert_eq!(
            d.selection_text().as_deref(),
            Some("alpha\nBETA\nGAMMA"),
            "multi-row right-column selection must concatenate the right_lines with newlines"
        );
    }

    #[test]
    fn selection_text_skips_added_row_content_when_left_side_is_selected() {
        let mut d = DiffData::build(
            PathBuf::from("x"),
            PathBuf::from("y"),
            lines(&["alpha", "gamma"]),
            lines(&["alpha", "delta", "gamma"]),
        );
        let added_row_idx = d
            .rows
            .iter()
            .position(|r| matches!(r, DiffRow::Added { .. }))
            .expect("test fixture must contain an Added row");
        d.start_selection(DiffSide::Left, added_row_idx, 0);
        d.extend_selection_to(added_row_idx, 999);
        assert!(
            matches!(d.selection_text().as_deref(), None | Some("")),
            "left-side selection over an Added row must yield no text — the left column is blank there"
        );
    }

    #[test]
    fn select_word_at_snaps_to_left_column_word_boundaries() {
        let mut d = DiffData::build(
            PathBuf::from("x"),
            PathBuf::from("y"),
            lines(&["alpha bravo charlie", "second"]),
            lines(&["alpha BRAVO charlie", "second"]),
        );
        // Click inside "bravo" on the left side, row 0.
        d.select_word_at(DiffSide::Left, 0, 8);
        let sel = d.selection.expect("word click must produce a selection");
        assert_eq!(sel.side, DiffSide::Left);
        assert_eq!(sel.anchor_row, 0);
        assert_eq!(sel.head_row, 0);
        assert_eq!(sel.anchor_col, 6);
        assert_eq!(sel.head_col, 11);
        assert_eq!(
            d.selection_text().as_deref(),
            Some("bravo"),
            "word selection must copy exactly the clicked word"
        );
    }

    #[test]
    fn select_word_at_on_punctuation_clears_selection() {
        let mut d = DiffData::build(
            PathBuf::from("x"),
            PathBuf::from("y"),
            lines(&["foo, bar"]),
            lines(&["foo, bar"]),
        );
        d.select_word_at(DiffSide::Left, 0, 3);
        assert!(
            d.selection.is_none(),
            "click on a non-word char (',') must NOT anchor a selection"
        );
    }

    #[test]
    fn select_word_at_on_added_row_left_side_clears() {
        let mut d = DiffData::build(
            PathBuf::from("x"),
            PathBuf::from("y"),
            lines(&["alpha", "gamma"]),
            lines(&["alpha", "BETA", "gamma"]),
        );
        let added_row_idx = d
            .rows
            .iter()
            .position(|r| matches!(r, DiffRow::Added { .. }))
            .expect("fixture must contain an Added row");
        d.select_word_at(DiffSide::Left, added_row_idx, 0);
        assert!(
            d.selection.is_none(),
            "double-clicking on the (blank) left side of an Added row must NOT anchor a phantom selection"
        );
    }

    #[test]
    fn selection_occurrence_needle_returns_single_row_word() {
        let mut d = DiffData::build(
            PathBuf::from("x"),
            PathBuf::from("y"),
            lines(&["alpha beta", "alpha gamma"]),
            lines(&["alpha beta", "alpha gamma"]),
        );
        d.select_word_at(DiffSide::Left, 0, 0);
        assert_eq!(
            d.selection_occurrence_needle().as_deref(),
            Some("alpha"),
            "a single-row word selection must expose its text as the occurrence needle"
        );
    }

    #[test]
    fn selection_occurrence_needle_is_none_for_zero_area_or_multi_row() {
        let mut d = DiffData::build(
            PathBuf::from("x"),
            PathBuf::from("y"),
            lines(&["alpha", "beta"]),
            lines(&["alpha", "beta"]),
        );
        // Zero-area click: no occurrence highlight.
        d.start_selection(DiffSide::Left, 0, 2);
        assert_eq!(
            d.selection_occurrence_needle(),
            None,
            "a click is not a word"
        );
        // Multi-row selection: VS Code only highlights single-line selections.
        d.start_selection(DiffSide::Left, 0, 0);
        d.extend_selection_to(1, 2);
        assert_eq!(
            d.selection_occurrence_needle(),
            None,
            "a multi-row selection must not drive occurrence highlighting"
        );
    }

    #[test]
    fn selection_occurrence_needle_skips_whitespace_only_selection() {
        let mut d = DiffData::build(
            PathBuf::from("x"),
            PathBuf::from("y"),
            lines(&["a   b"]),
            lines(&["a   b"]),
        );
        // Select the run of spaces between the two words.
        d.start_selection(DiffSide::Left, 0, 1);
        d.extend_selection_to(0, 4);
        assert_eq!(
            d.selection_occurrence_needle(),
            None,
            "a whitespace-only selection must not highlight occurrences"
        );
    }

    #[test]
    fn zero_area_selection_yields_no_text() {
        let mut d = DiffData::build(
            PathBuf::from("x"),
            PathBuf::from("y"),
            lines(&["alpha"]),
            lines(&["alpha"]),
        );
        d.start_selection(DiffSide::Left, 0, 2);
        assert!(
            d.selection_text().is_none(),
            "a click without a drag must not produce copyable text"
        );
    }
}
