//! The editor's tabs: opening, closing, pinning and moving them, previews and diff tabs,
//! the tab strip and its labels, and the breadcrumbs.

use super::*;

/// Multi-buffer editor: a stack of `Editor` instances with a single active
/// one, plus a 1-row clickable tab strip rendered above the active editor.
/// `Deref`/`DerefMut` aim at the active editor so existing call sites that
/// were written for a single `Editor` continue to work without rewrites.
/// One segment of the breadcrumb bar (VS Code's editor breadcrumbs): a path
/// folder, the file name, or an enclosing symbol. `target` is the caret
/// position a click jumps to; `None` for the informational path/file crumbs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub target: Option<(u32, u32)>,
    /// The glyph and colour a `surfaces` theme draws before the label: the
    /// file's type icon on the file crumb, the symbol kind's on a symbol
    /// crumb, none on a folder. Not part of the crumb's hit range.
    pub icon: Option<(char, Color)>,
}

/// A rendered breadcrumb crumb's hit-test span: `(x_start, width, jump target)`.
pub(super) type BreadcrumbRange = (u16, u16, Option<(u32, u32)>);

impl EditorTabs {
    pub fn new() -> Self {
        Self {
            last_body: Rect::default(),
            editors: vec![Editor::new()],
            active: 0,
            breadcrumbs: Vec::new(),
            breadcrumb_ranges: Vec::new(),
            breadcrumb_y: None,
            cell_images: false,
            tab_screen_ranges: Vec::new(),
            tab_close_x: Vec::new(),
            tab_strip_y: 0,
            tab_strip: Box::default(),
            last_full_area: Rect::default(),
            search_highlight_term: None,
            search_highlight_opts: crate::widgets::search::SearchOpts::default(),
            hover_pointer: None,
            released_bookmarks: Vec::new(),
        }
    }

    pub fn tab_count(&self) -> usize {
        self.editors.len()
    }

    /// Set (or clear) the search-match highlight term + opts for every
    /// open tab, so opening another file from search keeps the same query
    /// lit, clearing the search box wipes the highlights, and toggling a
    /// search mode (case / whole-word / regex) re-paints the file with
    /// the matching rule. Also persists term + opts so editors created
    /// after this call (e.g. after a close + reopen) inherit them.
    pub fn set_search_highlight(
        &mut self,
        term: Option<String>,
        opts: crate::widgets::search::SearchOpts,
    ) {
        let normalised = term.filter(|s| !s.is_empty());
        let cleared = normalised.is_none();
        self.search_highlight_term = normalised.clone();
        self.search_highlight_opts = opts;
        for ed in &mut self.editors {
            ed.search_highlight = normalised.clone();
            ed.search_highlight_opts = opts;
            if cleared {
                ed.active_search_match = None;
            }
        }
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The group's active tab, for callers that must act on the group without
    /// hoisting it into App::editor (e.g. a wheel scroll over an inactive
    /// split pane). editors is never empty, so this cannot fail.
    pub fn active_editor_mut(&mut self) -> &mut Editor {
        let active = self.active.min(self.editors.len().saturating_sub(1));
        &mut self.editors[active]
    }

    /// Wheel-scroll this group's active tab by notches (positive = down),
    /// mirroring what App does for the focused tab: a diff scrolls its own
    /// pane, a PDF preview steps pages (clamped to the document), a hex view
    /// scrolls by data rows, anything else scrolls text by notches lines.
    /// Keeping the modes here means a background split group behaves like the
    /// focused one instead of scrolling a buffer that isn't on screen.
    pub fn wheel_scroll_active(&mut self, notches: i32) {
        let ed = self.active_editor_mut();
        if let Some(diff) = ed.diff.as_mut() {
            if notches >= 0 {
                diff.scroll_down_by(notches.unsigned_abs() as usize);
            } else {
                diff.scroll_up_by(notches.unsigned_abs() as usize);
            }
            return;
        }
        if let Some(page) = ed.pdf_page() {
            // One page per wheel event, like the focused preview: the
            // caller's notches are text lines (3 a tick), not pages.
            let target = if notches >= 0 {
                page.saturating_add(1)
            } else {
                page.saturating_sub(1).max(1)
            };
            ed.set_pdf_page(target);
            return;
        }
        if let Some(view) = ed.hex.as_mut() {
            let rows = (view.layout.data_rows as usize).max(1);
            view.scroll_by(notches as i64, rows);
            return;
        }
        if notches >= 0 {
            ed.scroll_down(notches.unsigned_abs() as usize);
        } else {
            ed.scroll_up(notches.unsigned_abs() as usize);
        }
    }

    /// The tab strip label for the tab at `idx` (file name, diff pair, or
    /// "untitled"), for status messages. Empty string for an out-of-range idx.
    pub fn tab_display_label(&self, idx: usize) -> String {
        self.editors.get(idx).map(tab_label).unwrap_or_default()
    }

    /// The on-disk path backing the tab at `idx`, if any. `None` for an
    /// out-of-range index or a blank/untitled buffer. Used to seed the tab
    /// context-menu's path-bearing actions (Reveal in Finder, Copy Path).
    pub fn tab_path(&self, idx: usize) -> Option<PathBuf> {
        self.editors.get(idx).and_then(|e| e.path.clone())
    }

    /// The strip's display titles, disambiguated (#167); index-aligned
    /// with `iter_tabs` so projections (OPEN EDITORS) match the strip.
    pub fn tab_display_labels(&self) -> Vec<String> {
        disambiguated_tab_labels(&self.editors)
    }

    #[cfg(test)]
    pub fn tab_strip_y_for_test(&self) -> u16 {
        self.tab_strip_y
    }

    pub fn iter_tabs(&self) -> impl Iterator<Item = &Editor> {
        self.editors.iter()
    }

    /// Close the undo group of every tab that still has one open (#934): a
    /// vim change entered on one tab can be left with `Esc` on another, and
    /// an edit back on its tab that skips vim, such as a paste, would
    /// otherwise fold into the change. A tab with no group open is left as
    /// it is.
    pub fn end_open_undo_groups(&mut self) {
        for editor in &mut self.editors {
            if editor.undo_group.is_some() {
                editor.end_undo_group();
            }
        }
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

    /// [`Editor::trim_auto_indent`] over every tab (#956): the active one
    /// keeps the line its caret is still on when `focused_group`, and every
    /// other tab has been switched away from, so its line was left.
    pub fn trim_auto_indents(&mut self, focused_group: bool) {
        let active = self.active;
        for (i, editor) in self.editors.iter_mut().enumerate() {
            editor.trim_auto_indent(!(focused_group && i == active));
        }
    }

    /// Sweep EVERY open tab (not just the active one) for external on-disk
    /// changes. Clean tabs are silently reloaded; dirty tabs are flagged as
    /// conflicts. This is the core of the FS-sync invariant: a file open in
    /// a background tab must reflect disk reality just like the focused one.
    /// `skip` exempts paths from the sweep: a collab guest never reloads
    /// shared files from disk — its replica is authoritative, and the
    /// owner's own reload reaches it as ops (docs/MULTIPLAYER.md, Phase D).
    pub fn reload_externally_changed_tabs(
        &mut self,
        skip: &dyn Fn(&Path) -> bool,
    ) -> ExternalReloadReport {
        let mut report = ExternalReloadReport::default();
        for ed in &mut self.editors {
            // Diff/image/sheet views don't carry an editable text buffer to
            // reload, and a path-less blank tab has nothing to sync.
            if ed.path.is_none() || ed.diff.is_some() {
                continue;
            }
            if ed.path.as_deref().is_some_and(skip) {
                continue;
            }
            let path = ed.path.clone();
            match ed.reload_or_flag_conflict() {
                ExternalChange::Reloaded => {
                    if let Some(p) = path {
                        report.reloaded.push(p);
                    }
                }
                ExternalChange::Conflict => {
                    if let Some(p) = path {
                        report.conflicts.push(p);
                    }
                }
                ExternalChange::ReloadFailed => {
                    if let Some(p) = path {
                        report.failed.push(p);
                    }
                }
                ExternalChange::Deleted => {
                    if let Some(p) = path {
                        report.deleted.push(p);
                    }
                }
                ExternalChange::Unchanged => {}
            }
        }
        report
    }

    /// Discard unsaved edits and reload the given paths from disk — the
    /// "Reload" resolution of an external-change conflict. Returns the paths
    /// actually reverted (a tab must still be open on each).
    pub fn revert_paths_to_disk(&mut self, paths: &[PathBuf]) -> Vec<PathBuf> {
        let mut reverted = Vec::new();
        for ed in &mut self.editors {
            let Some(p) = ed.path.clone() else { continue };
            if paths.iter().any(|q| q == &p) && ed.revert_to_disk().is_ok() {
                reverted.push(p);
            }
        }
        reverted
    }

    /// If any tab currently points at `old`, repoint it to `new`. The on-
    /// disk file has already been moved; this only updates the in-memory
    /// path so subsequent saves and the tab label track the new name.
    pub fn rename_open_path(&mut self, old: &Path, new: &Path) {
        for e in &mut self.editors {
            // Under a moved folder too: a tab of `dir/a.rs` follows `dir` to
            // its new place, or its next save recreates the old path.
            let moved = e
                .path
                .as_deref()
                .and_then(|p| p.strip_prefix(old).ok())
                .map(|rest| {
                    if rest.as_os_str().is_empty() {
                        new.to_path_buf()
                    } else {
                        new.join(rest)
                    }
                });
            if let Some(moved) = moved {
                e.path = Some(moved);
                // Re-anchor the disk stamp to the new path so the rename
                // isn't mistaken for an external content change on the next
                // FS-sync sweep.
                e.mark_synced_with_disk();
            }
        }
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

    /// Insert a pinned tab for `path` without reading it (None: untitled),
    /// reusing the blank initial tab. The caller fills the buffer: this is
    /// for text whose only copy is in memory, like a session handoff's
    /// unsaved edits to a file that can no longer be opened.
    pub fn open_unreadable_tab(&mut self, path: Option<PathBuf>) -> &mut Editor {
        if !self.is_blank_initial() {
            let mut e = Editor::new();
            e.focused = self.editors[self.active].focused;
            let pos = self.active + 1;
            self.editors.insert(pos, e);
            self.editors[self.active].focused = false;
            self.active = pos;
        }
        let e = &mut self.editors[self.active];
        e.path = path;
        e.preview = false;
        e
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
        self.close_tab(self.active)
    }

    /// Close every tab showing `path` or a file under it (a deleted file or
    /// folder), except tabs with unsaved edits: those stay open, dirty, so
    /// the edits can still be saved (recreating the file) or discarded. A
    /// sheet cell still being typed into counts as an unsaved edit.
    /// Returns how many dirty tabs were kept.
    pub fn close_clean_tabs_under(&mut self, path: &Path) -> usize {
        let mut kept = 0;
        let mut i = self.editors.len();
        while i > 0 {
            i -= 1;
            let e = &self.editors[i];
            if !e.path.as_deref().is_some_and(|p| p.starts_with(path)) {
                continue;
            }
            if e.dirty || e.sheet_pending_cell().is_some() {
                kept += 1;
            } else {
                self.close_tab(i);
            }
        }
        kept
    }

    /// Close the tab at `idx`. When more than one tab is open the tab is
    /// removed and `self.active` is shifted so it still points at a valid
    /// tab. When this is the last remaining tab it is reset to the blank
    /// just-launched state instead of being removed (so the editor pane
    /// always has at least one buffer to render). Returns false only on an
    /// out-of-range index.
    pub fn close_tab(&mut self, idx: usize) -> bool {
        if idx >= self.editors.len() {
            return false;
        }
        if self.editors.len() == 1 {
            let was_focused = self.editors[0].focused;
            let mut fresh = Editor::new();
            fresh.focused = was_focused;
            let closed = std::mem::replace(&mut self.editors[0], fresh);
            self.release_bookmarks(&closed);
            self.active = 0;
            return true;
        }
        let closed = self.editors.remove(idx);
        self.release_bookmarks(&closed);
        if self.active > idx {
            self.active -= 1;
        } else if self.active >= self.editors.len() {
            self.active = self.editors.len() - 1;
        }
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == self.active;
        }
        true
    }

    /// Keep a closing tab's bookmarks for the App, which holds a file's
    /// marks once no tab does.
    pub(super) fn release_bookmarks(&mut self, closed: &Editor) {
        if !closed.bookmarks.is_empty() {
            self.released_bookmarks.push(closed.bookmarks_on_disk());
        }
    }

    /// The bookmarks of the tabs closed since the last call, oldest first.
    pub fn take_released_bookmarks(
        &mut self,
    ) -> Vec<std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>> {
        std::mem::take(&mut self.released_bookmarks)
    }

    /// Close every tab whose index ≠ `keep_idx`, except pinned tabs, which
    /// always survive (VS Code "Close Others" never closes a pinned tab), and
    /// tabs with unsaved changes, which a sweep never drops (#862). The
    /// kept tab stays active. Returns how many tabs were actually removed (0
    /// when `keep_idx` is out of range or nothing else is closeable). Mirrors
    /// VS Code's "Close Others" context-menu action.
    pub fn close_others(&mut self, keep_idx: usize) -> usize {
        if keep_idx >= self.editors.len() || self.editors.len() <= 1 {
            return 0;
        }
        let before = self.editors.len();
        let mut new_active = 0;
        let mut kept: Vec<Editor> = Vec::with_capacity(before);
        for (i, ed) in std::mem::take(&mut self.editors).into_iter().enumerate() {
            if i == keep_idx {
                new_active = kept.len();
                kept.push(ed);
            } else if ed.pinned || ed.dirty {
                kept.push(ed);
            } else {
                self.release_bookmarks(&ed);
            }
        }
        let removed = before - kept.len();
        self.editors = kept;
        self.active = new_active;
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == self.active;
        }
        removed
    }

    /// Close every tab whose index > `from_idx`, except pinned tabs, which
    /// always survive (VS Code "Close to the Right" never closes a pinned
    /// tab), and tabs with unsaved changes (#862). The tab at `from_idx`
    /// stays active; tabs to the left are untouched. Returns the number of
    /// tabs removed. Matches VS Code's "Close to the Right".
    pub fn close_to_right(&mut self, from_idx: usize) -> usize {
        if from_idx >= self.editors.len() {
            return 0;
        }
        let before = self.editors.len();
        let old_active = self.active;
        let mut new_active: Option<usize> = None;
        let mut pivot_pos = 0;
        let mut kept: Vec<Editor> = Vec::with_capacity(before);
        for (i, ed) in std::mem::take(&mut self.editors).into_iter().enumerate() {
            if i <= from_idx || ed.pinned || ed.dirty {
                if i == from_idx {
                    pivot_pos = kept.len();
                }
                if i == old_active {
                    new_active = Some(kept.len());
                }
                kept.push(ed);
            } else {
                self.release_bookmarks(&ed);
            }
        }
        let removed = before - kept.len();
        self.editors = kept;
        // Keep the previously-active tab active when it survived; otherwise
        // fall back to the pivot tab's new position.
        self.active = new_active.unwrap_or(pivot_pos);
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == self.active;
        }
        removed
    }

