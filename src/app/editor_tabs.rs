//! Tabs and editor groups, tab menus and pickers, Go Back/Forward, bookmarks, symbol
//! tabs, and moving or copying an editor into a new window.

use super::*;

/// Build the right-click context-menu items for an editor tab strip
/// click on tab `idx`. `tab_count` is the total number of open tabs;
/// used to gate "Close Others" / "Close to the Right" so they don't
/// surface when they would be no-ops.
pub(super) fn build_tab_context_menu_items(
    idx: usize,
    tab_count: usize,
    is_split: bool,
    clicked: Option<&Path>,
    reveal_enabled: bool,
    clicked_is_preview: bool,
    clicked_is_pinned: bool,
) -> Vec<MenuEntry> {
    let mut items: Vec<MenuEntry> = Vec::new();
    items.push(MenuEntry::item("Close", MenuAction::CloseTab(idx)));
    if tab_count > 1 {
        items.push(MenuEntry::item(
            "Close Others",
            MenuAction::CloseOtherTabs(idx),
        ));
    }
    if idx + 1 < tab_count {
        items.push(MenuEntry::item(
            "Close to the Right",
            MenuAction::CloseTabsToRight(idx),
        ));
    }
    if tab_count > 1 {
        items.push(MenuEntry::item("Close Saved", MenuAction::CloseSavedTabs));
    }
    items.push(MenuEntry::item("Close All", MenuAction::CloseAllTabs));
    // Copy Path: any tab backed by a real on-disk path, local or remote (it's
    // a clipboard write, not a host bridge). Sits between the close group and
    // Reveal in Finder, mirroring VS Code's tab-menu order.
    if let Some(path) = clicked {
        items.push(MenuEntry::item(
            "Copy Path",
            MenuAction::CopyTabPath(path.to_path_buf()),
        ));
        items.push(MenuEntry::item(
            "Copy Relative Path",
            MenuAction::CopyTabRelativePath(path.to_path_buf()),
        ));
    }
    // Reveal in Finder: local-macOS only, and only when the tab is backed by a
    // real on-disk path (a blank/untitled buffer has nothing to reveal). On a
    // remote SSH session the entry is withheld, matching the explorer menu.
    if let Some(path) = clicked.filter(|_| reveal_enabled) {
        items.push(MenuEntry::item(
            "Reveal in Finder",
            MenuAction::RevealInFinder(path.to_path_buf()),
        ));
    }
    // Reveal in Explorer View: select the file in croft's own tree. The tree
    // exists local and remote, so this is gated only on a real path.
    if let Some(path) = clicked {
        items.push(MenuEntry::item(
            "Reveal in Explorer View",
            MenuAction::RevealInExplorer(path.to_path_buf()),
        ));
    }
    // Keep Open: only meaningful for the replaceable preview tab (italic).
    // Promotes it so a subsequent single-click open won't replace it. VS Code
    // omits the entry on an already-permanent tab, so we do too.
    if clicked_is_preview {
        items.push(MenuEntry::item("Keep Open", MenuAction::KeepTabOpen(idx)));
    }
    // Pin / Unpin: always offered. A pinned tab reads "Unpin"; pinning keeps it
    // leftmost and shields it from Close Others / Close to the Right.
    items.push(MenuEntry::item(
        if clicked_is_pinned { "Unpin" } else { "Pin" },
        MenuAction::ToggleTabPin(idx),
    ));
    // A top-level "Split Right" (the common case), then a "Split & Move"
    // submenu holding every directional split, the four directional moves, and
    // Split in Group — mirroring VS Code's tab menu.
    items.push(MenuEntry::item("Split Right", MenuAction::SplitEditor));
    items.push(MenuEntry::Submenu {
        label: String::from("Split & Move"),
        items: vec![
            MenuEntry::item("Split Up", MenuAction::SplitEditorUp),
            MenuEntry::item("Split Down", MenuAction::SplitEditorDown),
            MenuEntry::item("Split Left", MenuAction::SplitEditorLeft),
            MenuEntry::item("Split Right", MenuAction::SplitEditor),
            MenuEntry::Separator,
            // VS Code's vertical labels: Above = Up, Below = Down.
            MenuEntry::item("Move Above", MenuAction::MoveEditorUp),
            MenuEntry::item("Move Below", MenuAction::MoveEditorDown),
            MenuEntry::item("Move Left", MenuAction::MoveEditorLeft),
            MenuEntry::item("Move Right", MenuAction::MoveEditorRight),
            MenuEntry::Separator,
            MenuEntry::item("Split in Group", MenuAction::SplitInGroup),
        ],
    });
    // VS Code's "Move into New Window" / "Copy into New Window". croft has no
    // OS windows, so a "window" is a full-screen, chrome-free editor surface
    // (Esc returns). Move works on any buffer (it carries the editor itself);
    // Copy reopens the file, so it needs a real on-disk path.
    items.push(MenuEntry::item(
        "Move into New Window",
        MenuAction::MoveIntoNewWindow(idx),
    ));
    if clicked.is_some() {
        items.push(MenuEntry::item(
            "Copy into New Window",
            MenuAction::CopyIntoNewWindow(idx),
        ));
    }
    // Focus-group navigation appears once a second group exists.
    if is_split {
        items.push(MenuEntry::item(
            "Focus Left Group",
            MenuAction::FocusGroupLeft,
        ));
        items.push(MenuEntry::item(
            "Focus Right Group",
            MenuAction::FocusGroupRight,
        ));
    }
    items
}

/// How many closed editor tabs Cmd+K ⇧W can walk back through.
pub(super) const CLOSED_TAB_STACK_CAP: usize = 20;

impl ClosedTab {
    /// The reopen record of `ed`, or `None` for a scratch buffer with no
    /// backing file, which has nothing to reopen.
    pub(super) fn of(ed: &Editor) -> Option<Self> {
        Some(Self {
            path: ed.path.clone()?,
            pinned: ed.pinned,
            cursor: (ed.cursor_row, ed.cursor_col),
            scroll: ed.scroll,
        })
    }
}

/// What a bulk tab close (Close Others, Close to the Right, Close Saved,
/// Close All) does to the tabs in its `reach`: the reopen records of those
/// it drops, in tab order so the reopen stack pops most-recent-first, and
/// how many it keeps open instead for their unsaved changes (#862). Pinned
/// tabs are never in reach (#941). A file open in several groups is
/// recorded once, at its last tab, so one reopen brings it back.
pub(super) fn bulk_close_records<'a>(
    reach: impl IntoIterator<Item = &'a Editor>,
) -> (Vec<ClosedTab>, usize) {
    let mut records: Vec<ClosedTab> = Vec::new();
    let mut unsaved = 0;
    for ed in reach.into_iter().filter(|ed| !ed.pinned) {
        if ed.dirty {
            unsaved += 1;
        } else if let Some(t) = ClosedTab::of(ed) {
            records.retain(|r| r.path != t.path);
            records.push(t);
        }
    }
    (records, unsaved)
}

/// The status line of a bulk tab close that closed `removed` tabs, which
/// `closed` describes ("Closed 2 other tabs"). One that kept tabs in its
/// reach open for their unsaved changes says so (#941), rather than leaving
/// them looking forgotten.
pub(super) fn bulk_close_status(closed: String, removed: usize, unsaved: usize) -> String {
    match (removed, unsaved) {
        (_, 0) => closed,
        (0, 1) => String::from("Kept 1 tab with unsaved changes open"),
        (0, n) => format!("Kept {n} tabs with unsaved changes open"),
        (_, n) => format!("{closed}; kept {n} with unsaved changes open"),
    }
}

impl App {
    /// [`Self::open_at`] for a position that came FROM a language server, whose
    /// column counts UTF-16 code units rather than characters. The file has to
    /// be open before the conversion can happen — the mapping needs the target
    /// line's text — so this opens at column 0 and then re-seats the caret.
    ///
    /// Out-of-range answers are clamped rather than rejected, matching
    /// `open_at`: a server that names a line past the end of a buffer we have
    /// since edited should still land the user on the right file.
    pub(super) fn open_at_utf16(&mut self, path: &Path, line: u32, character: u32) -> Result<()> {
        self.open_at(path, line as usize, 0)?;
        let row = self.editor.cursor_row;
        let col = self.editor.utf16_col_to_char_pub(row, character);
        let max_col = self
            .editor
            .lines
            .get(row)
            .map(|l| l.chars().count())
            .unwrap_or(0);
        self.editor.cursor_col = col.min(max_col);
        self.editor.ensure_cursor_col_visible();
        Ok(())
    }

