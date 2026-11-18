use crate::git::{ChangeEntry, ChangeKind, ChangeSection, GitStatus};
use crate::icons;
use crate::widgets::scrollbar;
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Widget, Wrap},
};
use std::collections::BTreeSet;

mod commit_menu;
mod palette;
mod repositories;
pub use commit_menu::CommitMenuItem;
use palette::*;
pub use repositories::RepoRow;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowActionAreas {
    pub entry_idx: usize,
    /// Discard / stage are painted on a selected *unstaged* row; `unstage`
    /// on a selected *staged* row. The irrelevant rects are left default
    /// (zero-size, so they never hit-test) for the row's section.
    pub discard: Rect,
    pub stage: Rect,
    pub unstage: Rect,
}

/// Most message rows the box shows before it scrolls.
pub const MESSAGE_MAX_ROWS: usize = 6;

/// Change-list rows a growing commit message never takes over.
const MESSAGE_LIST_MIN_ROWS: u16 = 2;

/// The empty commit field's placeholder on Croft Loch: one word, so it is
/// whole in the narrowest side bar.
const COMMIT_PLACEHOLDER: &str = "Message";

/// The key hints Croft Loch's empty commit field can show at its right edge,
/// longest first.
const COMMIT_PLACEHOLDER_HINTS: [&str; 2] = [
    "\u{23ce} commit \u{b7} \u{21e7}\u{23ce} newline",
    "\u{23ce} commit",
];

/// The longest key hint that fits beside [`COMMIT_PLACEHOLDER`] in a field
/// with `text_max` cells of text, at least three cells clear of it, or
/// `None` when even the shortest would crowd it: a hint is shown whole or
/// not at all, never cut mid-word the way one long placeholder was.
fn commit_placeholder_hint(text_max: usize) -> Option<&'static str> {
    let room = text_max.checked_sub(COMMIT_PLACEHOLDER.len() + 3)?;
    COMMIT_PLACEHOLDER_HINTS
        .into_iter()
        .find(|hint| unicode_width::UnicodeWidthStr::width(*hint) <= room)
}

pub struct SourceControlPanel {
    pub focused: bool,
    /// True under the Black theme: overpaint the focused outer border with the
    /// orange→green brand gradient instead of the legacy solid blue. Set by the
    /// app's focus/theme sync.
    pub focus_gradient: bool,
    /// Active color theme; drives the scrollbar track/thumb colors. Set by the
    /// app's theme sync.
    pub theme: crate::theme::Theme,
    pub message: String,
    pub message_cursor: usize,
    /// Leading characters scrolled out of view on the left of the
    /// commit-message input so the caret stays visible when the message is
    /// wider than the box. Recomputed every render from the cursor and the
    /// box width.
    pub message_scroll: usize,
    /// Message lines scrolled out of view above the commit-message input
    /// once the message has more lines than the box shows. Recomputed every
    /// render, like `message_scroll`, to keep the caret's line visible.
    pub message_line_scroll: usize,
    pub status: GitStatus,
    /// The prepared merge message last put in the box (#1282), whole
    /// (#1336), to tell an untouched seed from the user's own text.
    seeded_message: Option<String>,
    pub entries: Vec<ChangeEntry>,
    /// The multi-root repositories overview (#161), pushed by the App per
    /// frame from its per-folder git workers. Length below two hides the
    /// section (single-folder panels render exactly as before).
    pub repositories: Vec<RepoRow>,
    /// Frame-truth hit rects for the overview rows (cleared and
    /// re-recorded every render, #103): click switches the panel to that
    /// repository.
    pub repo_row_areas: Vec<(Rect, std::path::PathBuf)>,
    pub last_area: Rect,
    pub last_inner: Rect,
    pub last_input_area: Rect,
    pub last_button_area: Rect,
    /// Hit-test rect for the chevron split-button that opens the Commit
    /// dropdown (Commit & Push, Push, View …). Empty when the panel is in
    /// the no-repo state or the button row was clipped.
    pub last_commit_caret_area: Rect,
    /// Hit-test rects for the dropdown items (Commit & Push, Push, …)
    /// that pop below the caret. Empty when the menu is closed. The App
    /// owns the open/closed flag (`commit_menu_open`) so menu state
    /// survives re-renders.
    pub commit_menu_item_areas: Vec<(Rect, CommitMenuItem)>,
    pub last_list_area: Rect,
    pub last_scrollbar: Rect,
    /// Hit-test rect for the empty-state "Initialize Repository" button.
    /// Empty when the panel is in a git repo or when the panel is too
    /// small to draw the empty-state card.
    pub last_init_repo_button_area: Rect,
    /// Repositories in the workspace's immediate subfolders, as (label,
    /// absolute path), set by `App` while the workspace itself is in no
    /// repo (#1227). Non-empty, the empty-state card lists them with an
    /// Open button each in place of Initialize.
    pub nested_repos: Vec<(String, std::path::PathBuf)>,
    /// Hit-test rects for those Open buttons; empty whenever none paint.
    pub nested_repo_button_areas: Vec<(Rect, std::path::PathBuf)>,
    /// Cell rect reserved for the no-repo empty-state hero illustration.
    /// `App` paints an OSC-1337 inline PNG here on iTerm2-class terminals;
    /// the widget paints an ASCII Y-fork as fallback so the same rect
    /// reads as a logo on plain terminals too. Empty when the panel is
    /// in a git repo or the empty-state card is too small to allocate.
    pub last_hero_area: Rect,
    /// Set by `App` after probing the host terminal for OSC-1337 inline
    /// image support. When true, the empty-state render leaves the hero
    /// rect blank so the PNG owns those cells without competing with an
    /// ASCII fallback that would briefly flash through during resize.
    pub inline_hero_image_active: bool,
    pub scroll: usize,
    /// Index into `entries` of the currently-selected change, if any.
    /// Drives the row-highlight bg so the user can tell at a glance
    /// which entry the editor is showing the diff for.
    pub selected_change: Option<usize>,
    /// Indices of all rows currently selected (multi-select). Always
    /// includes `selected_change` when that is `Some`. Cmd+A populates this
    /// with every entry; a plain click resets it to the single clicked row.
    /// The Stage shortcut (Cmd+S) acts on this set; click on an action icon
    /// still acts on just that row.
    pub multi_selection: BTreeSet<usize>,
    /// The change list holds the keyboard instead of the message box
    /// (#871), as in VS Code: arrows move `selected_change` and Enter opens
    /// its diff. Tab, or Down on the message's last line, moves here; Tab,
    /// or Up on the first row, goes back.
    pub list_focused: bool,
    /// Status / error line painted below the button after a commit attempt.
    /// Cleared on the next refresh.
    pub commit_feedback: Option<String>,
    pub commit_feedback_is_error: bool,
    /// Hit-test rects for the inline action icons (Discard, Stage) painted
    /// on the selected unstaged row. `None` when no row is selected or the
    /// selected row is already staged. Cleared at the top of each render
    /// and re-populated for the visible selected row.
    pub last_row_actions: Option<RowActionAreas>,
    /// Render-time pointer cell, fed from `App::pointer_cell` each frame so a
    /// hovered (but unselected) change row can lift to the hover background.
    pub hover_pointer: Option<(u16, u16)>,
    /// Hit-test rect for the refresh pill at the right of the SOURCE CONTROL
    /// header row, mirroring the Remote Explorer refresh affordance. Clicking
    /// it forces an immediate, un-debounced git re-scan. Empty when the panel
    /// is in the no-repo state or the header row was clipped.
    pub header_refresh_btn: Rect,
    /// Hit-test rect for the branch name on the branch row. Clicking it
    /// opens the Checkout / Create Branch picker, mirroring VS Code's
    /// clickable status-bar branch indicator. Empty in the no-repo state.
    pub last_branch_area: Rect,
    /// Hit-test rect for the "⋯" actions pill in the header (left of the
    /// refresh pill). Opens the full Source Control actions menu. Empty in
    /// the no-repo state or when the header row is too narrow.
    pub header_more_btn: Rect,
}