    /// Close every tab, resetting the editor pane to the single blank
    /// just-launched state — mirrors `close_tab` on the last remaining
    /// tab. Returns how many tabs were collapsed away (always ≥ 1 when
    /// the editor had any content). Matches VS Code's "Close All": pinned
    /// tabs stay open (#941). Tabs with unsaved changes stay open too
    /// (#862): VS Code asks about each, croft leaves them for a Cmd+W that
    /// asks. That makes it the same sweep as [`Self::close_saved`].
    pub fn close_all(&mut self) -> usize {
        self.close_saved()
    }

    /// Close every saved (non-dirty) tab, keeping any with unsaved changes
    /// and every pinned tab (#941). When nothing is kept the pane resets to
    /// a single blank buffer, mirroring `close_all`; otherwise the kept tabs
    /// stay in order, the active one staying active when it is among them
    /// and the first becoming active when not. Returns how many tabs were
    /// removed (0 when every open tab is dirty or pinned). Matches VS
    /// Code's "Close Saved" (`workbench.action.closeUnmodifiedEditors`).
    pub fn close_saved(&mut self) -> usize {
        let before = self.editors.len();
        let old_active = self.active;
        let was_focused = self.editors[old_active].focused;
        let mut new_active = 0;
        let mut kept: Vec<Editor> = Vec::with_capacity(before);
        for (i, ed) in std::mem::take(&mut self.editors).into_iter().enumerate() {
            if ed.dirty || ed.pinned {
                if i == old_active {
                    new_active = kept.len();
                }
                kept.push(ed);
            } else {
                self.release_bookmarks(&ed);
            }
        }
        let removed = before - kept.len();
        if kept.is_empty() {
            let mut fresh = Editor::new();
            fresh.focused = was_focused;
            kept.push(fresh);
        }
        self.editors = kept;
        self.active = new_active;
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == self.active;
        }
        removed
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

    /// Whether `(col, row)` is on the tab strip row of this group, tabs or the
    /// gap after them.
    pub fn tab_strip_contains(&self, col: u16, row: u16) -> bool {
        let r = self.last_full_area;
        row == self.tab_strip_y && col >= r.x && col < r.x + r.width
    }

    /// Reserve `cells` at the strip's right end for what the app paints there.
    pub fn set_tab_reserved_right(&mut self, cells: u16) {
        self.tab_strip.reserved_right = cells;
    }

    /// Whether `(col, row)` is on the "all open tabs" button.
    pub fn tab_list_button_at(&self, col: u16, row: u16) -> bool {
        let r = self.tab_strip.list_button;
        r.width > 0 && row == r.y && col >= r.x && col < r.x + r.width
    }

    /// A wheel notch over the strip: [`Self::scroll_tabs`], but at most one tab
    /// per [`TAB_WHEEL_GAP`] in a direction. A touchpad sends dozens of events
    /// per swipe, which would otherwise fling the strip to its end.
    pub fn scroll_tabs_wheel(&mut self, delta: isize) {
        let now = std::time::Instant::now();
        if let Some((at, dir)) = self.tab_strip.wheel_at
            && dir == delta
            && now.duration_since(at) < TAB_WHEEL_GAP
        {
            return;
        }
        self.tab_strip.wheel_at = Some((now, delta));
        self.scroll_tabs(delta);
    }

    /// Scroll the tab strip by `delta` tabs (the wheel over it). The render
    /// clamps the result to what still fills the strip.
    pub fn scroll_tabs(&mut self, delta: isize) {
        let last = self.editors.len().saturating_sub(1);
        self.tab_strip.scroll = self.tab_strip.scroll.saturating_add_signed(delta).min(last);
        self.tab_strip.revealed = Some((self.active, self.editors.len()));
    }