    /// Go to the next (or previous) entry of the PROBLEMS list from the
    /// caret: build messages and diagnostics together, files in the panel's
    /// order and each file's entries by position, wrapping at the ends. The
    /// entry under the caret is skipped, so repeating the command walks the
    /// list. Works from the terminal pane too, which is the point after a
    /// build: the caret jumps into the editor at the message's position.
    pub(super) fn go_to_problem(&mut self, forward: bool) {
        use crate::lsp::manager::DiagnosticSeverity;
        let here = self.editor.path.clone();
        let cur = (self.editor.cursor_row as u32, self.editor.cursor_col as u32);
        let mut list: Vec<(PathBuf, crate::widgets::problems::ProblemItem, u32)> = Vec::new();
        // The walk keeps the panel's order, which does not depend on which
        // file is open: only the open file's UTF-16 columns can be turned
        // into characters, so ordering by converted columns reshuffled a
        // file once it opened, and a step from the remembered entry then
        // passed over the rows that had moved ahead of it.
        for g in self.problems.groups() {
            for item in &g.items {
                // Compare in characters, as the caret counts: an LSP column
                // is UTF-16, and only the open file's text is at hand.
                let ccol = if item.col_utf16 && here.as_ref() == Some(&g.path) {
                    self.editor
                        .utf16_col_to_char_pub(item.line as usize, item.col)
                        as u32
                } else {
                    item.col
                };
                list.push((g.path.clone(), item.clone(), ccol));
            }
        }
        if list.is_empty() {
            self.status = String::from("No problems");
            return;
        }
        let key = |e: &(PathBuf, crate::widgets::problems::ProblemItem, u32)| {
            (e.0.clone(), e.1.line, e.2)
        };
        // Still where the last jump left the caret: step from that entry
        // (by its place in the list when it is still there, else by its
        // position), whatever the clamped caret compares as.
        let from_last = self
            .last_problem_jump
            .as_ref()
            .filter(|(p, _, _, _, row, col)| {
                here.as_ref() == Some(p)
                    && self.editor.cursor_row == *row
                    && self.editor.cursor_col == *col
            })
            .and_then(|(p, line, col, at, _, _)| {
                let same = |e: &(PathBuf, crate::widgets::problems::ProblemItem, u32)| {
                    &e.0 == p && e.1.line == *line && e.1.col == *col
                };
                list.get(*at)
                    .filter(|e| same(e))
                    .map(|_| *at)
                    .or_else(|| list.iter().position(same))
            });
        let idx = if let Some(at) = from_last {
            if forward {
                (at + 1) % list.len()
            } else {
                (at + list.len() - 1) % list.len()
            }
        } else {
            match here {
                None => {
                    if forward {
                        0
                    } else {
                        list.len() - 1
                    }
                }
                Some(p) => {
                    let at = (p, cur.0, cur.1);
                    if forward {
                        list.iter().position(|e| key(e) > at).unwrap_or(0)
                    } else {
                        list.iter()
                            .rposition(|e| key(e) < at)
                            .unwrap_or(list.len() - 1)
                    }
                }
            }
        };
        // An entry whose file no longer opens (deleted since the build) is
        // passed over in the walk's direction, so it cannot hold every
        // repeat on itself; only when none opens is the failure reported.
        let mut idx = idx;
        let mut tried = 0;
        let (path, item) = loop {
            let (path, item, _) = list[idx].clone();
            let opened = if item.col_utf16 {
                self.open_at_utf16(&path, item.line, item.col)
            } else {
                self.open_at(&path, item.line as usize, item.col as usize)
            };
            match opened {
                Ok(()) => break (path, item),
                Err(e) => {
                    tried += 1;
                    if tried == list.len() {
                        self.status = format!("Open failed: {e}");
                        return;
                    }
                    idx = if forward {
                        (idx + 1) % list.len()
                    } else {
                        (idx + list.len() - 1) % list.len()
                    };
                }
            }
        };
        self.last_problem_jump = Some((
            path.clone(),
            item.line,
            item.col,
            idx,
            self.editor.cursor_row,
            self.editor.cursor_col,
        ));
        let kind = match item.severity {
            DiagnosticSeverity::Error => "error",
            DiagnosticSeverity::Warning => "warning",
            _ => "info",
        };
        let msg: String = item
            .message
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        self.status = format!(
            "Problem {}/{} ({}{}): {kind}: {msg}",
            idx + 1,
            list.len(),
            name.unwrap_or_default(),
            format_args!(":{}", item.line + 1),
        );
    }

    pub(super) fn open_at(&mut self, path: &Path, row: usize, col: usize) -> Result<()> {
        self.hover_popup = None;
        self.hover_diagnostic = None;
        self.editor.open_preview(path)?;
        self.sync_open_file_poll_mtime();
        self.place_caret_at(row, col);
        Ok(())
    }

    /// Put the active editor's caret at the 0-based `row` and `col`, clamped
    /// to the text, scroll it to mid-screen and focus the editor.
    pub fn place_caret_at(&mut self, row: usize, col: usize) {
        let row = row.min(self.editor.lines.len().saturating_sub(1));
        let max_col = self
            .editor
            .lines
            .get(row)
            .map(|l| l.chars().count())
            .unwrap_or(0);
        self.editor.cursor_row = row;
        self.editor.cursor_col = col.min(max_col);
        let page = self.editor.page_size().max(1);
        self.editor.scroll = row.saturating_sub(page / 2);
        self.editor.scroll_col = 0;
        self.editor.ensure_cursor_col_visible();
        self.focus_pane(Pane::Editor);
        self.poke_cursor();
    }

    /// The active editor's position as a NavLoc, for threading through
    /// back/forward so the walk is reversible. None on file-less surfaces
    /// (welcome screen, image tabs without a path).
    pub(super) fn current_nav_loc(&self) -> Option<NavLoc> {
        let path = self.editor.path.clone()?;
        Some(NavLoc {
            path,
            row: self.editor.cursor_row,
            col: self.editor.cursor_col,
        })
    }

    /// Run one key or mouse gesture, and when it moved the editor to
    /// another file without going through the navigation history itself
    /// (Quick Open, a Search result, the Explorer, a tab), record where the
    /// user left (#1635): Go Back knew only the jumps that recorded
    /// themselves, so the usual way of changing file left nothing to go
    /// back to. A gesture that recorded or walked the history (Go to
    /// Definition, Go Back itself) is left as it did it, and so is one that
    /// moved focus to another editor group: a history entry holds no group,
    /// so Go Back would reopen that group's file in the focused one.
    pub(super) fn recording_file_switches<R>(&mut self, gesture: impl FnOnce(&mut Self) -> R) -> R {
        let left = self.current_nav_loc();
        let changes = self.nav.changes();
        let group = self.editor_group_identity();
        let result = gesture(self);
        if let Some(left) = left
            && self.nav.changes() == changes
            && self.editor_group_identity() == group
            && self.editor.path.as_ref() != Some(&left.path)
        {
            self.nav.record(left);
        }
        #[cfg(test)]
        self.assert_one_view_per_tab();
        result
    }

    /// Fail when any tab, in any group, has more than one view slot filled
    /// ([`Editor::views_filled`]). Test builds run it after every key, click
    /// and paste, so every test that drives the app through them checks it,
    /// not only the tests written about viewers.
    #[cfg(test)]
    pub(super) fn assert_one_view_per_tab(&self) {
        for group in self.editor_layout.all_groups(&self.editor) {
            for tab in &group.editors {
                let views = tab.views_filled();
                assert!(
                    views.len() <= 1,
                    "{} shows {} views at once: {}",
                    tab.path.as_deref().map_or_else(
                        || String::from("an untitled tab"),
                        |p| p.display().to_string()
                    ),
                    views.len(),
                    views.join(", ")
                );
            }
        }
    }

    /// Which editor group has focus, as the split's leaf count and the
    /// focused leaf's position: a change in either is a group change.
    pub(super) fn editor_group_identity(&self) -> (usize, usize) {
        (
            self.editor_layout.leaf_count(),
            self.editor_layout.active_dfs_index(),
        )
    }

    /// A code navigation that leaves the active tab for another file keeps
    /// that tab open (#1561), as VS Code does by default
    /// (`enablePreviewFromCodeNavigation` off): were it the preview, the
    /// target would take its slot and close the file being read, and Go
    /// Back would swap them again. A jump within the same file replaces
    /// nothing, so the tab stays a preview.
    ///
    /// The target counts as the same file under the editor's own path
    /// matching (canonicalized), so a symlinked tab and the canonical path
    /// a language server answers with are one file. Returns whether the tab
    /// was promoted, for [`Self::restore_preview_after_failed_navigation`].
    pub(super) fn keep_preview_open_for_navigation_to(&mut self, target: &Path) -> bool {
        let same_file = self.editor.path.as_deref().is_some_and(|p| {
            p == target
                || matches!(
                    (p.canonicalize(), target.canonicalize()),
                    (Ok(a), Ok(b)) if a == b
                )
        });
        if !same_file && self.editor.is_preview(self.editor.active_index()) {
            self.editor.pin_active();
            return true;
        }
        false
    }

    /// A navigation whose target failed to open (a stale language-server
    /// answer, a file deleted since it entered the history) left the user
    /// on the tab it started from, so that tab goes back to being the
    /// preview it was before [`Self::keep_preview_open_for_navigation_to`].
    pub(super) fn restore_preview_after_failed_navigation(&mut self, pinned: bool) {
        if pinned {
            self.editor.preview = true;
        }
    }

    pub(super) fn nav_back(&mut self) {
        let current = self.current_nav_loc();
        let Some(loc) = self.nav.back(current) else {
            self.status = "No previous location".to_string();
            return;
        };
        let pinned = self.keep_preview_open_for_navigation_to(&loc.path);
        let line = loc.row;
        match self.open_at(&loc.path, loc.row, loc.col) {
            Ok(()) => {
                self.status = format!("Back to {} line {}", self.status_path(&loc.path), line + 1)
            }
            Err(e) => {
                self.restore_preview_after_failed_navigation(pinned);
                self.status = format!("Go back failed: {e}")
            }
        }
    }

    /// VS Code "Go Forward" (Ctrl+Shift+-): return along the trail Go Back
    /// walked, restoring the position each back step left.
    pub(super) fn nav_forward(&mut self) {
        let current = self.current_nav_loc();
        let Some(loc) = self.nav.forward(current) else {
            self.status = "No forward location".to_string();
            return;
        };
        let pinned = self.keep_preview_open_for_navigation_to(&loc.path);
        let line = loc.row;
        match self.open_at(&loc.path, loc.row, loc.col) {
            Ok(()) => {
                self.status = format!(
                    "Forward to {} line {}",
                    self.status_path(&loc.path),
                    line + 1
                )
            }
            Err(e) => {
                self.restore_preview_after_failed_navigation(pinned);
                self.status = format!("Go forward failed: {e}")
            }
        }
    }

    /// VS Code "Go to Last Edit Location" (Cmd+K Cmd+Q): jump to the most
    /// recent buffer edit across the open editors, threading the current
    /// position onto the back stack so Ctrl+- returns. The location lives
    /// on each editor (`last_edit_pos`), so an edit in a background split
    /// counts too; a buffer closed since its edit takes its location with
    /// it, like the rest of its view state.
    pub(super) fn goto_last_edit_location(&mut self) {
        let mut best: Option<(std::time::Instant, NavLoc)> = None;
        let groups = self.editor_layout.all_groups(&self.editor);
        for group in groups {
            for ed in &group.editors {
                let (Some(at), Some((row, col)), Some(path)) =
                    (ed.last_edit_at, ed.last_edit_pos, ed.path.clone())
                else {
                    continue;
                };
                if best.as_ref().is_none_or(|(b, _)| at > *b) {
                    best = Some((at, NavLoc { path, row, col }));
                }
            }
        }
        let Some((_, loc)) = best else {
            self.status = String::from("No edits this session");
            return;
        };
        if let Some(cur) = self.current_nav_loc() {
            self.nav.record(cur);
        }
        let line = loc.row;
        match self.open_at(&loc.path, loc.row, loc.col) {
            Ok(()) => {
                self.status = format!(
                    "Last edit: {} line {}",
                    self.status_path(&loc.path),
                    line + 1
                )
            }
            Err(e) => self.status = format!("Go to last edit failed: {e}"),
        }
    }