impl SourceControlPanel {
    pub fn new() -> Self {
        Self {
            focused: false,
            focus_gradient: false,
            theme: crate::theme::Theme::default(),
            message: String::new(),
            message_cursor: 0,
            message_scroll: 0,
            message_line_scroll: 0,
            status: GitStatus::default(),
            seeded_message: None,
            entries: Vec::new(),
            repositories: Vec::new(),
            repo_row_areas: Vec::new(),
            last_area: Rect::default(),
            last_inner: Rect::default(),
            last_input_area: Rect::default(),
            last_button_area: Rect::default(),
            last_commit_caret_area: Rect::default(),
            commit_menu_item_areas: Vec::new(),
            last_list_area: Rect::default(),
            last_scrollbar: Rect::default(),
            last_init_repo_button_area: Rect::default(),
            nested_repos: Vec::new(),
            nested_repo_button_areas: Vec::new(),
            last_hero_area: Rect::default(),
            inline_hero_image_active: false,
            scroll: 0,
            selected_change: None,
            multi_selection: BTreeSet::new(),
            list_focused: false,
            commit_feedback: None,
            commit_feedback_is_error: false,
            last_row_actions: None,
            hover_pointer: None,
            header_refresh_btn: Rect::default(),
            last_branch_area: Rect::default(),
            header_more_btn: Rect::default(),
        }
    }

    /// True when (x, y) lands on the "⋯" header pill, so the App can open
    /// the Source Control actions menu.
    pub fn click_more(&self, x: u16, y: u16) -> bool {
        rect_hit(self.header_more_btn, x, y)
    }

    /// Returns the entry index when (x, y) lands inside the Discard icon
    /// of the currently-selected row. The widget only paints this icon on
    /// the selected unstaged row, so a hit implies that row is the action
    /// target.
    pub fn click_discard_action(&self, x: u16, y: u16) -> Option<usize> {
        let a = self.last_row_actions?;
        rect_hit(a.discard, x, y).then_some(a.entry_idx)
    }

    /// Returns the entry index when (x, y) lands inside the Stage icon of
    /// the currently-selected row. Mirrors `click_discard_action`.
    pub fn click_stage_action(&self, x: u16, y: u16) -> Option<usize> {
        let a = self.last_row_actions?;
        rect_hit(a.stage, x, y).then_some(a.entry_idx)
    }

    /// Returns the entry index when (x, y) lands inside the Unstage icon
    /// of the currently-selected *staged* row. The widget only paints this
    /// icon on a selected staged row, so a hit implies that row.
    pub fn click_unstage_action(&self, x: u16, y: u16) -> Option<usize> {
        let a = self.last_row_actions?;
        rect_hit(a.unstage, x, y).then_some(a.entry_idx)
    }

    /// True when (x, y) lands on the branch name, so the App can open the
    /// Checkout / Create Branch picker.
    pub fn click_branch(&self, x: u16, y: u16) -> bool {
        rect_hit(self.last_branch_area, x, y)
    }

    /// True when (x, y) lands on the header refresh pill, so the App can
    /// force an immediate git re-scan.
    pub fn click_header_refresh(&self, x: u16, y: u16) -> bool {
        rect_hit(self.header_refresh_btn, x, y)
    }

    pub fn set_status(&mut self, status: GitStatus, entries: Vec<ChangeEntry>) {
        self.status = status;
        self.seed_prepared_message();
        let row_of = |i: usize| {
            self.entries
                .get(i)
                .map(|e| (e.path.clone(), e.kind.section()))
        };
        let selected = self.selected_change.and_then(row_of);
        let multi: Vec<_> = self
            .multi_selection
            .iter()
            .filter_map(|&i| row_of(i))
            .collect();
        self.entries = entries;
        // A change set refresh can re-order entries (staging an untracked
        // file moves it up): the selection follows its file, so the next
        // key acts on the row the user picked (#871), and on a partly
        // staged file's row in the same section while it has one. A file
        // that left the list hands its slot to the row that took its
        // place, and a slot past the end clears.
        let rows = &self.entries;
        let follow = |(path, section): &(String, crate::git::ChangeSection)| {
            let same_row = |e: &ChangeEntry| &e.path == path && e.kind.section() == *section;
            rows.iter()
                .position(same_row)
                .or_else(|| rows.iter().position(|e| &e.path == path))
        };
        // The multi-selection follows its files the same way, so Ctrl+S
        // stages what is highlighted; a file that left the list leaves it.
        let multi_selection = multi.iter().filter_map(follow).collect();
        if let Some(idx) = selected.as_ref().and_then(follow) {
            self.selected_change = Some(idx);
        } else if let Some(idx) = self.selected_change
            && idx >= self.entries.len()
        {
            self.selected_change = None;
        }
        self.multi_selection = multi_selection;
        if self.entries.is_empty() {
            self.list_focused = false;
        }
    }

    /// Click-to-select: returns the entry index now selected, if the
    /// click landed on an entry row. Resets the multi-selection to just
    /// this row — Cmd+A is the way to opt into multi.
    pub fn select_change_at(&mut self, y: u16) -> Option<usize> {
        let idx = self.entry_at_y(y)?;
        self.selected_change = Some(idx);
        self.multi_selection.clear();
        self.multi_selection.insert(idx);
        Some(idx)
    }

    /// Cmd+A in the SC pane: every entry row is now in `multi_selection`,
    /// so the Stage shortcut acts on all of them at once. Leaves
    /// `selected_change` alone so the diff editor keeps showing the same
    /// file. Returns the new selection count.
    pub fn select_all_changes(&mut self) -> usize {
        self.multi_selection = (0..self.entries.len()).collect();
        if self.selected_change.is_none() && !self.entries.is_empty() {
            self.selected_change = Some(0);
        }
        self.multi_selection.len()
    }

    pub fn changes_count(&self) -> usize {
        self.entries.len()
    }

    /// Hand the keyboard to the change list (#871), selecting its first
    /// row when no row is selected. False, leaving the message box
    /// focused, when the list is empty.
    pub fn focus_list(&mut self) -> bool {
        let Some(&first) = self.entry_order().first() else {
            return false;
        };
        self.list_focused = true;
        if self.selected_change.is_none() {
            self.select_only(first);
        }
        self.reveal_selected();
        true
    }

    /// Move the selection one row up or down the list as it is drawn
    /// (sections first, so not `entries` order), scrolling it into view.
    /// False on Up from the first row, where the list ends.
    pub fn move_selection(&mut self, up: bool) -> bool {
        let order = self.entry_order();
        let pos = self
            .selected_change
            .and_then(|s| order.iter().position(|&i| i == s));
        let next = match pos {
            None => order.first(),
            Some(0) if up => return false,
            Some(p) if up => order.get(p - 1),
            Some(p) => order.get(p + 1),
        };
        if let Some(&idx) = next {
            self.select_only(idx);
            self.reveal_selected();
        }
        true
    }

    /// Entry indices in the order the list draws them.
    fn entry_order(&self) -> Vec<usize> {
        self.list_layout()
            .into_iter()
            .filter_map(|line| match line {
                ListLine::Entry(i) => Some(i),
                ListLine::Header(_) => None,
            })
            .collect()
    }

    fn select_only(&mut self, idx: usize) {
        self.selected_change = Some(idx);
        self.multi_selection.clear();
        self.multi_selection.insert(idx);
    }