    /// Full filesystem path for a tab, used as the hover tooltip that
    /// disambiguates same-named files (the tab label only shows the bare
    /// `file_name()`). Returns `None` for an unsaved "untitled" buffer
    /// since there is nothing to disambiguate. Diff tabs report both
    /// sides so the viewer can tell which revisions are being compared.
    pub fn tab_full_path(&self, idx: usize) -> Option<String> {
        let e = self.editors.get(idx)?;
        if let Some(diff) = e.diff.as_ref() {
            let l = diff.left_path.to_string_lossy().into_owned();
            let r_is_real = diff.right_path != Path::new("/dev/null")
                && !diff.right_path.as_os_str().is_empty();
            if r_is_real {
                let r = diff.right_path.to_string_lossy().into_owned();
                return Some(format!("{l} \u{2194} {r}"));
            }
            return Some(l);
        }
        e.path.as_ref().map(|p| p.to_string_lossy().into_owned())
    }

    pub fn tab_screen_x(&self, idx: usize) -> Option<(u16, u16)> {
        self.tab_screen_ranges.get(idx).copied()
    }

    pub fn close_screen_x(&self, idx: usize) -> Option<u16> {
        self.tab_close_x.get(idx).copied().filter(|&x| x != 0)
    }

    /// Map a mouse cell to the tab whose close `\u{2715}` glyph occupies it,
    /// or `None` if the click missed every close button. Used by the App's
    /// mouse handler to short-circuit ahead of `tab_at` so a click on the X
    /// closes the tab instead of selecting it.
    pub fn close_at(&self, col: u16, row: u16) -> Option<usize> {
        if row != self.tab_strip_y {
            return None;
        }
        self.tab_close_x.iter().position(|&x| x != 0 && x == col)
    }

    /// Index of the first tab whose `path` matches `target` either by
    /// literal equality or by canonicalised equality (so symlink + relative
    /// path aliases dedupe to the same tab). Returns `None` if no tab is
    /// currently holding that file.
    ///
    /// A symbol tab (#369) is not "the file's tab": opening, previewing or
    /// selecting a file must land on a tab showing all of it, so this skips
    /// them. [`Self::find_any_tab_with_path`] includes them.
    pub fn find_tab_with_path(&self, target: &Path) -> Option<usize> {
        self.find_tab_matching(target, |e| e.symbol_view.is_none())
    }

    /// Like [`Self::find_tab_with_path`], symbol tabs included: for closing
    /// every tab of a file.
    pub fn find_any_tab_with_path(&self, target: &Path) -> Option<usize> {
        self.find_tab_matching(target, |_| true)
    }

    /// The tab whose buffer holds `target`'s text: its own tab first, else a
    /// symbol tab over it. A diff tab carries the file's `path` but shows a
    /// comparison, so an edit sent to it changes nothing (#1113); it never
    /// counts.
    pub fn find_buffer_tab(&self, target: &Path) -> Option<usize> {
        self.find_tab_matching(target, |e| e.diff.is_none() && e.symbol_view.is_none())
            .or_else(|| self.find_tab_matching(target, |e| e.diff.is_none()))
    }

    /// If `path` is open in a tab, apply LSP rename `edits` to that buffer
    /// in-memory (one undo step, marked dirty) and return the count applied.
    /// `None` when no tab holds the file, so the caller can edit it on disk.
    pub fn apply_rename_to_open_tab(
        &mut self,
        path: &Path,
        edits: &[TextSpanEdit],
    ) -> Option<usize> {
        let idx = self.find_buffer_tab(path)?;
        Some(self.editors[idx].apply_span_edits(edits))
    }

    /// Every open tab's file and `edit_seq`: the buffers as they stand when
    /// a language server is asked for a workspace edit (#1151).
    pub fn edit_seqs(&self) -> Vec<(PathBuf, u64)> {
        self.editors
            .iter()
            .filter_map(|e| Some((e.path.clone()?, e.edit_seq)))
            .collect()
    }

    /// True when the tab a workspace edit to `path` would land in (as
    /// [`Self::apply_rename_to_open_tab`] picks it) was edited after `seqs`
    /// was taken: the edit's positions describe text no longer there. A
    /// file no tab held when `seqs` was taken is never called edited.
    pub fn edited_since(&self, seqs: &[(PathBuf, u64)], path: &Path) -> bool {
        let Some(idx) = self.find_buffer_tab(path) else {
            return false;
        };
        let e = &self.editors[idx];
        let Some(p) = e.path.as_ref() else {
            return false;
        };
        let mut then = seqs.iter().filter(|(q, _)| q == p).peekable();
        then.peek().is_some() && !then.any(|(_, seq)| *seq == e.edit_seq)
    }

    pub(super) fn find_tab_matching(
        &self,
        target: &Path,
        extra: impl Fn(&Editor) -> bool,
    ) -> Option<usize> {
        let canon_target = target.canonicalize().ok();
        self.editors.iter().position(|e| {
            if !extra(e) {
                return false;
            }
            let Some(p) = e.path.as_ref() else {
                return false;
            };
            if p == target {
                return true;
            }
            match (canon_target.as_ref(), p.canonicalize().ok()) {
                (Some(a), Some(b)) => *a == b,
                _ => false,
            }
        })
    }

    /// The tab a preview open may replace, or drop when the file it opens is
    /// already in another tab. A tab holding unsaved work is never it
    /// ([`holds_unsaved_work`]); a preview open pins such a tab first
    /// ([`Self::pin_previews_with_unsaved_work`]) (#2073).
    pub fn preview_index(&self) -> Option<usize> {
        self.editors
            .iter()
            .position(|e| e.preview && !holds_unsaved_work(e))
    }

    /// Pin, for good, every preview tab holding unsaved work. Every edit
    /// pins its tab ([`Editor::mark_dirty`]), but a sheet cell still being
    /// typed into is not an edit yet, and a path that sets `dirty` some other
    /// way would leave the tab a preview. Only skipping such a tab would put
    /// a second preview beside it, and the first would turn replaceable
    /// again once saved (#2073).
    fn pin_previews_with_unsaved_work(&mut self) {
        for e in &mut self.editors {
            if e.preview && holds_unsaved_work(e) {
                e.preview = false;
            }
        }
    }

    /// Mark the active tab as pinned (no longer the preview slot). Called
    /// when the user double-clicks a file, hits Ctrl+Enter, or starts typing
    /// inside a preview tab.
    pub fn pin_active(&mut self) {
        self.editors[self.active].preview = false;
    }

    /// True when the tab at `idx` is the replaceable preview slot. Drives the
    /// tab menu's "Keep Open" entry, which only makes sense for a preview tab.
    pub fn is_preview(&self, idx: usize) -> bool {
        self.editors.get(idx).is_some_and(|e| e.preview)
    }

    /// VS Code "Keep Open": promote the tab at `idx` out of the preview slot
    /// so a subsequent single-click open no longer replaces it. Returns true
    /// when a preview tab was actually promoted (false if it was already
    /// permanent or the index is out of range).
    pub fn keep_open(&mut self, idx: usize) -> bool {
        match self.editors.get_mut(idx) {
            Some(e) if e.preview => {
                e.preview = false;
                true
            }
            _ => false,
        }
    }

    /// True when the tab at `idx` is pinned. Drives the tab menu's
    /// "Pin"/"Unpin" label and the close-cell glyph (pin vs `\u{2715}`).
    pub fn is_pinned(&self, idx: usize) -> bool {
        self.editors.get(idx).is_some_and(|e| e.pinned)
    }