    /// Record the focused group's tab at `idx` as reopenable (Cmd+K ⇧W).
    /// Scratch buffers with no backing file are not recorded. Must run
    /// BEFORE the close mutates the tab list. Tabs that merely move (into a
    /// new window) or whose file was just trashed are deliberately never
    /// recorded by their callers.
    pub(super) fn record_closed_tab_at(&mut self, idx: usize) {
        let record = self.editor.editors.get(idx).and_then(ClosedTab::of);
        self.push_closed_tabs(record);
    }

    /// Push `records` onto the reopen stack, dropping the oldest past
    /// [`CLOSED_TAB_STACK_CAP`].
    pub(super) fn push_closed_tabs(&mut self, records: impl IntoIterator<Item = ClosedTab>) {
        self.closed_tabs.extend(records);
        let over = self.closed_tabs.len().saturating_sub(CLOSED_TAB_STACK_CAP);
        self.closed_tabs.drain(..over);
    }

    /// Record every tab of the focused group that a bulk close is about to
    /// drop, its reach being the tabs `pred` selects (see
    /// [`bulk_close_records`]). Returns how many in reach it keeps open for
    /// their unsaved changes.
    pub(super) fn record_closed_tabs_where(
        &mut self,
        pred: impl Fn(usize, &Editor) -> bool,
    ) -> usize {
        let reach = self.editor.editors.iter().enumerate();
        let (records, unsaved) =
            bulk_close_records(reach.filter(|(i, ed)| pred(*i, ed)).map(|(_, ed)| ed));
        self.push_closed_tabs(records);
        unsaved
    }

    /// Cmd+K ⇧W: reopen the most recently closed editor tab (VS Code
    /// "Reopen Closed Editor"), restoring its pin state, cursor, and
    /// scroll. Entries whose file has vanished since the close are skipped,
    /// falling through to the next reopenable one.
    pub fn reopen_closed_tab(&mut self) {
        while let Some(t) = self.closed_tabs.pop() {
            if !t.path.exists() {
                continue;
            }
            let opened = if t.pinned {
                self.editor.open_pinned(&t.path)
            } else {
                self.editor.open_preview(&t.path)
            };
            match opened {
                Ok(()) => {
                    self.sync_open_file_poll_mtime();
                    let rows = self.editor.lines.len();
                    self.editor.cursor_row = t.cursor.0.min(rows.saturating_sub(1));
                    let max_col = self
                        .editor
                        .lines
                        .get(self.editor.cursor_row)
                        .map(|l| l.chars().count())
                        .unwrap_or(0);
                    self.editor.cursor_col = t.cursor.1.min(max_col);
                    self.editor.scroll = t.scroll.min(rows.saturating_sub(1));
                    self.editor.ensure_cursor_col_visible();
                    self.tree.reveal_path(&t.path);
                    self.focus_pane(Pane::Editor);
                    self.poke_cursor();
                    self.status = format!("Reopened {}", self.status_path(&t.path));
                }
                Err(e) => {
                    self.status = format!("Reopen failed: {e}");
                }
            }
            return;
        }
        self.status = String::from("No closed tabs to reopen");
    }

    /// Split the editor into two side-by-side columns (`Cmd+\`). The new
    /// group duplicates the active file (re-opens its path, copies the
    /// cursor/scroll position) and takes focus, landing in the right
    /// column. No-op if already split or the active tab has no path on
    /// disk (a blank/unsaved buffer has nothing to mirror).
    /// "Split Editor Right": split the active group side by side, the new
    /// focused duplicate to the right. The default split, on `Cmd+\`.
    pub(super) fn split_editor(&mut self) {
        self.split_editor_dir(editor_layout::SplitDir::Horizontal, true);
    }