    /// Scroll so the selected row is on screen, with its section header
    /// when that is the row above it.
    fn reveal_selected(&mut self) {
        let viewport = self.last_list_area.height as usize;
        let lines = self.list_layout();
        let Some(line) = self
            .selected_change
            .and_then(|s| lines.iter().position(|l| *l == ListLine::Entry(s)))
        else {
            return;
        };
        if viewport == 0 {
            return;
        }
        let top = match line.checked_sub(1).map(|l| lines[l]) {
            Some(ListLine::Header(_)) => line - 1,
            _ => line,
        };
        if top < self.scroll {
            self.scroll = top;
        } else if line >= self.scroll + viewport {
            self.scroll = line + 1 - viewport;
        }
    }

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
    fn caret_line_col(&self) -> (usize, usize) {
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
    fn set_caret_line_col(&mut self, line: usize, col: usize) {
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

    pub fn entry_at_y(&self, y: u16) -> Option<usize> {
        if y < self.last_list_area.y || y >= self.last_list_area.y + self.last_list_area.height {
            return None;
        }
        let row = (y - self.last_list_area.y) as usize;
        // The list interleaves section headers with rows; tag each visual
        // line with whether it points to an entry, then look up by offset.
        let lines = self.list_layout();
        let line_idx = self.scroll + row;
        match lines.get(line_idx)? {
            ListLine::Entry(idx) => Some(*idx),
            ListLine::Header(_) => None,
        }
    }

    pub fn click_button(&self, x: u16, y: u16) -> bool {
        rect_hit(self.last_button_area, x, y)
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

    pub fn click_init_repo_button(&self, x: u16, y: u16) -> bool {
        rect_hit(self.last_init_repo_button_area, x, y)
    }

    /// The subfolder repo whose Open button (x, y) lands on (#1227).
    pub fn click_nested_repo(&self, x: u16, y: u16) -> Option<std::path::PathBuf> {
        self.nested_repo_button_areas
            .iter()
            .find(|(r, _)| rect_hit(*r, x, y))
            .map(|(_, path)| path.clone())
    }

    pub fn scroll_up(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_sub(rows);
    }

    pub fn scroll_down(&mut self, rows: usize) {
        let max = self
            .list_layout()
            .len()
            .saturating_sub(self.last_list_area.height as usize);
        self.scroll = (self.scroll + rows).min(max);
    }

    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let total = self.list_layout().len();
        let viewport = self.last_list_area.height as usize;
        let Some(metrics) =
            scrollbar::vertical_metrics(self.last_scrollbar, total, viewport, self.scroll)
        else {
            return false;
        };
        self.scroll = scrollbar::scroll_for_y(metrics, y);
        true
    }

    fn byte_index_at_cursor(&self) -> usize {
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

    /// Compute the visual layout of the change list as a flat sequence of
    /// rows: section headers and entry rows. Used for hit-testing and
    /// scrollbar metrics.
    fn list_layout(&self) -> Vec<ListLine> {
        let mut out = Vec::new();
        for section in [
            ChangeSection::Conflicts,
            ChangeSection::Staged,
            ChangeSection::Changes,
            ChangeSection::Untracked,
        ] {
            let mut any = false;
            for (i, entry) in self.entries.iter().enumerate() {
                if entry.kind.section() == section {
                    if !any {
                        out.push(ListLine::Header(section));
                        any = true;
                    }
                    out.push(ListLine::Entry(i));
                }
            }
        }
        out
    }

    /// Croft Loch's Commit split button: one pill on the middle row of its
    /// click targets (the rows above and below stay chrome), Commit and its
    /// dropdown caret capped only on their outer ends. The one-cell gap
    /// between the two click targets carries a thin darker seam at its left
    /// edge and belongs to the caret half's face, so it reads as one button
    /// with a second action rather than two blocks, and the chevron centres
    /// in the face right of the seam. Each half lifts on its own under the
    /// pointer; the gap lifts with the caret it is painted as.
    fn render_soft_split_button(&self, buf: &mut Buffer, main: Rect, caret: Rect) {
        use crate::widgets::surface::{CHEVRON_UNFOLDED, paint_soft_button};
        let (fill, fg) = primary_button_colors(self.theme, self.focus_gradient);
        let lift = |r: Rect| {
            if crate::widgets::hover::contains(r, self.hover_pointer) {
                lift_button(fill)
            } else {
                fill
            }
        };
        let (main_fill, caret_fill) = (lift(main), lift(caret));
        let main_face = paint_soft_button(buf, main, main_fill, true, false);
        let caret_face = paint_soft_button(buf, caret, caret_fill, false, true);
        let gap = Rect {
            x: main.right(),
            width: caret.x.saturating_sub(main.right()),
            ..main
        };
        let mut caret_label = caret_face;
        if gap.width > 0 {
            let gap_face = paint_soft_button(buf, gap, caret_fill, false, false);
            if let Color::Rgb(r, g, b) = fill
                && gap_face.width > 0
            {
                let seam = crate::gradient::rgb_color(crate::gradient::lerp_rgb(
                    (r, g, b),
                    (0, 0, 0),
                    0.45,
                ));
                buf[(gap_face.x, gap_face.y)]
                    .set_symbol("\u{258f}")
                    .set_fg(seam);
            }
            if caret_face.width > 0 && gap_face.width > 0 {
                caret_label = Rect {
                    x: gap_face.x,
                    width: gap_face.width + caret_face.width,
                    ..caret_face
                };
            }
        }
        paint_button_label(buf, main_face, "\u{eab2} Commit", main_fill, fg);
        paint_button_label(
            buf,
            caret_label,
            &CHEVRON_UNFOLDED.to_string(),
            caret_fill,
            fg,
        );
    }

    /// Paint the "No repository detected" empty-state card. `inner` is the
    /// panel's inner rect (border already drawn); the SOURCE CONTROL header
    /// sits on `inner.y` so the card starts two rows below.
    fn render_no_repo_empty_state(&mut self, inner: Rect, buf: &mut Buffer) {
        if inner.height < 4 || inner.width < 8 {
            return;
        }
        let card_top = inner.y + 2;
        let card_bottom = inner.y + inner.height - 1;
        let card_h = card_bottom.saturating_sub(card_top);
        if card_h < 3 {
            return;
        }
        let side_pad: u16 = if inner.width >= 24 { 2 } else { 1 };
        let card_x = inner.x + side_pad;
        let card_w = inner.width.saturating_sub(side_pad * 2);
        let card_area = Rect {
            x: card_x,
            y: card_top,
            width: card_w,
            height: card_h,
        };
        let card_border = Style::default().fg(self.theme.ui(Color::Rgb(0x60, 0x68, 0x78)));
        let card_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(card_border);
        let card_inner = card_block.inner(card_area);
        // Croft Loch: a raised card with a rim (it never takes the keyboard,
        // so no accent lip) in place of the rounded line box; same inset.
        let tones = crate::widgets::surface::tones(self.theme);
        match tones {
            Some(t) => {
                buf.set_style(card_area, Style::default().bg(t.raised()));
                crate::widgets::surface::paint_quiet_frame(buf, card_area, self.theme);
            }
            None => card_block.render(card_area, buf),
        }
        if card_inner.width < 4 || card_inner.height < 3 {
            return;
        }

        // Black theme: the brand's teal card accent and button fill replace
        // the legacy blue line art and VS Code button blue; Croft Dark keeps
        // the historical blues.
        let blue = if self.focus_gradient {
            crate::gradient::rgb_color(crate::gradient::CARD_ACCENT)
        } else {
            self.theme.ui(Color::Rgb(0x60, 0x9a, 0xfe))
        };
        let dim_blue = self.theme.ui(Color::Rgb(0x4b, 0x50, 0x5a));
        let text_white = tones.map_or_else(
            || self.theme.ui(Color::Rgb(0xff, 0xff, 0xff)),
            |t| t.strong(),
        );
        let text_dim = tones.map_or_else(
            || self.theme.ui(Color::Rgb(0x9d, 0xa5, 0xb4)),
            |t| t.muted(),
        );
        let blue_bg = if self.focus_gradient || tones.is_some() {
            crate::gradient::rgb_color(crate::gradient::PRIMARY_BTN_BG)
        } else {
            Color::Rgb(BUTTON_BG_RGB.0, BUTTON_BG_RGB.1, BUTTON_BG_RGB.2)
        };

        let mut y = card_inner.y + 2;
        let bottom = card_inner.y + card_inner.height;

        // Illustration: reserve a centred block for the OSC-1337 hero
        // image (App paints the PNG over these cells on iTerm2-class
        // terminals). Paint the ASCII Y-fork as a fallback inside the
        // same rect so non-iTerm2 terminals still see a logo.
        let hero_h: u16 = if card_inner.height >= 18 {
            8
        } else if card_inner.height >= 14 {
            7
        } else {
            5
        };
        let hero_w: u16 = (hero_h * 2).min(card_inner.width).max(7);
        let hero_x = card_inner.x + (card_inner.width.saturating_sub(hero_w)) / 2;
        if y + hero_h <= bottom {
            self.last_hero_area = Rect {
                x: hero_x,
                y,
                width: hero_w,
                height: hero_h,
            };
            // Only paint the ASCII Y-fork when there's NO inline-image
            // hero owning these cells. With OSC-1337 active, leave the
            // rect blank: ratatui's text would briefly flash through the
            // image during a resize, which the user (correctly) called
            // out as old-logo glimpses peeking through the new one.
            if !self.inline_hero_image_active {
                if card_inner.width >= 13 && hero_h >= 7 {
                    paint_y_fork_illustration(buf, card_inner, y, dim_blue, blue);
                } else {
                    paint_y_fork_compact(buf, card_inner, y, blue);
                }
            }
            y += hero_h + 1;
        }

        // Title and description go through ratatui's `Paragraph` with
        // word wrapping so a resized panel reflows the text instead of
        // truncating mid-word - that's the "text inside is not even
        // adjusted" bug from the user's screenshot.
        let nested = !self.nested_repos.is_empty();
        let title_text = if nested {
            "Repositories found in subfolders"
        } else {
            "No repository detected"
        };
        let title_h = paragraph_line_count(title_text, card_inner.width).min(3) as u16;
        if y + title_h <= bottom && title_h > 0 {
            let title_rect = Rect {
                x: card_inner.x,
                y,
                width: card_inner.width,
                height: title_h,
            };
            let title = Paragraph::new(title_text)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(text_white).add_modifier(Modifier::BOLD));
            title.render(title_rect, buf);
            y += title_h + 1;
        }

        let desc_text = if nested {
            "This folder is not a Git repository, but these subfolders are. Open one to use Source Control."
        } else {
            "Open a folder under Git or create a new repository to start tracking changes."
        };
        let desc_h = paragraph_line_count(desc_text, card_inner.width).min(6) as u16;
        if y + desc_h <= bottom && desc_h > 0 {
            let desc_rect = Rect {
                x: card_inner.x,
                y,
                width: card_inner.width,
                height: desc_h,
            };
            let desc = Paragraph::new(desc_text)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(text_dim));
            desc.render(desc_rect, buf);
            y += desc_h + 2;
        }

        // Primary button, centered, width capped so it doesn't sprawl on
        // a wide sidebar.
        let max_btn_w: u16 = 40;
        let btn_w = card_inner.width.saturating_sub(2).min(max_btn_w);
        let btn_x = card_inner.x + (card_inner.width.saturating_sub(btn_w)) / 2;
        if nested {
            // One Open button per repo while they fit; Initialize would
            // `git init` this folder around them, so it is not offered.
            for (label, path) in &self.nested_repos {
                if y + 3 > bottom || btn_w < 6 {
                    break;
                }
                let area = Rect {
                    x: btn_x,
                    y,
                    width: btn_w,
                    height: 3,
                };
                let text = format!("Open {label}");
                let hovered = crate::widgets::hover::contains(area, self.hover_pointer);
                render_button(buf, area, &text, blue_bg, text_white, self.theme, hovered);
                self.nested_repo_button_areas.push((area, path.clone()));
                y += 4;
            }
            return;
        }
        let init_label: &str = if btn_w >= 23 {
            "Initialize Repository"
        } else if btn_w >= 12 {
            "Initialize"
        } else {
            "Init"
        };
        if y + 3 <= bottom && btn_w >= 6 {
            let init_area = Rect {
                x: btn_x,
                y,
                width: btn_w,
                height: 3,
            };
            self.last_init_repo_button_area = init_area;
            let hovered = crate::widgets::hover::contains(init_area, self.hover_pointer);
            render_button(
                buf, init_area, init_label, blue_bg, text_white, self.theme, hovered,
            );
        }
    }
}

/// How many rows ratatui needs to render `text` as a centered, wrapped
/// `Paragraph` at `width`. Uses the same word-wrap rules as ratatui itself
/// (whitespace-separated, no mid-word breaks unless a word exceeds the
/// width). Returns at least 1 unless `width == 0`.
fn paragraph_line_count(text: &str, width: u16) -> usize {
    if width == 0 || text.is_empty() {
        return 0;
    }
    let max = width as usize;
    let mut lines: usize = 1;
    let mut current: usize = 0;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        if current == 0 {
            current = wlen.min(max);
            if wlen > max {
                lines += wlen / max;
                current = wlen % max;
            }
        } else if current + 1 + wlen <= max {
            current += 1 + wlen;
        } else {
            lines += 1;
            current = wlen.min(max);
            if wlen > max {
                lines += wlen / max;
                current = wlen % max;
            }
        }
    }
    lines
}