    /// VS Code "Pin"/"Unpin" the tab at `idx`. Returns the tab's new pinned
    /// state. Pinning clears the preview flag (a pinned tab is never the
    /// replaceable preview slot) and reorders the strip so pinned tabs stay
    /// leftmost: a stable partition lands a newly-pinned tab at the end of the
    /// pinned block and a newly-unpinned tab at the front of the unpinned
    /// block, matching VS Code. The active tab follows its editor across the
    /// reorder.
    pub fn toggle_pin(&mut self, idx: usize) -> bool {
        if idx >= self.editors.len() {
            return false;
        }
        let now_pinned = !self.editors[idx].pinned;
        self.editors[idx].pinned = now_pinned;
        if now_pinned {
            self.editors[idx].preview = false;
        }
        // Stable partition: pinned first (keeping their relative order), then
        // unpinned (keeping theirs). `sort_by_key` is stable, so the two blocks
        // preserve order and only the toggled tab crosses the boundary.
        let n = self.editors.len();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| !self.editors[i].pinned);
        let new_active = order.iter().position(|&i| i == self.active).unwrap_or(0);
        let mut slots: Vec<Option<Editor>> = self.editors.drain(..).map(Some).collect();
        self.editors = order.iter().map(|&i| slots[i].take().unwrap()).collect();
        self.active = new_active;
        for (i, ed) in self.editors.iter_mut().enumerate() {
            ed.focused = i == self.active;
        }
        now_pinned
    }

    /// Move the tab at `from` to position `to` in this group (#1565), as VS
    /// Code's tab drag and Move Editor Left / Right do. Pinned tabs stay ahead
    /// of the rest, so `to` is clamped into the moved tab's own block. The
    /// active tab stays active. Returns the tab's new index, or `None` when
    /// `from` is out of range.
    pub fn move_tab(&mut self, from: usize, to: usize) -> Option<usize> {
        if from >= self.editors.len() {
            return None;
        }
        let pinned = self.editors.iter().filter(|e| e.pinned).count();
        let to = if self.editors[from].pinned {
            to.min(pinned - 1)
        } else {
            to.clamp(pinned, self.editors.len() - 1)
        };
        if to == from {
            return Some(from);
        }
        let active = self.active;
        let ed = self.editors.remove(from);
        self.editors.insert(to, ed);
        self.active = if active == from {
            to
        } else if from < active && active <= to {
            active - 1
        } else if to <= active && active < from {
            active + 1
        } else {
            active
        };
        Some(to)
    }

    /// View: Move Editor Left / Right (#1565): shift the active tab one place
    /// within its group. False when it is already at that end (of its pinned
    /// or unpinned block), so nothing moved.
    pub fn move_active_tab(&mut self, right: bool) -> bool {
        let from = self.active;
        let to = if right {
            from + 1
        } else {
            match from.checked_sub(1) {
                Some(to) => to,
                None => return false,
            }
        };
        self.move_tab(from, to).is_some_and(|at| at != from)
    }

    /// Remove and return the active editor (for a Move to another group). When
    /// it was the only tab the group falls back to a single blank tab (like
    /// closing the last tab), so the group stays renderable; the caller can
    /// detect that empty state via [`Self::is_blank_initial`] and prune it.
    pub fn take_active_editor(&mut self) -> Editor {
        let ed = self.editors.remove(self.active);
        if self.editors.is_empty() {
            self.editors.push(Editor::new());
            self.active = 0;
        } else if self.active >= self.editors.len() {
            self.active = self.editors.len() - 1;
        }
        for (i, e) in self.editors.iter_mut().enumerate() {
            e.focused = i == self.active;
        }
        ed
    }

    /// How `moved`, a tab moved in from another group, lands here (#2745).
    /// A group shows a file once, as in VS Code: when one of this group's
    /// tabs already holds the file, the move ends on one tab, the copy that
    /// keeps every unsaved edit. Each group keeps its own buffer, so both
    /// copies can hold edits of their own; then neither can go.
    pub fn landing_for(&self, moved: &Editor) -> MoveLanding {
        let plain = |e: &Editor| e.diff.is_none() && e.symbol_view.is_none();
        let twin = moved
            .path
            .as_deref()
            .filter(|_| plain(moved))
            .and_then(|path| self.find_tab_matching(path, plain));
        let Some(idx) = twin else {
            return MoveLanding::New;
        };
        let here = &self.editors[idx];
        if here.lines == moved.lines || !moved.dirty {
            MoveLanding::Twin(idx)
        } else if !here.dirty {
            MoveLanding::Replace(idx)
        } else {
            MoveLanding::Clash(idx)
        }
    }

    /// Take in `moved` from another group, as [`Self::landing_for`] says,
    /// and make the tab it lands on active. A clash, which the caller
    /// refuses before taking the tab out of its group, keeps both copies.
    pub fn receive_moved(&mut self, mut moved: Editor) {
        match self.landing_for(&moved) {
            MoveLanding::New | MoveLanding::Clash(_) => self.push_editor(moved),
            MoveLanding::Twin(idx) => {
                self.select(idx);
            }
            MoveLanding::Replace(idx) => {
                // The slot keeps its place, so its pin and preview state go
                // with it: pinned tabs stay ahead, one preview per group.
                moved.pinned = self.editors[idx].pinned;
                moved.preview = self.editors[idx].preview;
                moved.focused = false;
                self.editors[idx] = moved;
                self.select(idx);
            }
        }
    }

    /// Add `ed` (moved from another group) and make it active. A blank-initial
    /// group is replaced in place so a Move never leaves a stray blank tab
    /// beside the moved editor. A pinned editor lands at the end of the
    /// pinned block, so pinned tabs stay ahead of the rest.
    pub fn push_editor(&mut self, mut ed: Editor) {
        if self.is_blank_initial() {
            ed.focused = true;
            self.editors[0] = ed;
            self.active = 0;
            return;
        }
        let at = if ed.pinned {
            self.editors.iter().take_while(|e| e.pinned).count()
        } else {
            self.editors.len()
        };
        self.editors.insert(at, ed);
        self.active = at;
        for (i, e) in self.editors.iter_mut().enumerate() {
            e.focused = i == self.active;
        }
    }

    /// VS Code "preview tab" semantics: open `path` in the single
    /// replaceable preview slot. If the file is already in some tab, just
    /// switch to it. Otherwise reuse the existing preview slot, or create a
    /// fresh preview tab next to the active one when none exists.
    pub fn open_preview(&mut self, path: &Path) -> Result<()> {
        self.pin_previews_with_unsaved_work();
        // A diff of the file is not the file (#1113): opening it lands on
        // its own tab, or a new one beside the diff.
        if let Some(idx) =
            self.find_tab_matching(path, |e| e.diff.is_none() && e.symbol_view.is_none())
        {
            // Switching to an already-open file: if a stale preview tab
            // exists for some OTHER file, drop it so the user doesn't
            // accumulate ghost tabs from quick "peek" navigations.
            if let Some(prev) = self.preview_index()
                && prev != idx
            {
                let dropped = self.editors.remove(prev);
                self.release_bookmarks(&dropped);
                let new_idx = if idx > prev { idx - 1 } else { idx };
                // Bypass `select` here because the removal may have left
                // `self.active` pointing past the end (when the active
                // tab was the preview we just dropped).
                self.active = new_idx;
                for (i, ed) in self.editors.iter_mut().enumerate() {
                    ed.focused = i == new_idx;
                }
                return Ok(());
            }
            self.select(idx);
            return Ok(());
        }
        if let Some(idx) = self.preview_index() {
            self.select(idx);
            self.editors[idx].open(path)?;
            self.editors[idx].preview = true;
            return Ok(());
        }
        if self.is_blank_initial() {
            let active = self.active;
            self.editors[active].open(path)?;
            self.editors[active].preview = true;
            self.editors[active].search_highlight = self.search_highlight_term.clone();
            self.editors[active].search_highlight_opts = self.search_highlight_opts;
            return Ok(());
        }
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.open(path)?;
        e.preview = true;
        e.search_highlight = self.search_highlight_term.clone();
        e.search_highlight_opts = self.search_highlight_opts;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// The text a diff should show for `path` (#1572): an open tab's unsaved
    /// edits, as a save would write them, since that is the version the user
    /// is looking at; the file on disk when no tab has unsaved edits to it.
    pub(super) fn text_to_diff(unsaved: Option<String>, path: &Path) -> Result<String> {
        match unsaved {
            Some(text) => Ok(text),
            None => {
                std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
            }
        }
    }

    /// The text a save of `path`'s open tab would write, when that tab has
    /// unsaved edits; `None` otherwise. Searches this group only: the app
    /// resolves it across every split group before calling the `_resolved`
    /// openers.
    pub fn unsaved_text_of(&self, path: &Path) -> Option<String> {
        let idx = self.find_tab_matching(path, |e| {
            e.dirty && e.symbol_view.is_none() && !e.has_non_text_view()
        })?;
        Some(self.editors[idx].content_for_disk())
    }

    /// Open a side-by-side diff of two files in a fresh pinned tab next
    /// to the active one, each side read the way [`Self::text_to_diff`]
    /// says. The new tab is read-only: edits, save, and the
    /// text-rendering path are all bypassed via the `diff: Some(...)` flag
    /// on the underlying Editor.
    #[cfg(test)]
    pub fn open_diff(&mut self, left: &Path, right: &Path) -> Result<()> {
        let left_unsaved = self.unsaved_text_of(left);
        let right_unsaved = self.unsaved_text_of(right);
        self.open_diff_resolved(left, right, left_unsaved, right_unsaved)
    }

    /// [`Self::open_diff`] with each side's unsaved text already resolved by
    /// the caller (across every split group); `None` reads the disk.
    pub fn open_diff_resolved(
        &mut self,
        left: &Path,
        right: &Path,
        left_unsaved: Option<String>,
        right_unsaved: Option<String>,
    ) -> Result<()> {
        let left_text = Self::text_to_diff(left_unsaved, left)?;
        let right_text = Self::text_to_diff(right_unsaved, right)?;
        let left_lines: Vec<String> = left_text.lines().map(str::to_string).collect();
        let right_lines: Vec<String> = right_text.lines().map(str::to_string).collect();
        let mut data = crate::widgets::diff::DiffData::build(
            left.to_path_buf(),
            right.to_path_buf(),
            left_lines,
            right_lines,
        );
        data.left_is_real_file = true;
        data.source = crate::widgets::diff::DiffSource::TwoFiles;
        data.stamp_sides();
        data.set_whitespace_mode(self.diff_ws_default);

        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        // The path is set so close-by-path lookups work; `diff` being Some
        // is what diverts the renderer onto the side-by-side path.
        e.path = Some(left.to_path_buf());
        e.diff = Some(data);
        // Reuse the blank-initial slot if the editor pane was empty;
        // otherwise insert a new tab next to the active one.
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Open a side-by-side diff between an in-memory `left_text` (e.g.
    /// the HEAD version of a file) and `right` as the user sees it: its open
    /// tab's unsaved edits, else the file on disk. The left "path" is purely
    /// a label; nothing reads from it on disk.
    /// Used by the Source Control panel to show working-tree-vs-HEAD
    /// diffs when the user clicks a Modified entry.
    #[cfg(test)]
    pub fn open_head_diff_with_text(
        &mut self,
        left_label: PathBuf,
        left_text: &str,
        right: &Path,
        left_is_git_head: bool,
    ) -> Result<()> {
        let unsaved = self.unsaved_text_of(right);
        self.open_head_diff_with_text_resolved(
            left_label,
            left_text,
            right,
            left_is_git_head,
            unsaved,
        )
    }

    /// [`Self::open_head_diff_with_text`] with `right`'s unsaved text already
    /// resolved by the caller (across every split group); `None` reads the
    /// disk.
    pub fn open_head_diff_with_text_resolved(
        &mut self,
        left_label: PathBuf,
        left_text: &str,
        right: &Path,
        left_is_git_head: bool,
        unsaved: Option<String>,
    ) -> Result<()> {
        let right_is_unsaved = unsaved.is_some();
        let right_text = match unsaved {
            Some(text) => text,
            None => std::fs::read_to_string(right)
                .with_context(|| format!("reading {}", right.display()))?,
        };
        self.open_head_diff_with_texts(
            left_label,
            left_text,
            &right_text,
            right,
            left_is_git_head,
            right_is_unsaved,
        )
    }

    /// [`Self::open_head_diff_with_text`] with the working side already read
    /// and decoded, for a file that isn't UTF-8 (#1242). `right_is_unsaved`
    /// says `right_text` is the open tab's unsaved buffer, not the disk.
    pub fn open_head_diff_with_texts(
        &mut self,
        left_label: PathBuf,
        left_text: &str,
        right_text: &str,
        right: &Path,
        left_is_git_head: bool,
        right_is_unsaved: bool,
    ) -> Result<()> {
        let left_lines: Vec<String> = left_text.lines().map(str::to_string).collect();
        let right_lines: Vec<String> = right_text.lines().map(str::to_string).collect();
        let mut data = crate::widgets::diff::DiffData::build_with_byte_check(
            left_label,
            right.to_path_buf(),
            left_lines,
            right_lines,
            Some(left_text),
            Some(right_text),
        );
        data.set_whitespace_mode(self.diff_ws_default);
        // Park the viewport on the first change hunk so the user lands on
        // the first edit instead of reading through unchanged leading
        // lines. Identical files stay at scroll 0.
        if let Some(row) = data.first_change_row() {
            data.scroll_to_row(row);
        }
        data.left_is_git_head = left_is_git_head;
        data.right_is_unsaved = right_is_unsaved;
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(right.to_path_buf());
        e.diff = Some(data);
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        // If a tab is already showing this path (as a plain file or a
        // diff), reuse it so we don't pile up duplicate tabs as the
        // user clicks through the change list — unless it holds unsaved
        // edits, which replacing wholesale would silently destroy. A diff
        // tab of the path comes first: with the file's own tab dirty, it
        // is the only one to reuse (#1572).
        if let Some(idx) = self
            .find_tab_matching(right, |e| e.diff.is_some())
            .or_else(|| self.find_tab_with_path(right))
            && !self.editors[idx].dirty
        {
            self.editors[idx] = e;
            self.select(idx);
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Open a single-pane unified diff that paints every line of
    /// `head_text` as a removed line. `path` is the workspace-relative
    /// path that no longer exists on disk; it doubles as the display
    /// label in the diff header and as the tab dedup key so repeated
    /// clicks on the same Source-Control row reuse the tab rather than
    /// stacking new ones.
    pub fn open_deleted_diff_with_text(&mut self, path: &Path, head_text: &str) -> Result<()> {
        let mut data =
            crate::widgets::diff::DiffData::build_unified_deletion(path.to_path_buf(), head_text);
        data.set_whitespace_mode(self.diff_ws_default);
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(path.to_path_buf());
        e.diff = Some(data);
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        // Same dirty guard as `open_head_diff_with_text`: a deleted-on-disk
        // file can still hold unsaved edits in its tab.
        if let Some(idx) = self.find_tab_with_path(path)
            && !self.editors[idx].dirty
        {
            self.editors[idx] = e;
            self.select(idx);
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Open a side-by-side view of raw `git diff` text (e.g. the stdout
    /// of `git diff --staged` or `git diff <branch>`). `label` is a
    /// synthetic path used as the tab title and as the dedup key so a
    /// repeat invocation reuses the existing tab instead of stacking new
    /// ones. The text is parsed into separate left/right streams so the
    /// standard two-column renderer takes over — every `+`/`-` pair in a
    /// hunk lines up horizontally instead of zigzagging vertically.
    pub fn open_git_diff_side_by_side(&mut self, label: &Path, raw_diff: &str) -> Result<()> {
        let mut data = crate::widgets::diff::DiffData::build_side_by_side_from_git_text(
            label.to_path_buf(),
            raw_diff,
        );
        data.set_whitespace_mode(self.diff_ws_default);
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(label.to_path_buf());
        e.diff = Some(data);
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        if let Some(idx) = self.find_tab_with_path(label) {
            self.editors[idx] = e;
            self.select(idx);
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Show a fleet run's results in the `Fleet` tab (#363), replacing the
    /// previous run's, so one tab always holds the latest comparison.
    pub fn open_fleet(&mut self, view: crate::widgets::fleet::FleetView) {
        let label = PathBuf::from("Fleet");
        if let Some(idx) = self.find_tab_with_path(&label) {
            self.editors[idx].fleet = Some(view);
            self.select(idx);
            return;
        }
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(label);
        e.fleet = Some(view);
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return;
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
    }

    /// Open a pull request for review in a fresh tab labelled `PR #n`
    /// (#365), or re-select the tab already showing it.
    pub fn open_pr_review(&mut self, view: crate::widgets::pr_review::PrReviewView) {
        let label = PathBuf::from(format!("PR #{}", view.pr.number));
        if let Some(idx) = self.find_tab_with_path(&label) {
            self.editors[idx].pr_review = Some(view);
            self.select(idx);
            return;
        }
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(label);
        e.pr_review = Some(view);
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return;
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
    }

    /// Open arbitrary text in a scratch tab labelled `label` (no file on
    /// disk). Used by "Show Git Output" to surface the git command log. The
    /// tab has no `disk_stamp`, so the FS-sync layer never tries to reload
    /// or overwrite it, and is `scratch`, so no save writes it (#951).
    pub fn open_text_buffer(&mut self, label: &Path, text: &str) -> Result<()> {
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.preview = false;
        e.path = Some(label.to_path_buf());
        e.scratch = true;
        e.lines = split_into_lines(text);
        if e.lines.is_empty() {
            e.lines.push(String::new());
        }
        if self.is_blank_initial() {
            self.editors[self.active] = e;
            return Ok(());
        }
        if let Some(idx) = self.find_tab_with_path(label) {
            self.editors[idx] = e;
            self.select(idx);
            return Ok(());
        }
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// Pinned-tab open: if the file is already in some tab, pin and switch
    /// to it. Otherwise create a fresh pinned tab next to the active one.
    /// Used by double-click in the explorer and Ctrl+Enter on a tree row.
    pub fn open_pinned(&mut self, path: &Path) -> Result<()> {
        self.open_pinned_with(path, Editor::open)
    }

    /// [`Self::open_pinned`] for a file that may not exist yet: a missing
    /// one opens as an empty buffer that its first save creates (#1157).
    pub fn open_new_pinned(&mut self, path: &Path) -> Result<()> {
        self.open_pinned_with(path, Editor::open_new_file)
    }

    pub(super) fn open_pinned_with(
        &mut self,
        path: &Path,
        open: fn(&mut Editor, &Path) -> Result<()>,
    ) -> Result<()> {
        // A diff tab can carry the working file's `path` yet is not an
        // editable view of it, so skip diffs here: Enter on a diff (or a
        // double-click) opens the real file beside the diff rather than just
        // re-selecting the diff tab.
        if let Some(idx) =
            self.find_tab_matching(path, |e| e.diff.is_none() && e.symbol_view.is_none())
        {
            self.editors[idx].preview = false;
            self.select(idx);
            return Ok(());
        }
        if self.is_blank_initial() {
            let active = self.active;
            open(&mut self.editors[active], path)?;
            self.editors[active].preview = false;
            self.editors[active].search_highlight = self.search_highlight_term.clone();
            self.editors[active].search_highlight_opts = self.search_highlight_opts;
            return Ok(());
        }
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        open(&mut e, path)?;
        e.preview = false;
        e.search_highlight = self.search_highlight_term.clone();
        e.search_highlight_opts = self.search_highlight_opts;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
        Ok(())
    }

    /// True iff the editor is in its just-launched state: a single tab with
    /// no file loaded and no edits. `App::render` uses this to swap the
    /// editor pane for the welcome screen, and `open_preview` /
    /// `open_pinned` use it to reuse the placeholder tab rather than stack
    /// a new one on top.
    pub fn is_blank_initial(&self) -> bool {
        self.editors.len() == 1
            && self.editors[0].path.is_none()
            && self.editors[0].untitled.is_none()
            && !self.editors[0].dirty
            && self.editors[0].lines.iter().all(|l| l.is_empty())
    }

    /// File: New Untitled Text File (#1580): a pinned, empty tab with no
    /// path next to the active one, shown as `Untitled-n`. The caller picks
    /// `n` across every split group, which this group cannot see. The blank
    /// startup tab becomes it rather than staying behind.
    pub fn new_untitled(&mut self, n: usize) {
        if self.is_blank_initial() {
            let active = self.active;
            self.editors[active].untitled = Some(n);
            self.editors[active].preview = false;
            return;
        }
        let mut e = Editor::new();
        e.focused = self.editors[self.active].focused;
        e.untitled = Some(n);
        e.search_highlight = self.search_highlight_term.clone();
        e.search_highlight_opts = self.search_highlight_opts;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
    }

    /// Test-only helper that mirrors `open_preview` without going through
    /// the disk: insert a preview tab whose path is set but whose buffer is
    /// empty. Used by unit tests to exercise preview-slot bookkeeping in
    /// isolation from filesystem I/O.
    pub fn add_preview_tab_with_path(&mut self, path: PathBuf) {
        let mut e = Editor::new();
        e.path = Some(path);
        e.preview = true;
        e.focused = self.editors[self.active].focused;
        let pos = self.active + 1;
        self.editors.insert(pos, e);
        self.editors[self.active].focused = false;
        self.active = pos;
    }

    /// Test-only helper for the "single-click on a different file when a
    /// preview tab already exists" path: rewrite the existing preview tab's
    /// path without changing tab count.
    pub fn repoint_preview_to(&mut self, path: PathBuf) {
        if let Some(idx) = self.preview_index() {
            self.editors[idx].path = Some(path);
        }
    }
}

/// Whether a tab holds work not yet saved: unsaved text, or a sheet cell
/// still being typed into, which only becomes an edit when committed.
fn holds_unsaved_work(e: &Editor) -> bool {
    e.dirty || e.sheet_pending_cell().is_some()
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

// Tab-strip chrome (strip, tab bodies, hover lift, close pill) is per-theme
// data: `Theme::tab_*` carries the two built-ins' historical constants and
// derives colors for every manifest theme, so the strip follows the selected
// theme instead of staying VS-Code navy. Only the label foregrounds stay
// fixed: this muted grey and white are legible on all dark themes.
/// The tab strip's scroll position and button, see `EditorTabs::tab_strip`.
#[derive(Default)]
pub(super) struct TabStripState {
    /// Index of the first tab drawn in the strip: it scrolls by tab when
    /// there are more of them than fit (wheel over the strip, or the active
    /// tab changing).
    scroll: usize,
    /// The `(active, tab count)` the strip last scrolled itself to reveal, so
    /// a wheel scroll away from the active tab is not undone on the next
    /// frame.
    revealed: Option<(usize, usize)>,
    /// Cells at the strip's right end the app paints over (the Customize
    /// Layout icons); tabs stop short of them. Set before each render.
    reserved_right: u16,
    /// The "all open tabs" button at the right end of the tab area, drawn
    /// only when the tabs do not all fit (otherwise empty).
    list_button: Rect,
    /// When and which way the wheel last moved the strip, so a touchpad's
    /// flood of events moves it a tab at a time rather than all at once.
    wheel_at: Option<(std::time::Instant, isize)>,
}

/// Cells the "all open tabs" button takes at the right end of a crowded strip.
pub(super) const TAB_LIST_W: u16 = 3;

/// The least time between two tabs of wheel scrolling in one direction.
pub(super) const TAB_WHEEL_GAP: std::time::Duration = std::time::Duration::from_millis(90);

pub(super) const TAB_INACTIVE_FG: Color = Color::Rgb(0x9d, 0xa5, 0xb4);

pub(super) const TAB_ACTIVE_FG: Color = Color::White;

/// Ink for the tab close/pin glyph on its hover pill: white, except that a
/// high contrast theme (#1548) takes whichever of black or white reads
/// better on its pill (black on High Contrast's orange).
pub(super) fn close_pill_fg(theme: crate::theme::Theme, pill: Color) -> Color {
    match pill {
        Color::Rgb(r, g, b)
            if theme.high_contrast()
                && crate::theme::contrast_ratio((r, g, b), (0, 0, 0))
                    > crate::theme::contrast_ratio((r, g, b), (0xff, 0xff, 0xff)) =>
        {
            Color::Black
        }
        _ => Color::White,
    }
}

/// The file-type icon a tab leads with on a `surfaces` theme: the
/// Explorer's icon for its file (a diff tab's left side), the plain file
/// icon for an untitled buffer.
pub(super) fn tab_icon(ed: &Editor) -> crate::icons::Icon {
    let path = ed
        .path
        .as_deref()
        .or_else(|| ed.diff.as_ref().map(|d| d.left_path.as_path()));
    match path.and_then(|p| p.file_name().map(|n| (p, n))) {
        Some((p, name)) => {
            let suffix = p
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            crate::icons::for_path(&name.to_string_lossy(), &suffix)
        }
        None => crate::icons::DEFAULT_FILE,
    }
}

/// One visible tab of the strip on a `surfaces` theme, for
/// [`paint_surface_tab`]: where it sits and the states its look follows.
pub(super) struct SurfaceTab<'a> {
    /// The tab's first cell (its leading pad).
    x: u16,
    y: u16,
    label: &'a str,
    /// Where the label starts, after the pad, the icon and its gap.
    label_x: u16,
    /// The close / pin / dirty-dot cell, the tab's close hit target.
    close_x: u16,
    ed: &'a Editor,
    theme: crate::theme::Theme,
    t: crate::widgets::surface::Tones,
    /// The tab body's fill: canvas when active, the hover lift, or chrome.
    bg: Color,
    is_active: bool,
    /// The tab's group is the focused editor group.
    group_focused: bool,
    /// That group has the keyboard (focused, no modal over it).
    has_keyboard: bool,
    tab_hovered: bool,
    on_close: bool,
}

/// Paint a tab as ` {icon} {label} {close} ` on a `surfaces` theme. Three
/// inks and no bold: an inactive label is muted and lifts to body text
/// under the pointer; the active tab is the region's one strong label in
/// the focused group and body text in another, and only in the group with
/// the keyboard does it wear the accent, as an underline under its label.
/// The close cross is muted on the active or hovered tab and faint on the
/// rest; a dirty file shows a dot in that slot until the pointer is on the
/// tab, so the tab keeps one width whether its file is dirty or not.
pub(super) fn paint_surface_tab(buf: &mut Buffer, tab: SurfaceTab<'_>) {
    let SurfaceTab {
        x,
        y,
        label,
        label_x,
        close_x,
        ed,
        theme,
        t,
        bg,
        is_active,
        group_focused,
        has_keyboard,
        tab_hovered,
        on_close,
    } = tab;
    let fill = Style::default().bg(bg);
    let label_fg = if is_active {
        if group_focused { t.strong() } else { t.text() }
    } else if tab_hovered {
        t.text()
    } else {
        t.muted()
    };
    // The pads and gaps first, so every cell of the tab wears its fill.
    for cx in [x, x + 2, close_x - 1, close_x + 1] {
        buf.set_string(cx, y, " ", fill);
    }
    // The icon in its own colour on the active or hovered tab, eased most
    // of the way into the tab's fill on the others.
    let icon = tab_icon(ed);
    let icon_fg = match (theme.ui(icon.color), bg) {
        (Color::Rgb(r, g, b), Color::Rgb(br, bgc, bb)) if !(is_active || tab_hovered) => {
            crate::gradient::rgb_color(crate::gradient::lerp_rgb((r, g, b), (br, bgc, bb), 0.45))
        }
        (c, _) => c,
    };
    buf.set_string(
        x + 1,
        y,
        icon.glyph.to_string(),
        Style::default().fg(icon_fg).bg(bg),
    );
    let mut style = Style::default().fg(label_fg).bg(bg);
    if ed.preview {
        style = style.add_modifier(Modifier::ITALIC);
    }
    buf.set_string(label_x, y, label, style);
    if is_active && has_keyboard {
        let width = Span::raw(label).width() as u16;
        for ux in label_x..label_x + width {
            let cell = &mut buf[(ux, y)];
            cell.modifier.insert(Modifier::UNDERLINED);
            cell.underline_color = t.accent();
        }
    }
    // The close slot: a thumb-tack on a pinned tab (clicking it unpins), a
    // dot on a dirty one while the pointer is elsewhere, else the cross.
    let (glyph, ink) = if ed.pinned {
        ("\u{f08d}", t.muted())
    } else if ed.dirty && !tab_hovered {
        ("\u{25cf}", if is_active { t.text() } else { t.muted() })
    } else if is_active || tab_hovered {
        ("\u{2715}", t.muted())
    } else {
        ("\u{2715}", t.faint())
    };
    let close_style = if on_close {
        // The pill names exactly the glyph the click will land on.
        Style::default()
            .fg(t.strong())
            .bg(theme.tab_close_pill_bg())
    } else {
        Style::default().fg(ink).bg(bg)
    };
    buf.set_string(close_x, y, glyph, close_style);
}

impl Widget for &mut EditorTabs {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.last_full_area = area;
        if area.height == 0 || area.width == 0 {
            // Ctrl+Shift+J maximises the terminal pane, which collapses
            // the editor area to height 0. The inner editor's `render`
            // is never reached on this branch, so its `last_area` would
            // otherwise keep the rectangle from the pre-maximise frame
            // and `App::handle_mouse`'s `in_editor` hit-test
            // (rect_contains(self.editor.last_area, ...)) would still
            // win against the terminal in the dispatch chain at
            // app.rs:7229 / 7336 — clicks meant to begin a terminal
            // selection get routed to the editor's mouse_down and the
            // file caret jumps instead. Zero the inner editor's
            // hit-test rectangle here so the terminal pane wins
            // unambiguously while the editor is collapsed.
            for ed in self.editors.iter_mut() {
                ed.last_area = Rect {
                    x: area.x,
                    y: area.y,
                    width: area.width,
                    height: 0,
                };
                ed.last_inner = ed.last_area;
                ed.last_scrollbar = Rect::default();
            }
            return;
        }
        let strip_h: u16 = 1;
        let strip = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: strip_h,
        };
        let mut body = Rect {
            x: area.x,
            y: area.y + strip_h,
            width: area.width,
            height: area.height - strip_h,
        };

        // The tab chrome follows the theme. Like the brand flag below, the
        // theme propagates from a synced sibling: a tab opened since the last
        // sync still carries the default theme and must not flash the wrong
        // chrome for a frame.
        let theme = self
            .editors
            .iter()
            .map(|e| e.theme)
            .find(|t| *t != crate::theme::Theme::default())
            .unwrap_or_default();
        // Paint strip background first so the gap to the right of the last
        // tab still reads as the tab-strip colour rather than terminal default.
        let strip_bg_style = Style::default().bg(theme.tab_strip_bg());
        for x in strip.x..strip.x + strip.width {
            buf[(x, strip.y)].set_style(strip_bg_style);
            buf[(x, strip.y)].set_symbol(" ");
        }

        self.tab_strip_y = strip.y;
        self.tab_screen_ranges.clear();
        self.tab_close_x.clear();
        let active = self.active;
        // Black theme (`focus_gradient` doubles as the theme flag): the
        // active tab wears the muted teal selection fill instead of the
        // legacy navy chip; Croft Dark keeps the navy. Sync pushes the flag
        // to every editor, but one freshly opened since the last sync still
        // defaults to false — propagate from its siblings so it can't flash
        // the navy chip (or a blue body border) for a frame.
        let brand = self.editors.iter().any(|e| e.focus_gradient);
        if brand {
            for ed in self.editors.iter_mut() {
                ed.focus_gradient = true;
            }
        }
        let active_tab_bg = theme.tab_active_bg();
        let pointer = self.hover_pointer;
        // A `surfaces` theme (Croft Loch) lays a tab out as
        // ` {icon} {label} {close} `: a file-type icon leads the label, the
        // dirty dot moves out of the label into the close slot (so a tab
        // never changes width as its file goes dirty), and only the active
        // tab of the group with the keyboard wears the accent, as an
        // underline. Every other theme keeps ` {label} {close} `.
        let tones = crate::widgets::surface::tones(theme);
        let group_focused = self.editors[active].focused;
        let has_keyboard = group_focused && !self.editors[active].overlay_has_keyboard;
        let icon_w: u16 = if tones.is_some() { 2 } else { 0 };
        let display_labels: Vec<String> = disambiguated_tab_labels(&self.editors)
            .into_iter()
            .zip(&self.editors)
            .map(|(label, ed)| {
                // The menus, status line and screen reader keep the `● `
                // prefix (`tab_display_label`); only the strip drops it, and
                // only where the close slot shows the dot instead: a pinned
                // tab's slot holds the pin, so its label keeps the prefix.
                match label.strip_prefix("\u{25cf} ") {
                    Some(rest) if tones.is_some() && ed.dirty && !ed.pinned => rest.to_string(),
                    _ => label,
                }
            })
            .collect();
        // Tabs never run under what the app paints at the strip's right end,
        // and the strip scrolls by tab when they do not all fit.
        let mut strip_right = strip.x
            + strip
                .width
                .saturating_sub(self.tab_strip.reserved_right.min(strip.width));
        let avail_full = usize::from(strip_right.saturating_sub(strip.x));
        let widths: Vec<usize> = display_labels
            .iter()
            .map(|l| {
                // Display CELLS, not chars (#168 review): a CJK file or
                // directory name is double-width, and a char count shifted
                // the close button and hit ranges left of the painted text.
                // A pad either side, the close cell and a gap, plus the icon
                // and its gap on a `surfaces` theme.
                usize::from(Span::raw(l.as_str()).width() as u16) + 4 + usize::from(icon_w)
            })
            .collect();
        let n = widths.len();
        // Tabs that do not all fit get an "all open tabs" button in the last
        // cells of the tab area, so the hidden ones stay reachable.
        self.tab_strip.list_button = Rect::default();
        if widths.iter().sum::<usize>() > avail_full && avail_full > usize::from(TAB_LIST_W) {
            strip_right -= TAB_LIST_W;
            self.tab_strip.list_button = Rect {
                x: strip_right,
                y: strip.y,
                width: TAB_LIST_W,
                height: 1,
            };
            let hovered = self.hover_pointer.is_some_and(|(px, py)| {
                py == strip.y && px >= strip_right && px < strip_right + TAB_LIST_W
            });
            // As the layout icons beside it: the glyph goes from grey to the
            // accent on hover, the cell keeps the strip's own background. On
            // a `surfaces` theme it is a header action: muted at rest, the
            // strong ink on a faint chip under the pointer.
            let fg = if hovered {
                theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))
            } else {
                theme.ui(Color::Rgb(0xcc, 0xcc, 0xcc))
            };
            let mut style = Style::default().fg(fg).bg(theme.tab_strip_bg());
            if tones.is_some() {
                style = style.patch(crate::widgets::header_pill::action_style(theme, hovered));
            }
            buf.set_string(strip_right, strip.y, " \u{2261} ", style);
        }
        let avail = usize::from(strip_right.saturating_sub(strip.x));
        // Last scroll that still fills the strip: past it the tail would leave
        // blank room on the right while earlier tabs stay hidden.
        let max_scroll = {
            let mut first = n.saturating_sub(1);
            let mut used = widths.get(first).copied().unwrap_or(0);
            while first > 0 && used + widths[first - 1] <= avail {
                first -= 1;
                used += widths[first];
            }
            first
        };
        if self.tab_strip.revealed != Some((active, n)) {
            self.tab_strip.revealed = Some((active, n));
            if active < self.tab_strip.scroll {
                self.tab_strip.scroll = active;
            } else {
                // Scroll right just far enough that the active tab fits whole.
                while self.tab_strip.scroll < active
                    && widths[self.tab_strip.scroll..=active].iter().sum::<usize>() > avail
                {
                    self.tab_strip.scroll += 1;
                }
            }
        }
        self.tab_strip.scroll = self.tab_strip.scroll.min(max_scroll);
        let mut cursor_x = strip.x;
        let mut hidden_rest = false;
        for (i, ed) in self.editors.iter().enumerate() {
            let label_text = display_labels[i].clone();
            let label_chars = Span::raw(label_text.as_str()).width() as u16;
            let width = widths[i] as u16;
            if i < self.tab_strip.scroll
                || hidden_rest
                || usize::from(cursor_x.saturating_add(width)) > usize::from(strip_right)
            {
                // Scrolled off, or past the room: no cells, no hit range.
                hidden_rest |= i >= self.tab_strip.scroll;
                self.tab_screen_ranges.push((cursor_x, 0));
                self.tab_close_x.push(0);
                continue;
            }
            let is_active = i == active;
            let label_x = cursor_x + 1 + icon_w;
            let close_x = label_x + label_chars + 1;
            // Treatment C: the pointer resting anywhere on a tab lifts an
            // inactive tab's body (the active tab is already prominent), and
            // resting on the close cross gives that single cell a pill.
            let tab_hovered = pointer.is_some_and(|(px, py)| {
                py == strip.y && px >= cursor_x && px < cursor_x.saturating_add(width)
            });
            let on_close = pointer == Some((close_x, strip.y));
            let hover_lift = tab_hovered && !is_active;
            let bg = if hover_lift {
                theme.tab_hover_bg()
            } else if is_active {
                active_tab_bg
            } else {
                theme.tab_inactive_bg()
            };
            if let Some(t) = tones {
                paint_surface_tab(
                    buf,
                    SurfaceTab {
                        x: cursor_x,
                        y: strip.y,
                        label: &label_text,
                        label_x,
                        close_x,
                        ed,
                        theme,
                        t,
                        bg,
                        is_active,
                        group_focused,
                        has_keyboard,
                        tab_hovered,
                        on_close,
                    },
                );
                self.tab_screen_ranges.push((cursor_x, width));
                self.tab_close_x.push(close_x);
                cursor_x = cursor_x.saturating_add(width);
                continue;
            }
            let fg = if is_active || hover_lift {
                TAB_ACTIVE_FG
            } else {
                TAB_INACTIVE_FG
            };
            // A high contrast theme resolves the label greys to its pure
            // foreground (#1548): white on High Contrast Light's pale active
            // tab would be unreadable.
            let fg = if theme.high_contrast() {
                theme.ui(fg)
            } else {
                fg
            };
            let mut modifiers = Modifier::empty();
            if is_active {
                modifiers |= Modifier::BOLD;
            }
            if ed.preview {
                modifiers |= Modifier::ITALIC;
            }
            let style = Style::default().fg(fg).bg(bg).add_modifier(modifiers);
            // The close cell shows a thumb-tack on a pinned tab (clicking it
            // unpins) and the close cross otherwise (clicking it closes).
            let close_glyph = if ed.pinned { "\u{f08d}" } else { "\u{2715}" };
            // Layout: " " + label + " " + ✕/pin + " "
            let padded = format!(" {label_text} {close_glyph} ");
            buf.set_string(cursor_x, strip.y, &padded, style);
            // Overpaint the close/pin cell with its hover pill so the user sees
            // exactly which glyph their click will land on.
            if on_close {
                let pill_bg = theme.tab_close_pill_bg();
                buf.set_string(
                    close_x,
                    strip.y,
                    close_glyph,
                    Style::default()
                        .fg(close_pill_fg(theme, pill_bg))
                        .bg(pill_bg)
                        .add_modifier(Modifier::BOLD),
                );
            }
            self.tab_screen_ranges.push((cursor_x, width));
            self.tab_close_x.push(close_x);
            cursor_x = cursor_x.saturating_add(width);
        }

        // Breadcrumb bar (VS Code editor breadcrumbs): one row below the tab
        // strip showing the file path + enclosing symbol chain. Carve it off
        // the body so the editor's own coordinate math shrinks with it.
        self.breadcrumb_ranges.clear();
        self.breadcrumb_y = None;
        if !self.breadcrumbs.is_empty() && body.height > 1 {
            let crumb_y = body.y;
            self.breadcrumb_y = Some(crumb_y);
            let theme = self.editors[active].theme;
            let bg = theme.editor_bg();
            for x in area.x..area.x + area.width {
                buf[(x, crumb_y)].set_symbol(" ");
                buf[(x, crumb_y)].set_style(Style::default().bg(bg));
            }
            let max_x = area.x + area.width;
            let mut x = area.x + 1;
            // On a `surfaces` theme the trail is in the theme's inks: faint
            // separators, muted crumbs leading to the one you are in (the
            // last) in body text, the file and symbol crumbs led by their
            // icons.
            let tones = crate::widgets::surface::tones(theme);
            let last = self.breadcrumbs.len() - 1;
            for (i, crumb) in self.breadcrumbs.iter().enumerate() {
                if i > 0 {
                    let sep = " \u{203A} "; // " › "
                    let sw = sep.chars().count() as u16;
                    if x + sw >= max_x {
                        break;
                    }
                    let sep_fg = tones.map_or(Color::DarkGray, |t| t.faint());
                    buf.set_string(x, crumb_y, sep, Style::default().fg(sep_fg).bg(bg));
                    x += sw;
                }
                if x >= max_x {
                    break;
                }
                // The icon and its gap go before the label, outside its hit
                // range, and only where a cell of the label still fits.
                if tones.is_some()
                    && let Some((glyph, color)) = crumb.icon
                    && x + 2 < max_x
                {
                    let icon_style = Style::default().fg(theme.ui(color)).bg(bg);
                    buf.set_string(x, crumb_y, glyph.to_string(), icon_style);
                    x += 2;
                }
                let avail = (max_x - x) as usize;
                // Symbol crumbs (clickable) read brighter than the informational
                // path crumbs, mirroring VS Code's active/inactive breadcrumbs.
                let fg = match tones {
                    Some(t) if i == last => t.text(),
                    Some(t) => t.muted(),
                    None if crumb.target.is_some() => self.theme.ui(Color::Gray),
                    None => Color::DarkGray,
                };
                // Budget and advance in display CELLS: `set_stringn` clips by
                // grapheme width and reports where it stopped, so a CJK or
                // emoji crumb neither overruns the bar nor gets painted over by
                // the next crumb, and its hit rect covers what was drawn.
                let (end_x, _) = buf.set_stringn(
                    x,
                    crumb_y,
                    &crumb.label,
                    avail,
                    Style::default().fg(fg).bg(bg),
                );
                let sw = end_x.saturating_sub(x);
                self.breadcrumb_ranges.push((x, sw, crumb.target));
                x = end_x;
            }
            body = Rect {
                y: body.y + 1,
                height: body.height - 1,
                ..body
            };
        }

        self.last_body = body;
        let active_editor = &mut self.editors[active];
        active_editor.cell_images = self.cell_images;
        active_editor.pointer = self.hover_pointer;
        Widget::render(active_editor, body, buf);
    }
}

impl EditorTabs {
    /// If `(col, row)` lands on a breadcrumb crumb with a jump target, return
    /// the caret position to navigate to. Path/file crumbs return `None`.
    pub fn breadcrumb_target_at(&self, col: u16, row: u16) -> Option<(u32, u32)> {
        if self.breadcrumb_y != Some(row) {
            return None;
        }
        self.breadcrumb_ranges
            .iter()
            .find(|&&(x, w, _)| col >= x && col < x + w)
            .and_then(|&(_, _, target)| target)
    }

    /// The index in `breadcrumbs` of the crumb drawn under `(col, row)`,
    /// path or symbol alike (#1608).
    pub fn breadcrumb_index_at(&self, col: u16, row: u16) -> Option<usize> {
        if self.breadcrumb_y != Some(row) {
            return None;
        }
        self.breadcrumb_ranges
            .iter()
            .position(|&(x, w, _)| col >= x && col < x + w)
    }

    /// Where crumb `i`'s picker opens: the row under the bar, at the
    /// crumb's first cell. `None` while the bar is hidden or the crumb did
    /// not fit on it.
    pub fn breadcrumb_anchor(&self, i: usize) -> Option<(u16, u16)> {
        let y = self.breadcrumb_y?;
        let &(x, _, _) = self.breadcrumb_ranges.get(i)?;
        Some((x, y + 1))
    }
}

/// Tab titles for a strip, with VS Code's labelFormat disambiguation
/// (#167): colliding file names gain the SHORTEST distinguishing
/// trailing directory context (`main.rs — alpha` beside
/// `main.rs — beta`; deeper only when the parents collide too), and
/// unique titles stay bare. Diff/preview tabs keep their own labels.
pub(crate) fn disambiguated_tab_labels(editors: &[Editor]) -> Vec<String> {
    let base: Vec<String> = editors.iter().map(tab_label).collect();
    let mut out = base.clone();
    // Group by FILE NAME, not the rendered label: decorations (the dirty
    // dot) are part of the base string, and keying on it let a dirty tab
    // escape its collision group.
    let mut groups: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, e) in editors.iter().enumerate() {
        // A symbol tab's label already differs from its file's tab, and the
        // two share a path, so no parent suffix could tell them apart.
        // A proposal merge's scratch copy is labelled as one, and its
        // parent is a cache token that would only add noise (#1353).
        if e.diff.is_none()
            && e.symbol_view.is_none()
            && !e.merge.as_ref().is_some_and(|m| m.proposal_from.is_some())
            && let Some(name) = e.path.as_deref().and_then(|p| p.file_name())
        {
            groups
                .entry(name.to_string_lossy().into_owned())
                .or_default()
                .push(i);
        }
    }
    for idxs in groups.into_values() {
        if idxs.len() < 2 {
            continue;
        }
        // Walk as deep as the longest parent in the group: two DISTINCT
        // paths sharing a filename must differ somewhere in their
        // parents, so the full-parent suffixes are always distinct — a
        // fixed cap could leave deep-shared tails identical (#168
        // review).
        let max_depth = idxs
            .iter()
            .filter_map(|&i| editors[i].path.as_deref())
            .filter_map(|p| p.parent())
            .map(|p| p.components().count())
            .max()
            .unwrap_or(1)
            .max(1);
        for depth in 1..=max_depth {
            let sufs: Vec<String> = idxs
                .iter()
                .map(|&i| {
                    dir_suffix(
                        editors[i]
                            .path
                            .as_deref()
                            .unwrap_or(std::path::Path::new("")),
                        depth,
                    )
                })
                .collect();
            let mut seen = std::collections::HashSet::new();
            let all_unique = sufs.iter().all(|s| seen.insert(s.clone()));
            if all_unique || depth == max_depth {
                for (k, &i) in idxs.iter().enumerate() {
                    if !sufs[k].is_empty() {
                        out[i] = format!("{} — {}", base[i], sufs[k]);
                    }
                }
                break;
            }
        }
    }
    out
}

/// The last `depth` directory components of `path`'s parent, joined
/// with `/` — the trailing context the disambiguated label shows.
pub(super) fn dir_suffix(path: &std::path::Path, depth: usize) -> String {
    let Some(parent) = path.parent() else {
        return String::new();
    };
    let comps: Vec<String> = parent
        .components()
        .rev()
        .take(depth)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    comps.into_iter().rev().collect::<Vec<_>>().join("/")
}

/// Whether `path` is gone from disk (#900): not there at all, as opposed to
/// there but unreadable, which a deleted tab's save must not treat alike.
pub(super) fn gone_from_disk(path: &Path) -> bool {
    matches!(std::fs::metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

pub(super) fn tab_label(e: &Editor) -> String {
    if let Some(diff) = e.diff.as_ref() {
        let l = diff
            .left_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| diff.left_path.to_string_lossy().into_owned());
        // Synthetic single-sided diffs (deletion view, raw `git diff`
        // text view) leave `right_path` empty / `/dev/null` so the tab
        // label collapses to just the left label instead of trailing a
        // misleading "↔ null".
        let r_is_real = diff.right_path != std::path::Path::new("/dev/null")
            && !diff.right_path.as_os_str().is_empty();
        if r_is_real {
            let r = diff
                .right_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| diff.right_path.to_string_lossy().into_owned());
            return format!("{l} \u{2194} {r}");
        }
        return l;
    }
    let name = match &e.path {
        Some(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| String::from("untitled")),
        None => match e.untitled {
            Some(n) => format!("Untitled-{n}"),
            None => String::from("untitled"),
        },
    };
    // The merge editor is text underneath (dirty dot still applies), but
    // the tab says which flavour of the file it is showing.
    let name = if e.merge.as_ref().is_some_and(|m| m.proposal_from.is_some()) {
        format!("{name} (proposal merge)")
    } else if e.merge.is_some() {
        format!("{name} (merge)")
    } else if let Some(view) = e.symbol_view.as_ref() {
        // A symbol tab (#369) leads with its symbol; the file says where.
        format!("{} \u{b7} {name}", view.name)
    } else {
        name
    };
    // The lock VS Code shows on a file the user can't write (#1379).
    let name = if e.write_protected {
        format!("{name} \u{f023}")
    } else {
        name
    };
    // VS Code's mark on a tab whose file is gone from disk (#900).
    let name = if e.deleted_on_disk() {
        format!("{name} (deleted)")
    } else {
        name
    };
    if e.dirty {
        format!("\u{25cf} {name}")
    } else {
        name
    }
}