    /// Split the active group, duplicating its file into a new focused group
    /// placed in the direction the caller asks for. `dir` chooses the seam's
    /// orientation (Horizontal = side by side, Vertical = stacked) and
    /// `new_after` whether the NEW group sits after the existing one (right /
    /// below) or before it (left / above) — the four VS Code Split commands:
    /// Right = (H, after), Left = (H, before), Down = (V, after), Up = (V,
    /// before). Splitting is offered only from an unsplit editor for now.
    pub(super) fn split_editor_dir(&mut self, dir: editor_layout::SplitDir, new_after: bool) {
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Cannot split: save the file first");
            return;
        };
        // A fresh EditorTabs starts blank-initial; `open_preview` reuses
        // that placeholder tab so the new group holds exactly the file
        // (not a stray blank tab beside it). Opening it as the PREVIEW
        // slot - not pinned - means the next file the user opens in this
        // column swaps straight into it (clean "left = A, right = B"
        // comparison) rather than stacking a second tab. Editing or
        // double-clicking pins it, as everywhere else.
        let mut group = EditorTabs::default();
        if group.open_preview(&path).is_err() {
            self.status = String::from("Cannot split: failed to open file");
            return;
        }
        // Mirror the source view state (caret + scroll, incl. the wrap
        // sub-line offset) so the duplicate opens exactly where the
        // source was rather than at the top of the file.
        group.copy_view_position_from(&self.editor);
        // The new focused group (now in `self.editor`, always the active leaf)
        // takes the active slot; the existing group settles into the sibling.
        let existing = std::mem::replace(&mut self.editor, group);
        self.editor_layout.split_active(existing, dir, new_after);
        self.focus_pane(Pane::Editor);
    }

    /// VS Code "Move Editor into <dir> Group": relocate the active tab to the
    /// group adjacent in `dir`. If a neighbour exists there the tab moves into
    /// it (the source collapses when its last tab leaves); otherwise a new group
    /// is created in that direction holding the moved tab. Moving the only tab
    /// of a single group with no neighbour is a no-op (nowhere to move to).
    pub(super) fn move_active_editor(&mut self, dir: editor_layout::Dir) {
        // A nominal area: seam adjacency is scale-invariant, so the exact pane
        // size doesn't matter for finding the directional neighbour.
        let synth = ratatui::layout::Rect::new(0, 0, 10_000, 10_000);
        let target = self.editor_layout.neighbor_leaf_in_dir(synth, 1, dir);
        self.move_active_editor_to_group(target, dir);
    }

    /// Move the active tab into the group at depth-first index `target`, or,
    /// with none, into a new group created in direction `dir`.
    pub(super) fn move_active_editor_to_group(
        &mut self,
        target: Option<usize>,
        dir: editor_layout::Dir,
    ) {
        match target {
            Some(target) => {
                // Both copies of the file hold unsaved edits of their own
                // (#2745): one tab cannot keep them both, so nothing moves.
                let clash = self
                    .editor_layout
                    .group_ref_at_dfs(target)
                    .is_some_and(|g| {
                        matches!(
                            g.landing_for(&self.editor),
                            crate::widgets::editor::MoveLanding::Clash(_)
                        )
                    });
                if clash {
                    let label = self.editor.tab_display_label(self.editor.active_index());
                    self.status = format!(
                        "Move: {label} has other unsaved changes in that group; save or close one first"
                    );
                    return;
                }
                let moved = self.editor.take_active_editor();
                let source_emptied = self.editor.is_blank_initial();
                let reduced = std::mem::take(&mut self.editor);
                self.editor = self.editor_layout.refocus_to_dfs(reduced, target);
                self.editor.receive_moved(moved);
                if source_emptied {
                    self.editor_layout
                        .prune_blank_inactive(|t| t.is_blank_initial());
                }
                self.focus_pane(Pane::Editor);
            }
            None => {
                if self.editor.tab_count() <= 1 {
                    self.status = String::from("Move: the editor is already a single group");
                    return;
                }
                let moved = self.editor.take_active_editor();
                let mut group = EditorTabs::default();
                group.push_editor(moved);
                let existing = std::mem::replace(&mut self.editor, group);
                self.editor_layout
                    .split_active(existing, dir.split_dir(), dir.places_new_after());
                self.focus_pane(Pane::Editor);
            }
        }
    }

    /// VS Code "Move Editor into Next / Previous Group" (#1565): the active
    /// tab moves to the group after (or before) this one in reading order;
    /// past the last (or first) group, a new group opens to the right (or
    /// left) to hold it, as VS Code does.
    pub(super) fn move_active_editor_to_adjacent_group(&mut self, next: bool) {
        let here = self.editor_layout.active_dfs_index();
        let (target, dir) = if next {
            let target = here + 1;
            (
                (target < self.editor_layout.leaf_count()).then_some(target),
                editor_layout::Dir::Right,
            )
        } else {
            (here.checked_sub(1), editor_layout::Dir::Left)
        };
        self.move_active_editor_to_group(target, dir);
    }

    /// View: Move Editor Left / Right (#1565): the active tab one place
    /// along its tab bar. At the end of the bar (or of the pinned block)
    /// nothing moves and the status line says so.
    pub(super) fn move_active_tab(&mut self, right: bool) {
        if !self.editor.move_active_tab(right) {
            self.status = String::from(if right {
                "Move: the editor is already the last tab"
            } else {
                "Move: the editor is already the first tab"
            });
        }
        self.poke_cursor();
    }

    /// VS Code "Split in Group": open a second view of the active file beside
    /// it. A TUI has no true in-group sub-view, so the observable equivalent is
    /// a side-by-side split duplicating the active file — the same result as
    /// Split Right.
    pub(super) fn split_in_group(&mut self) {
        self.split_editor_dir(editor_layout::SplitDir::Horizontal, true);
    }

    /// Wheel-scroll the inactive split group under the pointer, if any.
    ///
    /// inactive_dfs_index_at hit-tests EditorTabs::last_full_area — the rect
    /// a group recorded during its last render. That is stale for one frame
    /// after focus moves, and worse, a group that has never painted since the
    /// split was created has Rect::default(), so the wheel misses it
    /// entirely (focused left → right pane never scrolled). So: use the
    /// recorded rect when it actually contains the cell, and otherwise fall
    /// back to the geometric layout (leaf_rects), excluding the active leaf,
    /// which is the authority for where each group sits on screen right now.
    ///
    /// Returns true when a group was found and scrolled. Focus is never moved.
    pub(super) fn scroll_inactive_group_at(&mut self, col: u16, row: u16, notches: i32) -> bool {
        if !self.editor_layout.is_split() {
            return false;
        }
        let target = self
            .editor_layout
            .inactive_dfs_index_at(col, row)
            .or_else(|| {
                let area = self.editor_layout_area;
                if area.width == 0 || area.height == 0 {
                    return None;
                }
                let active = self.editor_layout.active_dfs_index();
                self.editor_layout
                    .leaf_rects(area, EDITOR_SPLIT_MIN)
                    .iter()
                    .enumerate()
                    .position(|(i, r)| i != active && rect_contains(*r, col, row))
            });
        let Some(dfs) = target else {
            return false;
        };
        // inactive_groups_mut walks the leaves in depth-first order, minus
        // the active one.
        let active = self.editor_layout.active_dfs_index();
        let inactive_idx = if dfs > active { dfs - 1 } else { dfs };
        if let Some(group) = self.editor_layout.inactive_groups_mut().nth(inactive_idx) {
            // A PDF page step is a synchronous rasterise, so a background
            // split's PDF coalesces a wheel burst exactly like the focused one
            // (see wheel_pdf_page), stamped after the render.
            let ed = group.active_editor_mut();
            let pdf_doc = (ed.diff.is_none() && ed.pdf_page().is_some()).then(|| ed.path.clone());
            if let Some(doc) = pdf_doc {
                if pdf_wheel_cooling(self.last_pdf_wheel.as_ref(), notches, &doc) {
                    return true;
                }
                group.wheel_scroll_active(notches);
                self.last_pdf_wheel = Some((std::time::Instant::now(), notches.signum(), doc));
                return true;
            }
            group.wheel_scroll_active(notches);
            return true;
        }
        false
    }

    /// Move keyboard focus to the editor group in the given physical
    /// column (`true` = left, `false` = right). Hoists the active group
    /// through `editor_layout` so the focused group is always `editor`, keeping
    /// the physical columns fixed. No-op when not split or already focused
    /// there.
    pub(super) fn focus_editor_group(&mut self, want_left: bool) {
        if !self.editor_layout.is_split() {
            return;
        }
        // Physical columns map to depth-first leaf order: leftmost = 0,
        // rightmost = last. Hoist the active group out and pull the target
        // leaf's group into `self.editor` so the invariant holds.
        let target = if want_left {
            0
        } else {
            self.editor_layout.leaf_count() - 1
        };
        if self.editor_layout.active_dfs_index() == target {
            // Already focused there; the chord still doubles as "focus editor".
            if self.focus != Pane::Editor {
                self.focus_pane(Pane::Editor);
            }
            return;
        }
        let current = std::mem::take(&mut self.editor);
        self.editor = self.editor_layout.refocus_to_dfs(current, target);
        self.focus_pane(Pane::Editor);
    }

    /// The seam (if any) the cell `(col, row)` lands on, using the same
    /// two-cell hit-zone as the sidebar/terminal seams. Returns a clone so an
    /// in-flight drag can hold the boundary across frames.
    pub(super) fn seam_at(&self, col: u16, row: u16) -> Option<editor_layout::Seam> {
        self.editor_seams
            .iter()
            .find(|s| match s.dir {
                editor_layout::SplitDir::Horizontal => {
                    (col == s.pos || col == s.pos.saturating_sub(1))
                        && row >= s.cross_start
                        && row < s.cross_end
                }
                editor_layout::SplitDir::Vertical => {
                    (row == s.pos || row == s.pos.saturating_sub(1))
                        && col >= s.cross_start
                        && col < s.cross_end
                }
            })
            .cloned()
    }

    /// After a tab close, collapse the split if the focused group ran out
    /// of real tabs. The surviving (other) group becomes the sole editor.
    pub(super) fn collapse_split_if_empty(&mut self) {
        // While a full-screen window is open, `editor` is the window and the
        // grid is parked out of the tree — collapsing here would fold the
        // parked grid into the window. The window's own emptiness is handled by
        if !self.editor_layout.is_split() || !self.editor.is_blank_initial() {
            return;
        }
        // The marks its last tab left go before the group does.
        self.settle_bookmarks();
        // The focused group is empty; drop it and promote a surviving group to
        // become the new active editor. Evict the right slot's image so it
        // can't ghost after the column disappears.
        self.editor = self.editor_layout.collapse_active();
        self.editor_seams.clear();
        self.disable_editor_image(1);
        // Focus stays on the editor pane; just re-light the promoted
        // group's border. (No `focus_pane` so this is safe to call from
        // any close path without re-poking the cursor mid-frame.)
        self.sync_focus_flags();
    }

    /// VS Code "Move into New Window": open the tab at `idx` in a brand-new
    /// window of the host terminal (a real, separate window running croft on the
    /// same workspace with the file open), then close the tab here — the file
    /// has moved to the new window. The current window is otherwise untouched.
    pub(super) fn move_into_new_window(&mut self, idx: usize) {
        // The new window reads the file from disk, so unsaved edits would
        // stay behind in the tab this closes (#862).
        if self.editor.editors.get(idx).is_some_and(|e| e.dirty) {
            self.status = String::from("New Window: save the file first");
            return;
        }
        if !self.open_tab_in_new_window(idx) {
            return;
        }
        // The file now lives in the new window; drop it from this one. Mirrors
        // MenuAction::CloseTab so the split collapses if this was its last tab.
        let label = self.editor.tab_display_label(idx);
        if self.editor.close_tab(idx) {
            self.sync_open_file_poll_mtime();
            self.poke_cursor();
            self.collapse_split_if_empty();
        }
        self.status = format!("{label}: moved into a new window");
    }

    /// VS Code "Copy into New Window" (⌘K O): open the tab at `idx` in a
    /// brand-new OS terminal window, leaving the original tab exactly where it
    /// is. The current window is not touched at all.
    pub(super) fn copy_into_new_window(&mut self, idx: usize) {
        if !self.open_tab_in_new_window(idx) {
            return;
        }
        let label = self.editor.tab_display_label(idx);
        self.status = format!("{label}: copied into a new window");
    }

    /// Spawn a new OS terminal window running croft on the current workspace
    /// with the tab at `idx` opened. Returns false (and sets a status) when the
    /// action can't run: an unsaved/path-less buffer, a remote session (no
    /// local window server on the headless host), or a spawn failure. Shared by
    /// Move and Copy into New Window — the only difference is what the caller
    /// does to the source tab afterwards.
    pub(super) fn open_tab_in_new_window(&mut self, idx: usize) -> bool {
        // A new croft process opens a file by path, so an untitled buffer has
        // nothing to hand off.
        let Some(path) = self.editor.tab_path(idx) else {
            self.status = String::from("New Window: save the file first");
            return false;
        };
        // A new OS window only exists where croft can talk to a window server:
        // the local machine. Over SSH croft runs on the headless host.
        if self.is_remote {
            self.status = String::from("New Window: not available over SSH");
            return false;
        }
        match spawn_new_window(&self.tree.root, &path) {
            Ok(()) => true,
            Err(e) => {
                self.status = format!("New Window failed: {e}");
                false
            }
        }
    }

    /// The "all open tabs" list: every tab of the active group by its strip
    /// label, the current one selected. Picking one activates it, scrolling the
    /// strip to it.
    pub(super) fn open_tab_picker(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let rows: Vec<ListRow> = self
            .editor
            .tab_display_labels()
            .into_iter()
            .enumerate()
            .map(|(i, label)| ListRow {
                id: i.to_string(),
                label,
            })
            .collect();
        let mut picker = ListPicker::new(ListPurpose::OpenTab, "Open Tabs", rows);
        picker.selected = self.editor.active_index();
        self.open_list_picker(picker, "No open tabs");
    }

    /// Keep symbol tabs (#369) and the other tabs of their files in step.
    /// Runs once per tick; returns true when anything visible changed.
    ///
    /// A symbol tab holds the whole file, as a split does, and like a split
    /// it is a buffer of its own. So the tabs of a file that has a symbol tab
    /// MIRROR each other here: the one edited since the last pass (the
    /// focused one first, when several were) is copied into the rest, and
    /// saving any of them clears the dirty dot on all. A file that is live in
    /// a collab session is left to the session, which converges its panes
    /// already. Then each symbol tab follows the edit so its clip stays on
    /// its symbol, and closes once the symbol is gone (or, holding unsaved
    /// text no whole-file tab holds, turns into a tab of the whole file).
    pub(crate) fn sync_symbol_views(&mut self) -> bool {
        let paths = self.symbol_view_paths();
        if paths.is_empty() {
            return false;
        }
        let mut changed = false;
        for path in &paths {
            if !self.symbol_path_is_live(path) {
                changed |= self.mirror_symbol_siblings(path);
            }
        }
        changed | self.follow_symbol_views()
    }

    /// Every path some symbol tab shows, once each.
    pub(super) fn symbol_view_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = Vec::new();
        for group in self.editor_layout.all_groups(&self.editor) {
            for ed in &group.editors {
                if ed.symbol_view.is_some()
                    && let Some(p) = ed.path.as_ref()
                    && !paths.contains(p)
                {
                    paths.push(p.clone());
                }
            }
        }
        paths
    }

    /// A file live in a collab session: its tabs follow the session, not
    /// each other, so [`Self::sync_symbol_views`] leaves them alone.
    pub(super) fn symbol_path_is_live(&self, path: &Path) -> bool {
        self.collab.as_ref().is_some_and(|session| {
            collab_file_key(&self.tree.root, path).is_some_and(|f| session.is_live(&f))
        })
    }

    /// Mirror the text tabs of `path` against each other; see
    /// [`Self::sync_symbol_views`].
    pub(super) fn mirror_symbol_siblings(&mut self, path: &Path) -> bool {
        let active = self.editor.active_index();
        let mut members: Vec<(bool, &mut crate::widgets::editor::Editor)> = self
            .editor
            .editors
            .iter_mut()
            .enumerate()
            .map(|(i, ed)| (i == active, ed))
            .chain(
                self.editor_layout
                    .inactive_groups_mut()
                    .flat_map(|g| g.editors.iter_mut().map(|ed| (false, ed))),
            )
            .filter(|(_, ed)| ed.path.as_deref() == Some(path) && !ed.has_non_text_view())
            .collect();
        members.sort_by_key(|(focused, _)| !*focused);
        let settled = members
            .iter()
            .all(|(_, ed)| ed.mirror_seq == Some(ed.edit_seq));
        let mixed_dirty =
            members.iter().any(|(_, ed)| ed.dirty) && members.iter().any(|(_, ed)| !ed.dirty);
        if settled && !mixed_dirty {
            return false;
        }
        // The source is a buffer edited since the last pass. A tab that has
        // never been mirrored (`None`) may hold older text, such as a fresh
        // open from disk beside unsaved edits, so it is a source only when
        // nothing else can be.
        let source = members
            .iter()
            .position(|(_, ed)| ed.mirror_seq.is_some_and(|seq| seq != ed.edit_seq))
            .or_else(|| members.iter().position(|(_, ed)| ed.mirror_seq.is_some()))
            .unwrap_or(0);
        // Lent out for the pass rather than cloned: this runs after every
        // editor key on a file with a symbol tab.
        let lines = std::mem::take(&mut members[source].1.lines);
        let step = members[source].1.undo_step_id;
        let caret = (members[source].1.cursor_row, members[source].1.cursor_col);
        let mut changed = false;
        for (i, (_, ed)) in members.iter_mut().enumerate() {
            if i != source {
                changed |= ed.mirror_lines_from(&lines, step, caret);
            }
        }
        members[source].1.lines = lines;
        // Every member now holds the same text, so one that is clean (just
        // saved, or undone back to its saved state) proves they all match
        // the file on disk.
        if members.iter().any(|(_, ed)| !ed.dirty) {
            for (_, ed) in members.iter_mut().filter(|(_, ed)| ed.dirty) {
                ed.mark_clean_like_sibling();
                changed = true;
            }
        }
        for (_, ed) in members.iter_mut() {
            ed.mirror_seq = Some(ed.edit_seq);
        }
        changed
    }

    /// Move every symbol tab's clip with the edits since its last look, and
    /// close the tabs whose symbol is gone, except an orphan with unsaved
    /// text, which turns into a tab of the whole file.
    pub(super) fn follow_symbol_views(&mut self) -> bool {
        let mut changed = false;
        let mut notes: Vec<String> = Vec::new();
        let mut follow = |ed: &mut crate::widgets::editor::Editor| -> bool {
            let seq = ed.edit_seq;
            if ed.symbol_view.as_ref().is_none_or(|v| v.seen_seq == seq) {
                return false;
            }
            let kind = ed.path.as_deref().and_then(syntax_kind_of);
            let text = ed.lines.join("\n");
            // The caret that made the edit: a sibling's, mirrored in, or
            // this tab's own. A collaborator's edit, applied and pinned as
            // synced, has no caret here and is not this tab's own.
            let caret = match ed.mirror_caret.take() {
                Some((row, col)) => Some(crate::symbol_range::Caret {
                    row,
                    col,
                    own: false,
                }),
                None if ed.collab_doc_gen != 0 && ed.edit_seq == ed.collab_synced_seq => None,
                None => Some(crate::symbol_range::Caret {
                    row: ed.cursor_row,
                    col: ed.cursor_col,
                    own: true,
                }),
            };
            let Some(view) = ed.symbol_view.as_mut() else {
                return false;
            };
            changed = true;
            match view.follow(text, seq, kind, caret) {
                crate::symbol_range::ViewUpdate::Kept => false,
                crate::symbol_range::ViewUpdate::Renamed(old) => {
                    notes.push(format!("{old} is now {}", view.name));
                    false
                }
                crate::symbol_range::ViewUpdate::Gone => true,
            }
        };
        let gone_here: Vec<usize> = (0..self.editor.editors.len())
            .filter(|&i| follow(&mut self.editor.editors[i]))
            .collect();
        let mut gone_inactive: Vec<Vec<usize>> = Vec::new();
        for group in self.editor_layout.inactive_groups_mut() {
            gone_inactive.push(
                (0..group.editors.len())
                    .filter(|&i| follow(&mut group.editors[i]))
                    .collect(),
            );
        }
        // A gone symbol tab holding unsaved text that no tab of the whole
        // file also holds (its file tab was closed) becomes that file tab
        // instead of closing, or its edits, in and out of the clip, would
        // be lost with it.
        let mut kept_paths: Vec<PathBuf> = Vec::new();
        // Said last, so the status (the last note) names the kept edits
        // even when a sibling tab of the same symbol closed beside it.
        let mut kept_notes: Vec<String> = Vec::new();
        let mut keep = |ed: &mut crate::widgets::editor::Editor,
                        whole_file_open: &dyn Fn(&Path) -> bool|
         -> bool {
            let name = ed
                .symbol_view
                .as_ref()
                .map(|v| v.name.clone())
                .unwrap_or_default();
            let orphan = ed.dirty
                && ed
                    .path
                    .as_deref()
                    .is_some_and(|p| !whole_file_open(p) && !kept_paths.iter().any(|k| k == p));
            if orphan {
                kept_paths.extend(ed.path.clone());
                // Its mirror state stays: it is in step with its siblings
                // (mirrored this pass, or left to the collab session), so
                // its next edit is the source, not overwritten by them, and
                // folds into the undo step it already has.
                ed.drop_symbol_clip();
                kept_notes.push(format!(
                    "{name} is gone; its tab now shows the whole file with the unsaved edits"
                ));
            } else {
                notes.push(format!("Closed the {name} tab: that symbol is gone"));
            }
            orphan
        };
        let any_gone = !gone_here.is_empty() || gone_inactive.iter().any(|g| !g.is_empty());
        let whole_file_tabs: Vec<PathBuf> = if any_gone {
            self.editor_layout
                .all_groups(&self.editor)
                .flat_map(|g| g.editors.iter())
                .filter(|e| e.symbol_view.is_none() && !e.has_non_text_view())
                .filter_map(|e| e.path.clone())
                .collect()
        } else {
            Vec::new()
        };
        let whole_file_open = |p: &Path| whole_file_tabs.iter().any(|w| w == p);
        let gone_here: Vec<usize> = gone_here
            .into_iter()
            .filter(|&i| !keep(&mut self.editor.editors[i], &whole_file_open))
            .collect();
        for (g, group) in self.editor_layout.inactive_groups_mut().enumerate() {
            gone_inactive[g].retain(|&i| !keep(&mut group.editors[i], &whole_file_open));
        }
        notes.extend(kept_notes);
        for &i in gone_here.iter().rev() {
            self.editor.close_tab(i);
        }
        // A group this emptied folds away, as closing its last tab by hand
        // would; one the user left blank on purpose stays.
        let pre_blank: Vec<bool> = self
            .editor_layout
            .inactive_groups()
            .map(|g| g.is_blank_initial())
            .collect();
        let mut doomed = Vec::new();
        for (g, group) in self.editor_layout.inactive_groups_mut().enumerate() {
            for &i in gone_inactive[g].iter().rev() {
                group.close_tab(i);
            }
            if !gone_inactive[g].is_empty() && group.is_blank_initial() && !pre_blank[g] {
                doomed.push(g);
            }
        }
        if !doomed.is_empty() {
            self.editor_layout.prune_inactive_at(&doomed);
        }
        if let Some(note) = notes.pop() {
            self.status = note;
        }
        changed
    }

    /// The outline of the ACTIVE buffer: the Outline view's symbols when they
    /// describe this file, else a tree-sitter outline of the live text (the
    /// Outline may be hidden, or still waiting on its language server).
    pub(super) fn active_file_symbols(
        &self,
        path: &Path,
    ) -> Vec<crate::lsp::manager::OutlineSymbol> {
        if self.outline.path() == Some(path) && !self.outline.symbols().is_empty() {
            return self.outline.symbols().to_vec();
        }
        self.compute_syntax_outline(path)
    }

    /// Open the symbol under the cursor as its own tab (#369).
    ///
    /// The INNERMOST enclosing symbol, so opening from inside a method shows
    /// the method rather than its `impl`: the enclosing block is almost
    /// never what was meant.
    pub(super) fn open_symbol_tab(&mut self) {
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Save this buffer before opening a symbol from it");
            return;
        };
        let line = self.editor.cursor_row;
        let symbols = self.active_file_symbols(&path);
        let Some(sym) = crate::symbol_range::enclosing_symbol(&symbols, line as u32) else {
            self.status = if symbols.is_empty() {
                String::from("No symbols for this file yet")
            } else {
                String::from("The cursor is not inside a symbol")
            };
            return;
        };
        let (name, first, last) = (
            sym.name.clone(),
            sym.range_start_line as usize,
            sym.range_end_line as usize,
        );
        self.open_symbol_tab_for(name, first, last);
    }

    /// Open lines `first..=last` of the active file as a symbol tab named
    /// `name`, placed after the active tab, or focus the symbol tab already
    /// showing that symbol, matched by its tree-sitter identity: two
    /// symbols can start on one line, and pickers spell some symbols and
    /// their first lines differently. The new tab starts from the live
    /// buffer, unsaved edits included, and keeps the caret if it sits
    /// inside the symbol.
    pub(crate) fn open_symbol_tab_for(&mut self, name: String, first: usize, last: usize) {
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Save this buffer before opening a symbol from it");
            return;
        };
        if self.editor.has_non_text_view() {
            self.status = String::from("Symbol tabs open from a text editor");
            return;
        }
        let identity = crate::symbol_range::syntax_identity(
            &self.editor.lines.join("\n"),
            syntax_kind_of(&path),
            &name,
            first,
            last,
        );
        if let Some(i) = self.editor.editors.iter().position(|ed| {
            ed.path.as_deref() == Some(path.as_path())
                && ed
                    .symbol_view
                    .as_ref()
                    .is_some_and(|v| v.shows(&name, first, identity.as_ref()))
        }) {
            self.editor.select(i);
            self.focus_pane(Pane::Editor);
            return;
        }
        let lines = self.editor.lines.clone();
        let dirty = self.editor.dirty;
        let caret = (self.editor.cursor_row, self.editor.cursor_col);
        if let Err(e) = self.editor.open_in_new_tab(&path) {
            self.status = format!("Could not open {}: {e}", path.display());
            return;
        }
        let kind = syntax_kind_of(&path);
        let active = self.editor.active_index();
        let ed = &mut self.editor.editors[active];
        if ed.lines != lines {
            ed.seed_unsaved(lines, dirty);
        }
        let last = last.min(ed.lines.len().saturating_sub(1));
        let first = first.min(last);
        let view = crate::symbol_range::SymbolView::new(
            name.clone(),
            ed.lines.join("\n"),
            first,
            last,
            ed.edit_seq,
            kind,
        );
        ed.symbol_view = Some(view);
        ed.preview = false;
        if (first..=last).contains(&caret.0) {
            (ed.cursor_row, ed.cursor_col) = caret;
        } else {
            (ed.cursor_row, ed.cursor_col) = (first, 0);
        }
        ed.scroll = first;
        ed.clamp_to_symbol_view();
        self.sync_open_file_poll_mtime();
        self.focus_pane(Pane::Editor);
        self.status = format!("Opened {name} as its own tab");
    }

    /// Every tab of every editor group: the focused tab first, then the
    /// focused group's tabs and the other groups' in order. The order in
    /// which a tab's copy of a file's marks counts.
    pub(super) fn editors_in_bookmark_order(&self) -> impl Iterator<Item = &Editor> + '_ {
        std::iter::once(&*self.editor).chain(
            self.editor_layout
                .all_groups(&self.editor)
                .flat_map(|g| g.editors.iter()),
        )
    }

    /// The tab whose copy of `path`'s marks counts: the first, in
    /// [`Self::editors_in_bookmark_order`], that shows the file and holds
    /// marks for it, since those follow its text; else the first that
    /// still holds marks from showing it earlier.
    pub(super) fn bookmark_source(&self, path: &Path) -> Option<&Editor> {
        self.editors_in_bookmark_order()
            .find(|e| e.path.as_deref() == Some(path) && e.bookmarks.contains_key(path))
            .or_else(|| {
                self.editors_in_bookmark_order()
                    .find(|e| e.bookmarks.contains_key(path))
            })
    }

    /// The bookmarks of every tab of every editor group, keyed by file, one
    /// set per file: each file's from [`Self::bookmark_source`]. The tabs of
    /// a file hold the same marks (see [`Self::share_bookmarks`]), each on
    /// its own text, so the set is never a union of their copies, which
    /// would bring back a mark a tab cleared. A file whose tabs all closed
    /// adds the marks they left.
    pub(super) fn all_bookmarks(
        &self,
    ) -> std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>> {
        let mut all = std::collections::HashMap::new();
        for ed in self.editors_in_bookmark_order() {
            for path in ed.bookmarks.keys() {
                if !all.contains_key(path)
                    && let Some(source) = self.bookmark_source(path)
                {
                    all.insert(path.clone(), source.bookmarks[path].clone());
                }
            }
        }
        for (path, set) in &self.closed_bookmarks {
            all.entry(path.clone()).or_insert_with(|| set.clone());
        }
        all
    }

    /// Keep the marks the tabs closed since the last call left, for each
    /// file no open tab holds marks for, and hand a file's kept marks back
    /// to the tabs that show it again. Run every tick, before anything
    /// reads the marks. Returns whether a tab took marks back.
    pub(crate) fn settle_bookmarks(&mut self) -> bool {
        let released: Vec<_> = self
            .editor_layout
            .all_groups_mut(&mut self.editor)
            .flat_map(|g| g.take_released_bookmarks())
            .collect();
        for marks in released {
            for (path, set) in marks {
                if !self
                    .editors_in_bookmark_order()
                    .any(|e| e.bookmarks.contains_key(&path))
                {
                    self.closed_bookmarks.insert(path, set);
                }
            }
        }
        if self.closed_bookmarks.is_empty() {
            return false;
        }
        let mut taken = Vec::new();
        let groups = self.editor_layout.all_groups_mut(&mut self.editor);
        for group in groups {
            // A diff, image or sheet tab's rows are not the file's lines.
            for ed in group.editors.iter_mut().filter(|e| !e.has_non_text_view()) {
                if let Some(path) = ed.path.clone()
                    && !ed.bookmarks.contains_key(&path)
                    && let Some(set) = self.closed_bookmarks.get(&path)
                {
                    ed.adopt_bookmarks(&path, Some(set));
                    taken.push(path);
                }
            }
        }
        for path in &taken {
            self.closed_bookmarks.remove(path);
        }
        !taken.is_empty()
    }

    /// Hand saved bookmarks to every tab of every group, each taking its
    /// file's set trimmed to the lines the file still has.
    pub(super) fn restore_bookmarks_everywhere(
        &mut self,
        marks: &std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>,
    ) {
        if marks.is_empty() {
            return;
        }
        self.closed_bookmarks
            .retain(|path, _| !marks.contains_key(path));
        for ed in self
            .editor_layout
            .all_groups_mut(&mut self.editor)
            .flat_map(|g| g.editors.iter_mut())
        {
            ed.restore_bookmarks(marks);
        }
    }

    /// Toggle a bookmark on the cursor's line and report the new state in the
    /// status bar. A buffer with no path on disk cannot hold marks, and says so
    /// rather than silently doing nothing.
    pub(super) fn toggle_bookmark(&mut self) {
        self.toggle_bookmark_at(self.editor.cursor_row + 1);
    }

    /// [`Self::toggle_bookmark`] for a given 1-based `line`, leaving the
    /// cursor and scroll where they are (the gutter Ctrl+click).
    pub(super) fn toggle_bookmark_at(&mut self, line: usize) {
        if !self.editor_is_text() {
            return;
        }
        // Start from the marks the file's other tabs hold, or toggling here
        // would hand every tab this tab's set alone.
        self.adopt_shared_bookmarks();
        self.status = match self.editor.toggle_bookmark_at(line) {
            Some(true) => format!("Bookmark set on line {line}"),
            Some(false) => format!("Bookmark cleared on line {line}"),
            None => String::from("Save the file before bookmarking a line"),
        };
        if let Some(path) = self.editor.path.clone() {
            self.share_bookmarks(&path);
        }
    }

    /// Give the focused tab its file's marks from another tab, in any
    /// group, when it holds none of its own: a tab opened on a file after
    /// its marks were set starts without them.
    pub(super) fn adopt_shared_bookmarks(&mut self) {
        // The marks a file's last tab left when it closed come back first.
        self.settle_bookmarks();
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        if self.editor.bookmarks.contains_key(&path) {
            return;
        }
        let Some(source) = self.bookmark_source(&path) else {
            return;
        };
        let set = &source.bookmarks[&path];
        // A tab showing the file may hold unsaved edits this one lacks:
        // each mark goes to the line it is on here.
        let marks = if source.path.as_deref() == Some(path.as_path()) {
            crate::widgets::editor::carry_marks(set, &source.lines, &self.editor.lines)
        } else {
            set.clone()
        };
        self.editor.adopt_bookmarks(&path, Some(&marks));
    }

    /// Hand `path`'s marks, as the focused tab now holds them, to every
    /// other tab of every group that shows the file or still holds marks
    /// for it, so a file has one set of marks: one set or cleared in a tab
    /// is set or cleared in all of them.
    pub(super) fn share_bookmarks(&mut self, path: &Path) {
        // The focused tab's set is the file's now; marks a closed tab left
        // would bring back one it cleared.
        self.closed_bookmarks.remove(path);
        let active = self.editor.active_index();
        let (before, rest) = self.editor.editors.split_at_mut(active);
        let Some((source, after)) = rest.split_first_mut() else {
            return;
        };
        let source: &Editor = source;
        let marks = source.bookmarks.get(path);
        let in_splits = self
            .editor_layout
            .inactive_groups_mut()
            .flat_map(|g| g.editors.iter_mut());
        for ed in before.iter_mut().chain(after.iter_mut()).chain(in_splits) {
            if ed.path.as_deref() == Some(path) {
                // Another copy of the file, whose unsaved edits may differ
                // from this one's: each mark goes to the line it is on there.
                let carried =
                    marks.map(|m| crate::widgets::editor::carry_marks(m, &source.lines, &ed.lines));
                ed.adopt_bookmarks(path, carried.as_ref());
            } else if ed.bookmarks.contains_key(path) {
                ed.adopt_bookmarks(path, marks);
            }
        }
    }

    /// Jump to the next (`forward`) or previous bookmark in the open file,
    /// wrapping at the ends. Explains an empty set instead of doing nothing.
    pub(super) fn goto_bookmark(&mut self, forward: bool) {
        if !self.editor_is_text() {
            return;
        }
        let jumped = if forward {
            self.editor.goto_next_bookmark()
        } else {
            self.editor.goto_prev_bookmark()
        };
        self.status = match jumped {
            Some(line) => {
                // Position among the marks, 1-based, then the line itself:
                // "Bookmark 2 of 3 (line 42)", never a line number over a count.
                let marks = self.editor.bookmarked_lines();
                let total = marks.len();
                let idx = marks.iter().position(|&l| l == line).map_or(0, |i| i + 1);
                format!("Bookmark {idx} of {total} (line {line})")
            }
            None => String::from("No bookmarks in this file (Cmd+Opt+Shift+K sets one)"),
        };
    }

    /// Bookmarks: View Bookmarks: a filterable list of every bookmark in
    /// the open tabs of every editor group (`file:line  text of the line`),
    /// Enter jumps to it. Only the file a tab shows counts: the marks a tab
    /// still holds for a file it showed earlier are not of an open file.
    pub(super) fn open_bookmark_picker(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let root = self.tree.root.clone();
        // The focused group first, so a mark it shares with another group
        // jumps to the tab already in front of the user.
        let active = self.editor_layout.active_dfs_index();
        let groups = std::iter::once((active, &self.editor)).chain(
            (0..self.editor_layout.leaf_count())
                .filter_map(|g| Some((g, self.editor_layout.group_ref_at_dfs(g)?))),
        );
        // Two tabs on one file (a split, a symbol tab) list a shared mark once.
        let mut seen = std::collections::HashSet::new();
        let mut found: Vec<(BookmarkTarget, String)> = Vec::new();
        for (group, tabs) in groups {
            // A diff, image or sheet tab's rows are not the file's lines.
            for (tab, ed) in tabs.editors.iter().enumerate() {
                let Some(path) = ed.path.as_ref().filter(|_| !ed.has_non_text_view()) else {
                    continue;
                };
                // Only lines the buffer has (see `bookmarked_lines`), and on
                // a symbol tab only the lines of its symbol, the ones it shows.
                for line in ed.bookmarked_lines() {
                    if bookmark_line_shown(ed, line) && seen.insert((path.clone(), line)) {
                        let text = ed.lines[line - 1].trim().chars().take(80).collect();
                        let path = path.clone();
                        found.push((
                            BookmarkTarget {
                                group,
                                tab,
                                path,
                                line,
                            },
                            text,
                        ));
                    }
                }
            }
        }
        found.sort_by(|(a, _), (b, _)| (&a.path, a.line).cmp(&(&b.path, b.line)));
        self.pending_bookmarks.clear();
        let mut rows = Vec::with_capacity(found.len());
        for (target, text) in found {
            let shown = target.path.strip_prefix(&root).unwrap_or(&target.path);
            rows.push(ListRow {
                id: self.pending_bookmarks.len().to_string(),
                label: format!("{}:{}  {text}", shown.display(), target.line),
            });
            self.pending_bookmarks.push(target);
        }
        let picker = ListPicker::new(ListPurpose::Bookmark, "Bookmarks", rows);
        self.open_list_picker(picker, "No bookmarks (Cmd+Opt+Shift+K sets one)");
    }

    /// Enter on a View Bookmarks row: focus the editor group and the tab
    /// that hold the mark, and put the caret on its line there, with no
    /// selection left over for the next keystroke to type over. A tab that
    /// closed or moved on since the list opened leaves the file to open in
    /// that group as any jump would.
    pub(super) fn jump_to_bookmark(&mut self, target: BookmarkTarget) {
        let BookmarkTarget {
            group,
            tab,
            path,
            line,
        } = target;
        if group != self.editor_layout.active_dfs_index()
            && self.editor_layout.group_ref_at_dfs(group).is_some()
        {
            let current = std::mem::take(&mut self.editor);
            self.editor = self.editor_layout.refocus_to_dfs(current, group);
        }
        self.hover_popup = None;
        self.hover_diagnostic = None;
        let same_tab = self.editor.editors.get(tab).is_some_and(|ed| {
            ed.path.as_deref() == Some(path.as_path())
                && !ed.has_non_text_view()
                && bookmark_line_shown(ed, line)
        });
        if same_tab {
            self.editor.select(tab);
        } else if let Err(e) = self.editor.open_preview(&path) {
            self.status = format!("Cannot open bookmark: {e}");
            return;
        } else {
            // A tab opened here for a file another tab still shows gets
            // that file's marks too.
            self.adopt_shared_bookmarks();
        }
        self.sync_open_file_poll_mtime();
        self.editor.jump_to_bookmark(line);
        self.focus_pane(Pane::Editor);
        self.poke_cursor();
        self.status = format!("Bookmark at line {line}");
    }

    /// Clear every bookmark in the open file, reporting how many went.
    pub(super) fn clear_bookmarks(&mut self) {
        // Every path in (chord and palette) goes through here, so the guard
        // lives here: a preview or non-text view must not clear marks it does
        // not show.
        if !self.editor_is_text() {
            return;
        }
        let gone = self.editor.clear_bookmarks();
        // From every tab of the file, or a copy left in another would bring
        // the marks back with the next session.
        if let Some(path) = self.editor.path.clone() {
            self.share_bookmarks(&path);
        }
        self.status = if gone == 0 {
            String::from("No bookmarks in this file")
        } else if gone == 1 {
            String::from("Cleared 1 bookmark")
        } else {
            format!("Cleared {gone} bookmarks")
        };
    }

    /// Close every tab holding `path`, in the focused group and in every
    /// other one: a tab in an unfocused group would otherwise keep reading a
    /// rewritten file through its old index, and nothing heals it. Every
    /// match goes, not the first: `push_editor` can open one path twice. A
    /// group emptied by this is pruned as every other close path prunes it;
    /// the focused group is left blank for the caller to refill.
    pub(super) fn close_tabs_with_path_everywhere(&mut self, path: &Path) {
        // `close_tab` on a group's last tab leaves a blank editor with no
        // path, so the search finds nothing more and the loop ends.
        while let Some(idx) = self.editor.find_any_tab_with_path(path) {
            self.editor.close_tab(idx);
        }
        // The prune names the groups THIS close emptied, by position: a
        // group the user left blank on purpose (a split with one side
        // cleared) is not this command's to fold away, and a blankness
        // sweep would take it along with the emptied one.
        let pre_blank: Vec<bool> = self
            .editor_layout
            .inactive_groups()
            .map(|g| g.is_blank_initial())
            .collect();
        let mut doomed = Vec::new();
        for (i, group) in self.editor_layout.inactive_groups_mut().enumerate() {
            let mut closed_here = false;
            while let Some(idx) = group.find_any_tab_with_path(path) {
                group.close_tab(idx);
                closed_here = true;
            }
            if closed_here && group.is_blank_initial() && !pre_blank[i] {
                doomed.push(i);
            }
        }
        if !doomed.is_empty() {
            self.editor_layout.prune_inactive_at(&doomed);
        }
    }

    /// Close the tabs showing a file under `dir` in every group, except
    /// ones with unsaved edits, for a folder that has left the disk. A
    /// group this empties is pruned like `close_tabs_with_path_everywhere`
    /// prunes one: it held a tab under `dir`, so it was not blank before.
    pub(super) fn close_clean_tabs_under_everywhere(&mut self, dir: &Path) {
        self.editor.close_clean_tabs_under(dir);
        let mut doomed = Vec::new();
        for (i, group) in self.editor_layout.inactive_groups_mut().enumerate() {
            let touched = group
                .editors
                .iter()
                .any(|e| e.path.as_deref().is_some_and(|p| p.starts_with(dir)));
            group.close_clean_tabs_under(dir);
            if touched && group.is_blank_initial() {
                doomed.push(i);
            }
        }
        if !doomed.is_empty() {
            self.editor_layout.prune_inactive_at(&doomed);
        }
        self.sync_open_file_poll_mtime();
    }

    /// Tab context "Close Others" and View: Close Other Editors in Group
    /// (#852): close every tab of the focused group but `keep_idx` and the
    /// pinned ones, recording each for Reopen Closed Editor. Unsaved tabs
    /// stay open, and the status line counts them.
    pub(super) fn close_other_tabs(&mut self, keep_idx: usize) {
        let unsaved = self.record_closed_tabs_where(|i, _| i != keep_idx);
        let removed = self.editor.close_others(keep_idx);
        if removed == 0 && unsaved == 0 {
            self.status = String::from("No other tabs to close");
            return;
        }
        self.sync_open_file_poll_mtime();
        let closed = if removed == 1 {
            String::from("Closed 1 other tab")
        } else {
            format!("Closed {removed} other tabs")
        };
        self.status = bulk_close_status(closed, removed, unsaved);
        self.poke_cursor();
    }

    /// Cmd+K →, tab context "Close to the Right" and View: Close Editors to
    /// the Right in Group (#852): close the focused group's unpinned tabs
    /// right of `from_idx`, recording each for Reopen Closed Editor. Unsaved
    /// tabs stay open, and the status line counts them.
    pub(super) fn close_tabs_to_right(&mut self, from_idx: usize) {
        let unsaved = self.record_closed_tabs_where(|i, _| i > from_idx);
        let removed = self.editor.close_to_right(from_idx);
        if removed == 0 && unsaved == 0 {
            self.status = String::from("No tabs to the right to close");
            return;
        }
        self.sync_open_file_poll_mtime();
        let closed = if removed == 1 {
            String::from("Closed 1 tab to the right")
        } else {
            format!("Closed {removed} tabs to the right")
        };
        self.status = bulk_close_status(closed, removed, unsaved);
        self.poke_cursor();
    }

    /// Cmd+K ⇧P, tab context "Keep Open" and View: Keep Editor (#852):
    /// promote the preview tab at `idx` so the next single-click open does
    /// not replace it.
    pub(super) fn keep_tab_open(&mut self, idx: usize) {
        if self.editor.is_blank_initial() {
            self.status = String::from("No editor is open");
        } else if self.editor.keep_open(idx) {
            self.status = String::from("Kept tab open");
            self.poke_cursor();
        } else {
            self.status = String::from("Tab is already kept open");
        }
    }

    /// Cmd+K P and tab context "Pin" / "Unpin": flip the pin of the tab at
    /// `idx`. The welcome screen's placeholder is not a tab to pin.
    pub(super) fn toggle_tab_pin(&mut self, idx: usize) {
        if self.editor.is_blank_initial() {
            self.status = String::from("No editor is open");
            return;
        }
        let pinned = self.editor.toggle_pin(idx);
        self.status = String::from(if pinned { "Pinned tab" } else { "Unpinned tab" });
        self.poke_cursor();
    }

    /// View: Pin Editor (`pin`) / View: Unpin Editor (#852): the active
    /// tab's pin, through the tab menu's toggle. Each is a no-op on a tab
    /// already in that state, so a key bound to one never does the other.
    pub(super) fn set_active_tab_pinned(&mut self, pin: bool) {
        let idx = self.editor.active_index();
        if !self.editor.is_blank_initial() && self.editor.is_pinned(idx) == pin {
            self.status = String::from(if pin {
                "Tab is already pinned"
            } else {
                "Tab is not pinned"
            });
            return;
        }
        self.toggle_tab_pin(idx);
    }

    /// Cmd+K W / tab context "Close All" / View: Close All Editors (#852):
    /// close every tab in EVERY editor group, not just the focused one,
    /// collapsing any split back to a single blank pane. A side-by-side
    /// layout (e.g. `cgr duplicates` diffs) would otherwise only empty the
    /// clicked group and look like a single close once the blank group
    /// collapsed away.
    ///
    /// Tabs with unsaved changes stay open, in whichever group holds them
    /// (#862): a sweep must not decide to throw edits away, and Cmd+W on one
    /// of them asks. Pinned tabs stay open too (#941), as Close Others and
    /// Close to the Right leave them. Only the groups left with nothing fold
    /// away then. The closed tabs are recorded for Reopen Closed Editor, the
    /// focused group's last so they are the first to come back.
    pub(super) fn close_all_tabs(&mut self) {
        let groups = self.editor_layout.inactive_groups();
        let (records, unsaved) =
            bulk_close_records(groups.flat_map(|g| &g.editors).chain(&self.editor.editors));
        self.push_closed_tabs(records);
        let mut removed = self.editor.close_all();
        if self.editor_layout.is_split() {
            let mut emptied = Vec::new();
            let mut any_kept = !self.editor.is_blank_initial();
            for (g, group) in self.editor_layout.inactive_groups_mut().enumerate() {
                removed += group.close_all();
                if group.is_blank_initial() {
                    emptied.push(g);
                } else {
                    any_kept = true;
                }
            }
            if !any_kept {
                self.editor_layout = editor_layout::EditorLayout::single();
            } else {
                self.editor_layout.prune_inactive_at(&emptied);
                self.collapse_split_if_empty();
            }
            self.editor_seams.clear();
            self.disable_editor_image(1);
            self.sync_focus_flags();
        }
        self.sync_open_file_poll_mtime();
        let closed = if removed == 1 {
            String::from("Closed 1 tab")
        } else {
            format!("Closed {removed} tabs")
        };
        self.status = bulk_close_status(closed, removed, unsaved);
        self.poke_cursor();
    }

    /// Close every saved (non-dirty) editor tab, keeping any with unsaved
    /// changes and every pinned one (#941), and record the closed tabs for
    /// Reopen Closed Editor. Shared by the tab context menu, the `Cmd+K U`
    /// chord and View: Close Saved Editors in Group (#852) so every surface
    /// produces the same status line and split-collapse behavior.
    pub(super) fn close_saved_tabs(&mut self) {
        self.record_closed_tabs_where(|_, _| true);
        let removed = self.editor.close_saved();
        if removed == 0 {
            self.status = String::from("No saved tabs to close");
            return;
        }
        self.sync_open_file_poll_mtime();
        self.status = if removed == 1 {
            String::from("Closed 1 saved tab")
        } else {
            format!("Closed {removed} saved tabs")
        };
        self.poke_cursor();
        self.collapse_split_if_empty();
    }

    /// Copy `path` (an editor tab's absolute path) to the system clipboard via
    /// the NSPasteboard-first wrapper (OSC 52 fallback on a remote host). Shared
    /// by the tab context menu and the `Cmd+Opt+C` chord. Matches VS Code's
    /// "Copy Path".
    pub(super) fn copy_path_to_clipboard(&mut self, path: PathBuf) {
        let text = path.display().to_string();
        let landing = self.copy_where_pasted(&text);
        self.status = format!("Copied path: {text}{}", landing.note());
    }

    /// VS Code "Copy Relative Path": copy `path` relative to the workspace
    /// root. Files outside the root (rare: a tab opened by absolute path above
    /// the workspace) fall back to the absolute path so the clipboard never
    /// holds a misleading `../../` walk or an empty string.
    pub(super) fn copy_relative_path_to_clipboard(&mut self, path: PathBuf) {
        let text = relative_clipboard_text(&path, self.workspace_root());
        let landing = self.copy_where_pasted(&text);
        self.status = format!("Copied relative path: {text}{}", landing.note());
    }

    /// File: Reveal Active File in Explorer View (`Cmd+K E`): the active
    /// tab's file, selected in the Explorer.
    pub(super) fn reveal_active_file_in_explorer(&mut self) {
        if let Some(path) = self.editor.path.clone() {
            self.reveal_in_explorer(path);
        } else {
            self.status = String::from("Reveal in Explorer View: no active file");
        }
    }

    /// Reveal `path` (an editor tab's file) in the Explorer tree: switch the
    /// side bar to the Explorer, expand the file's parent folders, select its
    /// row, and focus the tree. Shared by the tab context menu and the
    /// `Cmd+K E` chord. Matches VS Code's "Reveal in Explorer View".
    pub(super) fn reveal_in_explorer(&mut self, path: PathBuf) {
        self.set_sidebar_view(SidebarView::Explorer);
        // A sidebar-targeting command must work even with the sidebar hidden
        // (#260). `set_sidebar_view` above already forced it open and focuses
        // the tree, so nothing further is required here.
        let found = self.tree.reveal_path(&path);
        self.focus_pane(Pane::Tree);
        self.status = if found {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            format!("Revealed {name} in Explorer")
        } else {
            String::from("File is outside the workspace")
        };
    }

    /// Reveal `path` in macOS Finder by spawning `open -R`, which selects the
    /// entry inside its containing folder. Fire-and-forget: `open` hands off to
    /// LaunchServices and returns immediately, so the TUI never blocks. Only
    /// reachable on a local macOS session because `maybe_add_reveal_in_finder`
    /// withholds the menu entry everywhere else; the `cfg` guard keeps the call
    /// out of non-macOS builds entirely.
    pub(super) fn reveal_in_finder(&mut self, path: PathBuf) {
        #[cfg(target_os = "macos")]
        {
            self.status = if std::process::Command::new("open")
                .arg("-R")
                .arg(&path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
            {
                String::from("Revealed in Finder")
            } else {
                String::from("Could not reveal in Finder")
            };
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
        }
    }
}