/// 7-row "elegant" illustration: a Git Y-fork (top node, trunk vertical,
/// branch row with side node, trunk vertical, bottom node) framed by a
/// dashed ring of dots. The classic source-control silhouette - no file
/// outline competing for attention.
fn paint_y_fork_illustration(
    buf: &mut Buffer,
    card_inner: Rect,
    top_y: u16,
    dim_blue: Color,
    blue: Color,
) {
    let dot = Style::default().fg(dim_blue);
    let trunk = Style::default().fg(blue);
    let node = Style::default().fg(blue).add_modifier(Modifier::BOLD);
    let cx = card_inner.x + card_inner.width / 2;

    // Top + bottom dashed arcs framing the tree.
    let arc = "· · · · ·";
    let arc_w = arc.chars().count() as u16;
    let arc_x = card_inner.x + (card_inner.width - arc_w) / 2;
    buf.set_string(arc_x, top_y, arc, dot);
    buf.set_string(arc_x, top_y + 6, arc, dot);

    // Side dots flanking the tree on the 5 inner rows, completing the
    // dashed-ring silhouette of the SVG mockup.
    let side_off: u16 = 5;
    if cx > card_inner.x + side_off {
        buf.set_string(cx - side_off, top_y + 1, "·", dot);
        buf.set_string(cx - side_off, top_y + 3, "·", dot);
        buf.set_string(cx - side_off, top_y + 5, "·", dot);
    }
    if cx + side_off < card_inner.x + card_inner.width {
        buf.set_string(cx + side_off, top_y + 1, "·", dot);
        buf.set_string(cx + side_off, top_y + 3, "·", dot);
        buf.set_string(cx + side_off, top_y + 5, "·", dot);
    }

    // Y-fork tree - three nodes (●) on the trunk column, branch out on
    // the middle row to a side node, trunk verticals between rows.
    //     ●
    //     │
    //     ●─●
    //     │
    //     ●
    buf.set_string(cx, top_y + 1, "●", node);
    buf.set_string(cx, top_y + 2, "│", trunk);
    buf.set_string(cx, top_y + 3, "●", node);
    buf.set_string(cx + 1, top_y + 3, "─", trunk);
    if cx + 2 < card_inner.x + card_inner.width {
        buf.set_string(cx + 2, top_y + 3, "●", node);
    }
    buf.set_string(cx, top_y + 4, "│", trunk);
    buf.set_string(cx, top_y + 5, "●", node);
}

/// 5-row compact fallback for narrow panels: just the Y-fork tree, no
/// surrounding ring. Width: 5 cells.
fn paint_y_fork_compact(buf: &mut Buffer, card_inner: Rect, top_y: u16, blue: Color) {
    let trunk = Style::default().fg(blue);
    let node = Style::default().fg(blue).add_modifier(Modifier::BOLD);
    let cx = card_inner.x + card_inner.width / 2;
    buf.set_string(cx, top_y, "●", node);
    buf.set_string(cx, top_y + 1, "│", trunk);
    buf.set_string(cx, top_y + 2, "●", node);
    buf.set_string(cx + 1, top_y + 2, "─", trunk);
    if cx + 2 < card_inner.x + card_inner.width {
        buf.set_string(cx + 2, top_y + 2, "●", node);
    }
    buf.set_string(cx, top_y + 3, "│", trunk);
    buf.set_string(cx, top_y + 4, "●", node);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListLine {
    Header(ChangeSection),
    Entry(usize),
}

impl Default for SourceControlPanel {
    fn default() -> Self {
        Self::new()
    }
}

fn section_label(section: ChangeSection) -> &'static str {
    match section {
        ChangeSection::Conflicts => "MERGE CONFLICTS",
        ChangeSection::Staged => "STAGED CHANGES",
        ChangeSection::Changes => "CHANGES",
        ChangeSection::Untracked => "UNTRACKED",
    }
}

/// Paint a chunky 3-row solid-bg button at `area`, with rounded corner
/// glyphs that let the panel bg show through the four corner notches so
/// the button reads as a rounded rectangle (matching the VS Code mockup).
fn rect_hit(rect: Rect, x: u16, y: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && x >= rect.x
        && x < rect.x + rect.width
        && y >= rect.y
        && y < rect.y + rect.height
}

