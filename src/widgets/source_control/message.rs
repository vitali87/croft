//! The commit message box: its placeholder and key hint, typing and moving the caret across
//! the subject and body lines, git's prepared message, clearing it after a commit, and where
//! its caret is on screen.

use super::*;

/// Most message rows the box shows before it scrolls.
pub const MESSAGE_MAX_ROWS: usize = 6;

/// Change-list rows a growing commit message never takes over.
pub(super) const MESSAGE_LIST_MIN_ROWS: u16 = 2;

/// The empty commit field's placeholder on Croft Loch: one word, so it is
/// whole in the narrowest side bar.
pub(super) const COMMIT_PLACEHOLDER: &str = "Message";

/// The key hints Croft Loch's empty commit field can show at its right edge,
/// longest first.
pub(super) const COMMIT_PLACEHOLDER_HINTS: [&str; 2] = [
    "\u{23ce} commit \u{b7} \u{21e7}\u{23ce} newline",
    "\u{23ce} commit",
];

/// The longest key hint that fits beside [`COMMIT_PLACEHOLDER`] in a field
/// with `text_max` cells of text, at least three cells clear of it, or
/// `None` when even the shortest would crowd it: a hint is shown whole or
/// not at all, never cut mid-word the way one long placeholder was.
pub(super) fn commit_placeholder_hint(text_max: usize) -> Option<&'static str> {
    let room = text_max.checked_sub(COMMIT_PLACEHOLDER.len() + 3)?;
    COMMIT_PLACEHOLDER_HINTS
        .into_iter()
        .find(|hint| unicode_width::UnicodeWidthStr::width(*hint) <= room)
}

impl SourceControlPanel {
    /// Insert at the caret. A newline starts a new message line (the
    /// commit body); a carriage return counts as one.
    pub fn insert_char(&mut self, c: char) {
        let c = if c == '\r' { '\n' } else { c };
        let idx = self.byte_index_at_cursor();
        self.message.insert(idx, c);
        self.message_cursor += 1;
    }

    /// Insert pasted text at the caret, keeping its line breaks: CRLF and
    /// a lone CR both become one newline, so words on adjacent lines never
    /// run together.
    pub fn insert_str(&mut self, s: &str) {
        for c in s.replace("\r\n", "\n").chars() {
            self.insert_char(c);
        }
    }