/// The host terminal croft is running inside, for spawning a new window of the
/// SAME terminal (Move/Copy into New Window). Detected from `$TERM_PROGRAM` /
/// `$TERM`, the same signals croft's inline-image detection keys off.
#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum HostTerminal {
    Ghostty,
    ITerm2,
    AppleTerminal,
    /// A terminal we don't know how to drive; carries its reported name so the
    /// status line can say which one rather than failing silently.
    Unknown(String),
}

#[cfg(target_os = "macos")]
pub(super) fn detect_host_terminal(term_program: Option<&str>, term: Option<&str>) -> HostTerminal {
    match term_program {
        Some("ghostty") => HostTerminal::Ghostty,
        Some("iTerm.app") => HostTerminal::ITerm2,
        Some("Apple_Terminal") => HostTerminal::AppleTerminal,
        Some(other) if !other.is_empty() => HostTerminal::Unknown(other.to_string()),
        // Some setups only export TERM (e.g. `xterm-ghostty`).
        _ if term.is_some_and(|t| t.contains("ghostty")) => HostTerminal::Ghostty,
        _ => HostTerminal::Unknown(term.unwrap_or("unknown").to_string()),
    }
}

/// Spawn a brand-new OS window of the terminal croft is running in, opening
/// `croft <root> --open-file <file> --zen` — VS Code's "Move/Copy into New
/// Window", realised as a genuine separate window so the originating croft is
/// never disturbed. The new window opens in `--zen` (Explorer + terminal hidden)
/// so it focuses on just the file. The croft binary is referenced by its
/// resolved ABSOLUTE path: a GUI-launched croft inherits a stripped launchd
/// PATH, so the bare name would not resolve in the new window's shell (see the
/// project memory on PATH stripping). macOS only.
#[cfg(target_os = "macos")]
pub(super) fn spawn_new_window(
    root: &std::path::Path,
    file: &std::path::Path,
) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let host = detect_host_terminal(
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    );
    match host {
        // Ghostty on macOS has no IPC (`+new-window` is Linux/GTK-only), so a
        // new window must come from `open -na` (a fresh instance). A fresh
        // instance would ALSO open its default window → two windows
        // (ghostty-org/ghostty#8669; verified empirically). `--initial-window=false`
        // suppresses that default (verified: it adds zero windows), so `-e`'s
        // command window is the only one. `--quit-after-last-window-closed` makes
        // the spawned instance exit once that window closes. `-e` is exec'd
        // directly (no shell), so the croft argv is passed token-by-token.
        HostTerminal::Ghostty => {
            std::process::Command::new("open")
                .args(ghostty_open_argv(&exe, root, file))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            Ok(())
        }
        // iTerm2 and Terminal have no `open --args` command path; both are driven
        // by AppleScript, and both run the `command` / `do script` string through
        // a shell, so the croft invocation is shell-quoted.
        HostTerminal::ITerm2 => {
            std::process::Command::new("osascript")
                .arg("-e")
                .arg(iterm2_new_window_script(&exe, root, file))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            Ok(())
        }
        HostTerminal::AppleTerminal => {
            std::process::Command::new("osascript")
                .arg("-e")
                .arg(apple_terminal_new_window_script(&exe, root, file))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            Ok(())
        }
        HostTerminal::Unknown(name) => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("unsupported terminal '{name}'"),
        )),
    }
}