/// Shared between the Source Control commit button and the Run-and-Debug
/// button — same style, same corner treatment.
pub fn render_rounded_button(buf: &mut Buffer, area: Rect, label: &str, bg: Color, fg: Color) {
    if area.width < 2 || area.height < 1 {
        return;
    }
    // Fill every cell with the solid bg. The label and the corner glyphs
    // overwrite specific cells below.
    let solid = Style::default().bg(bg);
    for ry in 0..area.height {
        for rx in 0..area.width {
            buf[(area.x + rx, area.y + ry)]
                .set_symbol(" ")
                .set_style(solid);
        }
    }
    // Replace the four corner cells with rounded-border glyphs whose bg is
    // *unset* so the panel bg shows through the outer angle of each curve;
    // the glyph's stroke is drawn in the button bg colour so the curve
    // visually continues the button outline.
    let corner_style = Style::default().fg(bg);
    if area.height >= 2 {
        buf[(area.x, area.y)]
            .set_symbol("╭")
            .set_style(corner_style);
        buf[(area.x + area.width - 1, area.y)]
            .set_symbol("╮")
            .set_style(corner_style);
        buf[(area.x, area.y + area.height - 1)]
            .set_symbol("╰")
            .set_style(corner_style);
        buf[(area.x + area.width - 1, area.y + area.height - 1)]
            .set_symbol("╯")
            .set_style(corner_style);
    }
    // Truncate the label rather than dropping it entirely when the button
    // is narrower than `label.chars().count()`. The earlier silent-drop
    // path produced a blank blue rectangle in narrow side panels — the
    // user-visible "WTF is this" bug from the no-repo empty state.
    let max_w = area.width as usize;
    let visible_label: String = label.chars().take(max_w).collect();
    if visible_label.is_empty() {
        return;
    }
    let label_w = visible_label.chars().count() as u16;
    let label_x = area.x + (area.width - label_w) / 2;
    let label_y = area.y + area.height / 2;
    buf.set_string(
        label_x,
        label_y,
        &visible_label,
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    );
}

/// A primary button's fill and label ink: the brand teal under the Black
/// theme (`focus_gradient`) and always under a `surfaces` theme, whose label
/// takes the strong ink rather than pure white; VS Code's button blue with
/// white on Croft Dark.
pub fn primary_button_colors(theme: crate::theme::Theme, focus_gradient: bool) -> (Color, Color) {
    let teal = crate::gradient::rgb_color(crate::gradient::PRIMARY_BTN_BG);
    match crate::widgets::surface::tones(theme) {
        Some(t) => (teal, t.strong()),
        None => (
            if focus_gradient {
                teal
            } else {
                Color::Rgb(BUTTON_BG_RGB.0, BUTTON_BG_RGB.1, BUTTON_BG_RGB.2)
            },
            Color::Rgb(BUTTON_FG_RGB.0, BUTTON_FG_RGB.1, BUTTON_FG_RGB.2),
        ),
    }
}

/// A button colour lifted for the pointer: a step toward white, the hover a
/// filled button takes in VS Code (`button.hoverBackground`).
fn lift_button(fill: Color) -> Color {
    match fill {
        Color::Rgb(r, g, b) => crate::gradient::rgb_color(crate::gradient::lerp_rgb(
            (r, g, b),
            (0xff, 0xff, 0xff),
            0.12,
        )),
        other => other,
    }
}

/// Write `label` bold and centred on `area`'s middle row, truncated to the
/// button's width rather than dropped (a blank button reads as broken).
fn paint_button_label(buf: &mut Buffer, area: Rect, label: &str, bg: Color, fg: Color) {
    let visible_label: String = label.chars().take(area.width as usize).collect();
    if visible_label.is_empty() {
        return;
    }
    let label_w = visible_label.chars().count() as u16;
    buf.set_string(
        area.x + (area.width - label_w) / 2,
        area.y + area.height / 2,
        &visible_label,
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    );
}

/// The primary button every view draws (Commit, Initialize, Open, Run and
/// Debug). A boxed theme gets [`render_rounded_button`]. Under a `surfaces`
/// theme it is a pill ([`crate::widgets::surface::paint_soft_button`]): one
/// face row with half-block caps on the middle row of the three-row target,
/// the rows above and below left as the surface it sits on, instead of
/// drawing `╭╮╰╯` arcs on the chrome; the label centres between the caps
/// and the pointer lifts the face a step. The click target is the full
/// `area` either way, so the press area stays as generous as the box's.
pub fn render_button(
    buf: &mut Buffer,
    area: Rect,
    label: &str,
    bg: Color,
    fg: Color,
    theme: crate::theme::Theme,
    hovered: bool,
) {
    if crate::widgets::surface::tones(theme).is_none() {
        render_rounded_button(buf, area, label, bg, fg);
        return;
    }
    if area.width < 2 || area.height < 1 {
        return;
    }
    let fill = if hovered { lift_button(bg) } else { bg };
    let face = crate::widgets::surface::paint_soft_button(buf, area, fill, true, true);
    paint_button_label(buf, face, label, fill, fg);
}

fn badge_color(kind: ChangeKind, theme: crate::theme::Theme) -> Color {
    match kind {
        ChangeKind::StagedAdded => theme.ui(Color::Rgb(0x81, 0xb8, 0x8c)),
        ChangeKind::StagedModified | ChangeKind::Modified => theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b)),
        ChangeKind::StagedDeleted | ChangeKind::Deleted => theme.ui(Color::Rgb(0xe7, 0x70, 0x70)),
        ChangeKind::StagedRenamed => theme.ui(Color::Rgb(0xb4, 0x8e, 0xad)),
        ChangeKind::Untracked => theme.ui(Color::Rgb(0x88, 0xc0, 0xd0)),
        ChangeKind::Conflicted => theme.ui(Color::Rgb(0xe7, 0x4c, 0x3c)),
    }
}