    /// The caret's (line, column) within the message, in chars.
    pub(super) fn caret_line_col(&self) -> (usize, usize) {
        let before = self.message.chars().take(self.message_cursor);
        let (mut line, mut col) = (0, 0);
        for c in before {
            if c == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    /// Move the caret to `col` (clamped to the line's end) on message line
    /// `line`.
    pub(super) fn set_caret_line_col(&mut self, line: usize, col: usize) {
        let mut offset = 0;
        for (n, text) in self.message.split('\n').enumerate() {
            let len = text.chars().count();
            if n == line {
                self.message_cursor = offset + col.min(len);
                return;
            }
            offset += len + 1;
        }
    }

    /// Caret to the same column on the line above. False on the first
    /// line, where there is nowhere to go.
    pub fn move_cursor_up(&mut self) -> bool {
        let (line, col) = self.caret_line_col();
        if line == 0 {
            return false;
        }
        self.set_caret_line_col(line - 1, col);
        true
    }

    /// Caret to the same column on the line below. False on the last line.
    pub fn move_cursor_down(&mut self) -> bool {
        let (line, col) = self.caret_line_col();
        if line + 1 >= self.message.split('\n').count() {
            return false;
        }
        self.set_caret_line_col(line + 1, col);
        true
    }

    pub fn backspace(&mut self) {
        if self.message_cursor == 0 {
            return;
        }
        let target = self.message_cursor - 1;
        let mut iter = self.message.char_indices();
        let mut prev_byte = 0;
        for (n, (b, _)) in iter.by_ref().enumerate() {
            if n == target {
                prev_byte = b;
                break;
            }
        }
        let next_byte = iter
            .next()
            .map(|(b, _)| b)
            .unwrap_or_else(|| self.message.len());
        self.message.replace_range(prev_byte..next_byte, "");
        self.message_cursor = target;
    }

    pub fn delete_char(&mut self) {
        if self.message_cursor >= self.message.chars().count() {
            return;
        }
        let at = self.byte_index_at_cursor();
        let next = self.message[at..]
            .char_indices()
            .nth(1)
            .map(|(b, _)| at + b)
            .unwrap_or_else(|| self.message.len());
        self.message.replace_range(at..next, "");
    }

    pub fn move_cursor_left(&mut self) {
        if self.message_cursor > 0 {
            self.message_cursor -= 1;
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.message_cursor < self.message.chars().count() {
            self.message_cursor += 1;
        }
    }

    /// Caret to the start of its line (#1334): a message has a subject and
    /// a body, and Home works within the line as in the editor.
    pub fn home(&mut self) {
        let (line, _) = self.caret_line_col();
        self.set_caret_line_col(line, 0);
    }

    /// Caret to the end of its line (#1334).
    pub fn end(&mut self) {
        let (line, _) = self.caret_line_col();
        self.set_caret_line_col(line, usize::MAX);
    }

    /// Caret to the start of the whole message (Ctrl+Home).
    pub fn message_start(&mut self) {
        self.message_cursor = 0;
    }

    /// Caret to the end of the whole message (Ctrl+End).
    pub fn message_end(&mut self) {
        self.message_cursor = self.message.chars().count();
    }

    /// Fill an empty box with git's prepared message (#1282), so finishing
    /// a merge commits `Merge branch 'other'` rather than "Empty commit
    /// message", or with the `commit.template` file when no merge is in
    /// progress (#1620). The whole message, body included (#1336): a revert's
    /// "This reverts commit …" and a cherry-pick's "(cherry picked from
    /// commit …)" are what tie the commit to the one it came from. Text the
    /// user typed, or edited from the seed, is never replaced; a seed the
    /// user cleared is not put back; and an untouched seed goes once the
    /// merge is committed or aborted.
    pub fn seed_prepared_message(&mut self) {
        let prepared = self
            .status
            .prepared_message
            .as_deref()
            .map(|m| m.trim().to_string());
        match prepared {
            Some(prepared) => {
                // An untouched seed gives way to a different prepared
                // message: switching repositories, or a changed template,
                // must not leave the other one's text to be committed.
                if let Some(seed) = self.seeded_message.as_deref()
                    && seed != prepared
                    && self.message == seed
                {
                    self.clear_message();
                    self.seeded_message = None;
                }
                if self.message.is_empty()
                    && !prepared.is_empty()
                    && self.seeded_message.as_deref() != Some(prepared.as_str())
                {
                    self.insert_str(&prepared);
                    self.seeded_message = Some(prepared);
                }
            }
            None => {
                if let Some(seed) = self.seeded_message.take()
                    && self.message == seed
                {
                    self.clear_message();
                }
            }
        }
    }

    /// Empty the box after a commit made from it, and forget the seed it
    /// held, so the next refresh fills it from `commit.template` again
    /// (#1620): every commit starts from the team's template, not just the
    /// first.
    pub fn clear_after_commit(&mut self) {
        self.clear_message();
        self.seeded_message = None;
    }

    pub fn clear_message(&mut self) {
        self.message.clear();
        self.message_cursor = 0;
        self.message_scroll = 0;
        self.message_line_scroll = 0;
    }

    pub fn click_input(&self, x: u16, y: u16) -> bool {
        let rect = self.last_input_area;
        rect.width > 0
            && rect.height > 0
            && x >= rect.x
            && x < rect.x + rect.width
            && y >= rect.y
            && y < rect.y + rect.height
    }

    pub(super) fn byte_index_at_cursor(&self) -> usize {
        self.message
            .char_indices()
            .nth(self.message_cursor)
            .map(|(b, _)| b)
            .unwrap_or_else(|| self.message.len())
    }

    /// Absolute (col, row) of the commit-message caret in screen
    /// coordinates, matching the pattern `Editor::cursor_screen_pos` uses
    /// to drive the host terminal's hardware caret. Returns `None` when
    /// the message box isn't focused, isn't in a repo (no input box painted),
    /// hasn't been laid out yet, or the cursor would land outside the
    /// visible inner width of the input box. The render path paints
    /// message line `n` at `input_inner.x + 1` on `input_inner.y + n`
    /// (less the scrolled-off lines and columns), so the caret sits at
    /// column `input_box.x + 2 + col`, row `input_box.y + 1 + line`
    /// (rounded border occupies one cell on each side).
    pub fn cursor_screen_pos(&self) -> Option<(u16, u16)> {
        if !self.focused || self.list_focused {
            return None;
        }
        let r = self.last_input_area;
        if r.width < 3 || r.height < 3 {
            return None;
        }
        let (line, col) = self.caret_line_col();
        let cursor_col = col.saturating_sub(self.message_scroll) as u16;
        let max_col = r.width.saturating_sub(3);
        if cursor_col > max_col {
            return None;
        }
        let row = line.checked_sub(self.message_line_scroll)? as u16;
        if row + 2 >= r.height {
            return None;
        }
        Some((r.x + 2 + cursor_col, r.y + 1 + row))
    }
}