/// `open` argv for a new Ghostty window running croft, focused on the file
/// (`--zen`). `--initial-window=false` stops the fresh instance opening a stray
/// default window beside the `-e` one. Split out so the shape is unit-testable
/// without launching a window.
#[cfg(target_os = "macos")]
pub(super) fn ghostty_open_argv(
    exe: &std::path::Path,
    root: &std::path::Path,
    file: &std::path::Path,
) -> Vec<std::ffi::OsString> {
    use std::ffi::OsString;
    vec![
        OsString::from("-na"),
        OsString::from("Ghostty"),
        OsString::from("--args"),
        OsString::from("--initial-window=false"),
        OsString::from("--quit-after-last-window-closed=true"),
        OsString::from("-e"),
        exe.as_os_str().to_os_string(),
        root.as_os_str().to_os_string(),
        OsString::from("--open-file"),
        file.as_os_str().to_os_string(),
        OsString::from("--zen"),
    ]
}

/// `croft <root> --open-file <file> --zen` as a single shell command line, with
/// every path single-quoted so spaces and shell metacharacters survive (iTerm2
/// `command` and Terminal `do script` both run it via a shell). `--zen` opens
/// the new window focused on the file (no sidebar/terminal).
#[cfg(target_os = "macos")]
pub(super) fn new_window_shell_command(
    exe: &std::path::Path,
    root: &std::path::Path,
    file: &std::path::Path,
) -> String {
    fn shq(p: &std::path::Path) -> String {
        format!("'{}'", p.to_string_lossy().replace('\'', r"'\''"))
    }
    format!("{} {} --open-file {} --zen", shq(exe), shq(root), shq(file))
}