impl Widget for &mut SourceControlPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let focus_blue = self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff));
        // Black theme: the commit-input focus ring wears the muted brand teal
        // instead of the legacy blue (the outer border becomes the gradient).
        let accent = if self.focus_gradient {
            crate::gradient::rgb_color(crate::gradient::INNER_ACCENT)
        } else {
            focus_blue
        };
        let outer_style = if self.focused {
            Style::default().fg(focus_blue)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let outer = crate::widgets::surface::view_block(self.theme, true, outer_style);
        let inner = outer.inner(area);
        outer.render(area, buf);
        // Black theme: replace the solid focus border with the brand gradient.
        // The SOURCE CONTROL header sits inside the border, so nothing on the
        // border itself needs re-stamping.
        crate::widgets::surface::paint_view_focus(
            buf,
            area,
            self.theme,
            self.focused,
            self.focus_gradient,
        );
        self.last_area = area;
        self.last_inner = inner;
        self.last_input_area = Rect::default();
        self.last_button_area = Rect::default();
        self.last_commit_caret_area = Rect::default();
        self.commit_menu_item_areas.clear();
        self.last_list_area = Rect::default();
        self.last_scrollbar = Rect::default();
        self.last_row_actions = None;
        self.header_refresh_btn = Rect::default();
        self.last_branch_area = Rect::default();
        self.header_more_btn = Rect::default();
        // Frame-truth (#103): a zero-sized frame must not leave last
        // frame's repository rows clickable.
        self.repo_row_areas.clear();

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Row 0: SOURCE CONTROL header (light grey bold), inside the panel.
        let header_title = crate::i18n::tr("SOURCE CONTROL");
        // Header pills must start past the (translated) title plus a gap.
        let title_end = inner.x
            + u16::try_from(unicode_width::UnicodeWidthStr::width(header_title.as_ref()))
                .unwrap_or(u16::MAX)
                .saturating_add(1);
        // Croft Loch: the view title is the strong ink while the view has
        // the keyboard and muted otherwise, never bold, as the Explorer's.
        let tones = crate::widgets::surface::tones(self.theme);
        let title_style = match tones {
            Some(t) => Style::default().fg(if self.focused { t.strong() } else { t.muted() }),
            None => Style::default()
                .fg(self.theme.ui(Color::Rgb(0xb0, 0xb8, 0xc8)))
                .add_modifier(Modifier::BOLD),
        };
        buf.set_stringn(
            inner.x,
            inner.y,
            &header_title,
            inner.width as usize,
            title_style,
        );
        // Refresh pill at the right of the header row, mirroring the Remote
        // Explorer affordance. Live FS-driven refresh already keeps the panel
        // current; this is the manual override for the rare case the watcher
        // misses an edit. Only painted in a repo with room past the title.
        let pill_w: u16 = 2;
        let refresh_x = (inner.x + inner.width).saturating_sub(pill_w + 1);
        if self.status.in_repo && refresh_x > title_end {
            let rect = Rect {
                x: refresh_x,
                y: inner.y,
                width: pill_w,
                height: 1,
            };
            self.header_refresh_btn = rect;
            let hovered = crate::widgets::hover::contains(rect, self.hover_pointer);
            crate::widgets::header_pill::render(
                buf,
                refresh_x,
                inner.y,
                crate::widgets::header_pill::REFRESH_GLYPH,
                self.theme,
                hovered,
            );
            // "⋯" actions pill, one pill-plus-gap to the left of refresh,
            // opening the full Source Control actions menu (VS Code's title
            // "⋯" menu). cod-ellipsis (U+EA7C).
            let more_x = refresh_x.saturating_sub(pill_w + 1);
            if more_x > title_end {
                let more_rect = Rect {
                    x: more_x,
                    y: inner.y,
                    width: pill_w,
                    height: 1,
                };
                self.header_more_btn = more_rect;
                let more_hovered = crate::widgets::hover::contains(more_rect, self.hover_pointer);
                // Croft Loch draws the Explorer's midline ellipsis, so the
                // two views' "more" buttons are one glyph.
                let more_glyph = match tones {
                    Some(_) => '\u{22ef}',
                    None => crate::widgets::scm_menu::ICON_ELLIPSIS,
                };
                crate::widgets::header_pill::render(
                    buf,
                    more_x,
                    inner.y,
                    more_glyph,
                    self.theme,
                    more_hovered,
                );
            }
        }

        // Non-repo workspace: render the "No repository detected" card -
        // bordered card with illustration, title, description, primary
        // (Initialize Repository) and secondary (Open Folder) buttons,
        // plus a "Learn more about Git" help link. Mirrors the reference
        // design the user supplied.
        if !self.status.in_repo {
            self.last_init_repo_button_area = Rect::default();
            self.nested_repo_button_areas.clear();
            self.last_hero_area = Rect::default();
            self.render_no_repo_empty_state(inner, buf);
            // With several folders open, the OTHER folders' repositories
            // stay reachable from the empty state too (#161): the
            // overview paints over the card's top rows.
            self.render_repositories_section(inner, inner.y + 2, buf);
            return;
        }
        // Repo path: hero is only for the empty state.
        self.last_hero_area = Rect::default();
        self.nested_repo_button_areas.clear();

        // Row 2: branch row — a green branch glyph plus the branch name.
        let mut y = inner.y + 2;
        y = self.render_repositories_section(inner, y, buf);
        if y >= inner.y + inner.height {
            return;
        }
        let mut spans: Vec<Span> = Vec::with_capacity(5);
        // Codicon `cod-source-control` (U+EA68, verified against the
        // upstream codicon mapping.json AND the Nerd Fonts CSS) — the
        // Y-fork branch glyph in cyan, matching VS Code's mockup.
        // U+EB14 was wrong: that's `cod-link-external`, the share-arrow
        // the user spotted; ACTIVITY_SOURCE_CONTROL in icons.rs had the
        // same mis-pin and is fixed in the same commit.
        // Croft Loch keeps the accent for focus and selection: the glyph is
        // a muted icon beside the branch name.
        spans.push(Span::styled(
            "\u{ea68} ",
            Style::default().fg(match tones {
                Some(t) => t.muted(),
                None => self.theme.ui(Color::Rgb(0x88, 0xc0, 0xd0)),
            }),
        ));
        let label = self.status.head_label().to_string();
        // The glyph (2 cells) plus the name is the clickable target that
        // opens the branch picker, clamped to the panel width.
        let branch_w = (2 + label.chars().count() as u16).min(inner.width);
        self.last_branch_area = Rect {
            x: inner.x,
            y,
            width: branch_w,
            height: 1,
        };
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(tones.map_or_else(|| self.theme.ui(Color::White), |t| t.text()))
                .add_modifier(Modifier::BOLD),
        ));
        // What git stopped in the middle of (#1356): the ⋯ menu holds its
        // Continue / Abort.
        if let Some(op) = self.status.operation {
            spans.push(Span::styled(
                format!("  {}", op.label()),
                Style::default()
                    .fg(self.theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b)))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if self.status.ahead > 0 {
            spans.push(Span::styled(
                format!("  \u{2191}{}", self.status.ahead),
                Style::default().fg(self.theme.ui(Color::Rgb(0xa3, 0xbe, 0x8c))),
            ));
        }
        if self.status.behind > 0 {
            spans.push(Span::styled(
                format!("  \u{2193}{}", self.status.behind),
                Style::default().fg(self.theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b))),
            ));
        }
        buf.set_line(inner.x, y, &Line::from(spans), inner.width);
        y += 2; // blank gap below branch row

        // Rows y..y+3: commit-message input, a rounded box one row per
        // message line (up to MESSAGE_MAX_ROWS, then it scrolls).
        if y + 3 > inner.y + inner.height {
            return;
        }
        let line_count = self.message.split('\n').count();
        // Below the box: gap, Commit button, gap, the feedback line when
        // there is one, the separator and a couple of change-list rows. A
        // long message gives up rows (and scrolls) before any of those do;
        // a short panel still gets a one-line box.
        let feedback_rows = if self.commit_feedback.is_some() { 2 } else { 0 };
        let below = 1 + 3 + 1 + feedback_rows + 1 + MESSAGE_LIST_MIN_ROWS;
        let left = inner.y + inner.height - y;
        let input_box = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: (line_count.clamp(1, MESSAGE_MAX_ROWS) as u16 + 2)
                .min(left.saturating_sub(below).max(3))
                .min(left),
        };
        self.last_input_area = input_box;
        let input_border_style = if self.focused && !self.list_focused {
            Style::default().fg(accent)
        } else {
            Style::default().fg(self.theme.ui(Color::Rgb(0x60, 0x68, 0x78)))
        };
        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(input_border_style);
        let input_inner = input_block.inner(input_box);
        // Croft Loch: a raised well with a hairline edge (the rim at rest,
        // the accent while it has the keyboard) in place of the rounded line
        // box. The inner rect is the box's, so the text and the caret land on
        // the same cells.
        let field_focused = self.focused && !self.list_focused;
        match tones {
            Some(t) => crate::widgets::surface::paint_field(
                buf,
                input_box,
                t,
                if field_focused { t.accent() } else { t.rim() },
            ),
            None => input_block.render(input_box, buf),
        }
        if input_inner.width > 0 && input_inner.height > 0 {
            let content_y = input_inner.y;
            // 1-cell left padding inside the rounded border, and clamp the
            // available text width to `input_inner.width - 2` so the
            // string can never overflow past the right border into the
            // editor pane. The previous `buf.set_string` call had no width
            // cap and bled "r = push)" past the sidebar's right edge into
            // the editor area on narrow sidebar widths, which the user
            // reported as the placeholder visibly leaking out of the box.
            let text_x = input_inner.x + 1;
            let text_max = (input_inner.width as usize).saturating_sub(2);
            if self.message.is_empty() {
                self.message_scroll = 0;
                self.message_line_scroll = 0;
                if let Some(t) = tones {
                    // Croft Loch: "Message" in the muted ink, and the keys
                    // as a faint hint at the field's right edge, as long a
                    // hint as fits whole: never cut mid-word.
                    buf.set_stringn(
                        text_x,
                        content_y,
                        COMMIT_PLACEHOLDER,
                        text_max,
                        Style::default()
                            .fg(t.muted())
                            .add_modifier(Modifier::ITALIC),
                    );
                    if let Some(hint) = commit_placeholder_hint(text_max) {
                        let w = unicode_width::UnicodeWidthStr::width(hint);
                        buf.set_string(
                            text_x + (text_max - w) as u16,
                            content_y,
                            hint,
                            Style::default().fg(t.faint()),
                        );
                    }
                } else {
                    buf.set_stringn(
                        text_x,
                        content_y,
                        "Message (Enter = commit, \u{21e7}Enter = new line, \u{2303}Enter = push)",
                        text_max,
                        Style::default()
                            .fg(Color::Rgb(
                                INPUT_PROMPT_RGB.0,
                                INPUT_PROMPT_RGB.1,
                                INPUT_PROMPT_RGB.2,
                            ))
                            .add_modifier(Modifier::ITALIC),
                    );
                }
            } else {
                // Horizontal scroll: keep the caret inside the box so a
                // message wider than the input (e.g. a pasted path) stays
                // editable. Without this the box always shows byte 0, the
                // caret falls off the right edge, and edits at the tail are
                // invisible — the field reads as frozen. Vertical scroll
                // does the same for the caret's line in a long message.
                let (line, cursor) = self.caret_line_col();
                if cursor < self.message_scroll {
                    self.message_scroll = cursor;
                } else if text_max > 0 && cursor > self.message_scroll + text_max {
                    self.message_scroll = cursor - text_max;
                }
                let rows = input_inner.height as usize;
                if line < self.message_line_scroll {
                    self.message_line_scroll = line;
                } else if line >= self.message_line_scroll + rows {
                    self.message_line_scroll = line + 1 - rows;
                }
                for (row, text) in self
                    .message
                    .split('\n')
                    .skip(self.message_line_scroll)
                    .take(rows)
                    .enumerate()
                {
                    let visible: String = text
                        .chars()
                        .skip(self.message_scroll)
                        .take(text_max)
                        .collect();
                    buf.set_stringn(
                        text_x,
                        content_y + row as u16,
                        &visible,
                        text_max,
                        Style::default()
                            .fg(tones.map_or_else(|| self.theme.ui(Color::White), |t| t.text())),
                    );
                }
            }
        }
        y += input_box.height + 1; // input box + 1-row gap

        // Rows y..y+3: chunky split-button — wide "Commit" main + narrow
        // chevron caret. The caret reveals a "Commit & Push" dropdown so
        // users have the same affordance VS Code surfaces. Narrow panels
        // skip the split and render a plain Commit button so the caret
        // never collides with the label.
        if y + 3 > inner.y + inner.height {
            return;
        }
        // Black theme: brand-teal Commit button; Croft Dark keeps VS Code blue.
        let (blue, white) = primary_button_colors(self.theme, self.focus_gradient);
        let split_caret_w: u16 = 3;
        let split_gap_w: u16 = 1;
        if inner.width >= 16 + split_caret_w + split_gap_w {
            let main_w = inner.width.saturating_sub(split_caret_w + split_gap_w);
            let main_area = Rect {
                x: inner.x,
                y,
                width: main_w,
                height: 3,
            };
            let caret_area = Rect {
                x: inner.x + main_w + split_gap_w,
                y,
                width: split_caret_w,
                height: 3,
            };
            self.last_button_area = main_area;
            self.last_commit_caret_area = caret_area;
            // Nerd Fonts `cod-check` (U+EAB2, verified against
            // ryanoasis/nerd-fonts glyphnames.json v3.4.0) — the same crisp
            // codicon VS Code paints on its Commit button, replacing the
            // generic Unicode check that did not match the dropdown glyphs.
            if tones.is_some() {
                self.render_soft_split_button(buf, main_area, caret_area);
            } else {
                render_rounded_button(buf, main_area, "\u{eab2} Commit", blue, white);
                render_rounded_button(buf, caret_area, "▾", blue, white);
            }
        } else {
            let button_area = Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 3,
            };
            self.last_button_area = button_area;
            let hovered = crate::widgets::hover::contains(button_area, self.hover_pointer);
            render_button(buf, button_area, "Commit", blue, white, self.theme, hovered);
        }
        y += 3 + 1; // button + 1-row gap

        // Optional feedback line: its first line only, cut at the panel's
        // edge. Git's output runs to several lines and past any sidebar, and
        // an uncapped `set_string` painted it across the editor (#858).
        if let Some(msg) = self.commit_feedback.as_ref()
            && y < inner.y + inner.height
        {
            let style = if self.commit_feedback_is_error {
                Style::default().fg(self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70)))
            } else {
                Style::default().fg(self.theme.ui(Color::Rgb(0xa3, 0xbe, 0x8c)))
            };
            // git's headline (#858), wrapped to the panel: a refusal ran on
            // past its edge into the editor, which painted over it (#989).
            let line = crate::git::headline(msg);
            let rows = wrap_feedback(line, inner.width as usize, FEEDBACK_MAX_ROWS);
            for row in &rows {
                if y >= inner.y + inner.height {
                    break;
                }
                buf.set_stringn(inner.x, y, row, inner.width as usize, style);
                y += 1;
            }
            y += 1;
        }

        // Thin separator line.
        if y >= inner.y + inner.height {
            return;
        }
        // Croft Loch: a hairline seam hugging the list below, not a line
        // twice as bright as every other seam in the side bar.
        let (sep_glyph, sep_style) = match tones {
            Some(t) => ("\u{2581}", Style::default().fg(t.hairline())),
            None => (
                "─",
                Style::default().fg(self.theme.ui(Color::Rgb(0x40, 0x48, 0x58))),
            ),
        };
        for x in inner.x..inner.x + inner.width {
            buf.set_string(x, y, sep_glyph, sep_style);
        }
        y += 1;

        // List of changes.
        if y >= inner.y + inner.height {
            return;
        }
        let list_area = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: inner.y + inner.height - y,
        };
        self.last_list_area = list_area;
        let lines = self.list_layout();
        let total = lines.len();
        let viewport = list_area.height as usize;
        if viewport == 0 {
            return;
        }
        if self.scroll > total.saturating_sub(viewport) {
            self.scroll = total.saturating_sub(viewport);
        }

        let scrollbar_area = Rect {
            x: list_area.x + list_area.width.saturating_sub(1),
            y: list_area.y,
            width: u16::from(list_area.width > 0),
            height: list_area.height,
        };
        let scrollbar_metrics =
            scrollbar::vertical_metrics(scrollbar_area, total, viewport, self.scroll);
        if let Some(m) = scrollbar_metrics {
            self.last_scrollbar = m.area;
        }
        let row_width = list_area
            .width
            .saturating_sub(u16::from(scrollbar_metrics.is_some()));

        if total == 0 {
            buf.set_stringn(
                list_area.x,
                list_area.y,
                "No changes",
                list_area.width as usize,
                Style::default().fg(tones.map_or(Color::DarkGray, |t| t.muted())),
            );
            return;
        }

        let selected_bg = self.theme.ui(Color::Rgb(0x26, 0x4f, 0x78));
        let end = (self.scroll + viewport).min(total);
        for (row, idx) in (self.scroll..end).enumerate() {
            let row_y = list_area.y + row as u16;
            match &lines[idx] {
                ListLine::Header(section) => {
                    let (count, sec_add, sec_del) = self
                        .entries
                        .iter()
                        .filter(|e| e.kind.section() == *section)
                        .fold((0usize, 0usize, 0usize), |(c, a, d), e| {
                            (c + 1, a + e.additions, d + e.deletions)
                        });
                    let mut header_spans = match tones {
                        // Croft Loch: the side bar's section header (a muted
                        // codicon chevron, the label in body text) and a
                        // quiet count chip, muted on a faint lift.
                        Some(t) => vec![
                            Span::styled(
                                format!("{} ", crate::widgets::surface::CHEVRON_UNFOLDED),
                                Style::default().fg(t.muted()),
                            ),
                            Span::styled(
                                section_label(*section),
                                Style::default().fg(t.text()).add_modifier(Modifier::BOLD),
                            ),
                            Span::raw("  "),
                            Span::styled(
                                format!(" {count} "),
                                Style::default().fg(t.muted()).bg(t.keycap_on(t.chrome())),
                            ),
                        ],
                        None => vec![
                            Span::styled(
                                "\u{25be} ",
                                Style::default().fg(self.theme.ui(Color::Rgb(0xb0, 0xb8, 0xc8))),
                            ),
                            Span::styled(
                                section_label(*section),
                                Style::default()
                                    .fg(Color::Rgb(
                                        SECTION_HEADER_RGB.0,
                                        SECTION_HEADER_RGB.1,
                                        SECTION_HEADER_RGB.2,
                                    ))
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::raw("  "),
                            Span::styled(
                                format!(" {count} "),
                                Style::default()
                                    .fg(self.theme.ui(Color::White))
                                    .bg(self.theme.ui(Color::Rgb(0x2a, 0x33, 0x42)))
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ],
                    };
                    if sec_add > 0 || sec_del > 0 {
                        header_spans.push(Span::raw("  "));
                        if sec_add > 0 {
                            header_spans.push(Span::styled(
                                format!("+{sec_add}"),
                                Style::default()
                                    .fg(Color::Rgb(
                                        ADDITIONS_RGB.0,
                                        ADDITIONS_RGB.1,
                                        ADDITIONS_RGB.2,
                                    ))
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                        if sec_add > 0 && sec_del > 0 {
                            header_spans.push(Span::raw(" "));
                        }
                        if sec_del > 0 {
                            header_spans.push(Span::styled(
                                format!("-{sec_del}"),
                                Style::default()
                                    .fg(Color::Rgb(
                                        DELETIONS_RGB.0,
                                        DELETIONS_RGB.1,
                                        DELETIONS_RGB.2,
                                    ))
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                    }
                    buf.set_line(list_area.x, row_y, &Line::from(header_spans), row_width);
                }
                ListLine::Entry(entry_idx) => {
                    let entry = &self.entries[*entry_idx];
                    let row_rect = Rect {
                        x: list_area.x,
                        y: row_y,
                        width: row_width,
                        height: 1,
                    };
                    let is_primary = self.selected_change == Some(*entry_idx);
                    let is_in_multi = self.multi_selection.contains(entry_idx);
                    // Multi-selected rows all carry the VS Code list-active
                    // blue bg so a Cmd+A → Cmd+S sweep makes its scope
                    // visible; the primary row (the one driving the diff)
                    // keeps the brightest tone, secondary rows a slightly
                    // dimmer one so the user still sees which file the
                    // editor is bound to.
                    //
                    // Croft Loch: the one selected-row anatomy (band plus the
                    // accent bar in the pane's left padding column, greyed
                    // while the list does not have the keyboard), and a
                    // softer tint of the band for the rest of a multi-row
                    // selection instead of the legacy navy.
                    if let Some(t) = tones
                        && is_primary
                    {
                        crate::widgets::surface::paint_selected_row(
                            buf,
                            Rect {
                                x: area.x,
                                width: row_width.saturating_add(list_area.x - area.x),
                                ..row_rect
                            },
                            t,
                            t.chrome,
                            self.focused && self.list_focused,
                        );
                    }
                    let row_bg = if is_primary {
                        tones.is_none().then_some(selected_bg)
                    } else if is_in_multi {
                        Some(match tones {
                            Some(t) => crate::gradient::rgb_color(crate::gradient::lerp_rgb(
                                t.chrome, t.accent, 0.12,
                            )),
                            None => self.theme.ui(Color::Rgb(0x1f, 0x3a, 0x5c)),
                        })
                    } else {
                        crate::widgets::hover::row_hover_bg(
                            row_rect,
                            self.hover_pointer,
                            self.theme,
                        )
                    };
                    let is_selected = is_primary;
                    if let Some(bg) = row_bg {
                        let row_bg_style = Style::default().bg(bg);
                        for rx in 0..row_rect.width {
                            buf[(row_rect.x + rx, row_rect.y)]
                                .set_symbol(" ")
                                .set_style(row_bg_style);
                        }
                    }
                    let path_str = entry.path.as_str();
                    let is_dir = path_str.ends_with('/');
                    let icon = if is_dir {
                        icons::FOLDER_CLOSED
                    } else {
                        let basename = std::path::Path::new(path_str)
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path_str.to_string());
                        let suffix = std::path::Path::new(&basename)
                            .extension()
                            .map(|e| format!(".{}", e.to_string_lossy()))
                            .unwrap_or_default();
                        icons::for_path(&basename, &suffix)
                    };
                    let badge = entry.kind.badge();
                    let badge_str = badge.to_string();
                    let badge_w: u16 = 1;
                    let row_padding: u16 = 1;
                    let badge_x = row_rect
                        .x
                        .saturating_add(row_rect.width.saturating_sub(badge_w + row_padding));
                    let mut badge_style = Style::default()
                        .fg(badge_color(entry.kind, self.theme))
                        .add_modifier(Modifier::BOLD);
                    let mut icon_style = Style::default().fg(self.theme.ui(icon.color));
                    let mut path_style = Style::default().fg(self.theme.ui(Color::White));
                    if let Some(bg) = row_bg {
                        badge_style = badge_style.bg(bg);
                        icon_style = icon_style.bg(bg);
                        path_style = path_style.bg(bg);
                    }
                    if row_rect.width > badge_w + row_padding + 4 {
                        buf.set_string(badge_x, row_y, badge_str.as_str(), badge_style);
                    }
                    let icon_x = row_rect.x + 1;
                    buf.set_string(icon_x, row_y, icon.glyph.to_string(), icon_style);
                    let text_x = icon_x + 2;
                    // Reserve a 4-cell strip before the badge for inline
                    // action icons on the selected row. An unstaged row
                    // gets [discard][gap][stage]; a staged row gets the
                    // single [unstage] minus, mirroring VS Code (staged
                    // changes only offer Unstage; the Changes section
                    // offers Discard + Stage).
                    let is_staged = entry.kind.section() == ChangeSection::Staged;
                    let text_w_unreserved = badge_x.saturating_sub(text_x).saturating_sub(1);
                    let show_actions = is_selected && text_w_unreserved > ROW_ACTIONS_WIDTH;
                    let text_w_reserved = if show_actions { ROW_ACTIONS_WIDTH } else { 0 };
                    let text_w = text_w_unreserved.saturating_sub(text_w_reserved);
                    if let Some(t) = tones
                        && text_w > 0
                    {
                        // Croft Loch: the file name in body text, then its
                        // folder in the muted ink, as VS Code lists changes,
                        // so the eye finds the file before the path.
                        let trimmed = path_str.trim_end_matches('/');
                        let (dir, name) = match trimmed.rsplit_once('/') {
                            Some((dir, name)) if !is_dir => (Some(dir), name),
                            _ => (None, path_str),
                        };
                        let mut spans = vec![Span::styled(name, path_style.fg(t.text()))];
                        if let Some(dir) = dir {
                            spans.push(Span::styled(format!(" {dir}"), path_style.fg(t.muted())));
                        }
                        buf.set_line(text_x, row_y, &Line::from(spans), text_w);
                    } else if text_w > 0 {
                        let path_para = Paragraph::new(path_str).style(path_style);
                        path_para.render(
                            Rect {
                                x: text_x,
                                y: row_y,
                                width: text_w,
                                height: 1,
                            },
                            buf,
                        );
                    }
                    if show_actions {
                        let action_fg = tones.map_or_else(
                            || self.theme.ui(Color::Rgb(0xd4, 0xd9, 0xe2)),
                            |t| t.text(),
                        );
                        let mut action_style = Style::default().fg(action_fg);
                        if let Some(bg) = row_bg {
                            action_style = action_style.bg(bg);
                        }
                        let cell = |x: u16| Rect {
                            x,
                            y: row_y,
                            width: 1,
                            height: 1,
                        };
                        if is_staged {
                            let unstage_x = badge_x - 2;
                            buf.set_string(
                                unstage_x,
                                row_y,
                                UNSTAGE_GLYPH.to_string(),
                                action_style,
                            );
                            self.last_row_actions = Some(RowActionAreas {
                                entry_idx: *entry_idx,
                                discard: Rect::default(),
                                stage: Rect::default(),
                                unstage: cell(unstage_x),
                            });
                        } else {
                            let discard_x = badge_x - 4;
                            let stage_x = badge_x - 2;
                            buf.set_string(
                                discard_x,
                                row_y,
                                DISCARD_GLYPH.to_string(),
                                action_style,
                            );
                            buf.set_string(stage_x, row_y, STAGE_GLYPH.to_string(), action_style);
                            self.last_row_actions = Some(RowActionAreas {
                                entry_idx: *entry_idx,
                                discard: cell(discard_x),
                                stage: cell(stage_x),
                                unstage: Rect::default(),
                            });
                        }
                    }
                }
            }
        }
        if let Some(metrics) = scrollbar_metrics {
            scrollbar::render_vertical_on_chrome(buf, metrics, self.focused, self.theme);
        }
    }
}

/// Most rows a commit feedback message takes before it is cut with `…`.
const FEEDBACK_MAX_ROWS: usize = 4;

/// `msg` broken into rows of at most `width` columns, at spaces where it can
/// be and mid-word where a word is wider than a row; at most `max_rows`,
/// the last ending in `…` when the message runs longer.
fn wrap_feedback(msg: &str, width: usize, max_rows: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr as _;
    if width == 0 {
        return Vec::new();
    }
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for word in msg.split_whitespace() {
        let gap = usize::from(!row.is_empty());
        if row.width() + gap + word.width() <= width {
            if gap == 1 {
                row.push(' ');
            }
            row.push_str(word);
            continue;
        }
        if !row.is_empty() {
            rows.push(std::mem::take(&mut row));
        }
        for c in word.chars() {
            if row.width() + unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) > width {
                rows.push(std::mem::take(&mut row));
            }
            row.push(c);
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    if rows.len() > max_rows {
        rows.truncate(max_rows);
        let last = &mut rows[max_rows - 1];
        while last.width() + 1 > width {
            last.pop();
        }
        last.push('…');
    }
    rows
}

#[cfg(test)]
mod tests;