/// Escape a Rust string for embedding inside an AppleScript double-quoted
/// string literal (backslash and double-quote are the only specials).
#[cfg(target_os = "macos")]
pub(super) fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// AppleScript to open a new iTerm2 window running croft. iTerm2's `command`
/// argument overrides the profile's default `login`, running our shell command.
#[cfg(target_os = "macos")]
pub(super) fn iterm2_new_window_script(
    exe: &std::path::Path,
    root: &std::path::Path,
    file: &std::path::Path,
) -> String {
    let cmd = applescript_escape(&new_window_shell_command(exe, root, file));
    format!(r#"tell application "iTerm" to create window with default profile command "{cmd}""#)
}

/// AppleScript to open a new Terminal.app window running croft. `do script`
/// with no target opens a fresh window and runs the command there.
#[cfg(target_os = "macos")]
pub(super) fn apple_terminal_new_window_script(
    exe: &std::path::Path,
    root: &std::path::Path,
    file: &std::path::Path,
) -> String {
    let cmd = applescript_escape(&new_window_shell_command(exe, root, file));
    format!(r#"tell application "Terminal" to do script "{cmd}""#)
}

/// Non-macOS builds have no supported window-spawn path yet (each terminal's
/// CLI differs per platform); the caller surfaces the error as a status line.
#[cfg(not(target_os = "macos"))]
pub(super) fn spawn_new_window(
    _root: &std::path::Path,
    _file: &std::path::Path,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "opening a new window is only supported on macOS",
    ))
}

/// Whether `ed` shows 1-based `line`: every line of an ordinary tab, only
/// its symbol's lines on a symbol tab (#369), where a caret elsewhere would
/// be snapped back to the symbol's edge.
pub(super) fn bookmark_line_shown(ed: &Editor, line: usize) -> bool {
    ed.symbol_clip()
        .is_none_or(|(first, end)| (first..end).contains(&line.saturating_sub(1)))
}
