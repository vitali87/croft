//! The gutter and its marks: breakpoints, bookmarks, git change marks and blame, the
//! overview ruler, line numbers, and keeping marks on their lines as text moves.

use super::*;

/// How a buffer line differs from its committed (HEAD) version, for the git
/// gutter. The colour carries the meaning, exactly like VS Code's gutter:
/// green added, blue modified, red where lines were deleted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GitMark {
    Added,
    Modified,
    /// One or more lines were removed just above this surviving line (or at
    /// end of file, on the last line). VS Code paints a triangle here; the
    /// gutter shows a red bar on the boundary line.
    Deleted,
}

/// What the inline blame says of the cursor's line (#1556).
#[derive(Clone, Copy, Debug)]
pub enum LineBlame<'a> {
    /// The line differs from HEAD: the annotation reads `Uncommitted changes`.
    Uncommitted,
    /// The commit that last changed the line.
    Commit(&'a crate::git::BlameCommit),
}

/// One tick on the editor's overview ruler: the whole-file summary VS Code and
/// Zed paint down the scrollbar track, so a problem or a search hit outside the
/// viewport is still visible.
///
/// The terminal gives the ruler a single column, where VS Code has three lanes,
/// so a row that several marks land on shows only the most urgent one. `Ord` IS
/// that priority — declaration order runs least to most urgent, and the render
/// takes the `max` of everything mapping to a row. A broken build must never be
/// hidden behind a whitespace change.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum OverviewMark {
    GitAdded,
    GitModified,
    GitDeleted,
    SearchMatch,
    Warning,
    Error,
}

impl OverviewMark {
    /// The tick's colour, matching the in-editor decoration it summarises:
    /// the git gutter's own three colours, the diagnostic underline's red and
    /// amber, and the search panel's highlight hue.
    pub(super) fn color(self, theme: crate::theme::Theme) -> Color {
        match self {
            OverviewMark::GitAdded => theme.git_added(),
            OverviewMark::GitModified => theme.git_modified(),
            OverviewMark::GitDeleted => theme.git_deleted(),
            OverviewMark::SearchMatch => theme.ui(Color::Rgb(0xea, 0x9c, 0x2a)),
            OverviewMark::Warning => theme.ui(Color::Rgb(0xcc, 0xa7, 0x00)),
            OverviewMark::Error => theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c)),
        }
    }
}

/// What a line's sign cell, the gutter's first column, shows: see
/// [`Editor::sign_glyph`].
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SignGlyph {
    /// The debugger is stopped on this line.
    Stop,
    /// A breakpoint, in the look its kind wears.
    Breakpoint(BreakpointMark),
    /// An AI stream writes on this line; a click cancels it.
    StreamStop,
    /// The reader's bookmark.
    Bookmark,
    /// A test is defined on this line; a click runs the named test.
    TestPlay(String),
}

/// The kind of breakpoint a [`SignGlyph::Breakpoint`] marks.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum BreakpointMark {
    /// The adapter could not bind it.
    Unverified,
    /// It pauses only on a condition or a hit count.
    Conditional,
    /// It logs a message instead of pausing.
    Logpoint,
    /// A plain, live breakpoint.
    Plain,
}

impl Editor {
    /// Toggle a debugger breakpoint on the cursor's current line for the active
    /// file. Returns `(path, 1-based line, now_set)` where `now_set` is `true`
    /// if a breakpoint was just added and `false` if one was removed, or `None`
    /// when no file is open. Empty sets are dropped so the map stays clean for
    /// the gutter renderer.
    pub fn toggle_breakpoint(&mut self) -> Option<(PathBuf, usize, bool)> {
        self.toggle_breakpoint_line(self.cursor_row + 1) // gutter + DAP are 1-based
    }

    /// Toggle a breakpoint on an explicit 1-based `line` (rather than the
    /// cursor). Drives the gutter right-click "Add / Remove Breakpoint" menu,
    /// where the target line is the row the user clicked, not where the caret
    /// happens to sit. Returns `(path, line, now_set)`, or `None` with no open
    /// file.
    pub fn toggle_breakpoint_line(&mut self, line: usize) -> Option<(PathBuf, usize, bool)> {
        let path = self.path.clone()?;
        let set = self.breakpoints.entry(path.clone()).or_default();
        let now_set = if set.remove(&line) {
            // Removing a breakpoint removes the whole breakpoint: an
            // orphaned condition or log message would silently re-attach to
            // the next plain breakpoint set on this line (a resurrected
            // logpoint never pauses at all).
            if let Some(conds) = self.breakpoint_conditions.get_mut(&path) {
                conds.remove(&line);
                if conds.is_empty() {
                    self.breakpoint_conditions.remove(&path);
                }
            }
            if let Some(logs) = self.breakpoint_logs.get_mut(&path) {
                logs.remove(&line);
                if logs.is_empty() {
                    self.breakpoint_logs.remove(&path);
                }
            }
            if let Some(hits) = self.breakpoint_hit_conditions.get_mut(&path) {
                hits.remove(&line);
                if hits.is_empty() {
                    self.breakpoint_hit_conditions.remove(&path);
                }
            }
            false
        } else {
            set.insert(line);
            true
        };
        if set.is_empty() {
            self.breakpoints.remove(&path);
        }
        Some((path, line, now_set))
    }

    /// The 0-based buffer line under screen `(col, row)`, but ONLY when `col`
    /// falls in the left gutter (glyph margin + line-number columns) rather
    /// than the text body. Returns `None` over the body, past the last line,
    /// or outside the viewport. Complements [`Editor::buffer_pos_at`] (which
    /// resolves body clicks) and drives the gutter right-click breakpoint menu,
    /// mirroring VS Code's glyph-margin context menu.
    pub fn gutter_line_at(&self, col: u16, row: u16) -> Option<usize> {
        if self.last_inner.height == 0 || self.lines.is_empty() {
            return None;
        }
        if row < self.last_inner.y || row >= self.last_inner.y + self.last_inner.height {
            return None;
        }
        // text_x mirrors `buffer_pos_at`: the gutter occupies every column to
        // its left, so `col < text_x` is exactly "in the gutter".
        let text_x = self.last_inner.x + self.last_gutter_width + 1;
        if col < self.last_inner.x || col >= text_x {
            return None;
        }
        // `last_wrap_rows` is the row->line map the render built, which already
        // skips folded-away lines, so it is correct for both wrap and fold.
        // Before the first paint it is empty; fall back to the linear map, which
        // is exact then since nothing can be wrapped or folded yet.
        let vis_row = (row - self.last_inner.y) as usize;
        let line = if self.last_wrap_rows.is_empty() {
            self.scroll + vis_row
        } else {
            self.text_row(vis_row)?.0
        };
        (line < self.lines.len()).then_some(line)
    }

    /// The 0-based line whose number `(col, row)` is on (#1570): only the
    /// number's own columns, right of the sign margin (the breakpoint lane
    /// and the fold chevron) and left of the spacer before the change bar,
    /// on any visual row of a source line. A press there selects the line,
    /// as in VS Code. With line numbers off there are no such columns, and
    /// views without source lines have none either.
    pub fn line_number_line_at(&self, col: u16, row: u16) -> Option<usize> {
        let x = self.last_inner.x;
        let numbers = x + 2..x + self.last_gutter_width.saturating_sub(1);
        if !numbers.contains(&col) || self.has_non_text_view() || self.markdown_preview.is_some() {
            return None;
        }
        self.gutter_line_at(col, row)
    }

    /// The 0-based line whose breakpoint lane `(col, row)` is on (#1544): the
    /// sign cell left of the fold chevron, where breakpoints draw, on any
    /// visual row of a source line. A left click there toggles a breakpoint,
    /// as a click in VS Code's glyph margin does. The line number and the
    /// chevron keep their own clicks, and views without source lines (a
    /// diff, sheet, image, hex or log view, a Markdown preview) have no lane.
    pub fn breakpoint_lane_line_at(&self, col: u16, row: u16) -> Option<usize> {
        if col != self.last_inner.x || self.has_non_text_view() || self.markdown_preview.is_some() {
            return None;
        }
        self.gutter_line_at(col, row)
    }

    /// If `(col, row)` lands on a fold chevron in the gutter, toggle that fold
    /// and return `true`. The chevron sits at `inner.x + 1` on a foldable
    /// header's first visual row (see the render's fold-chevron block).
    pub fn fold_chevron_at(&mut self, col: u16, row: u16) -> bool {
        if col != self.last_inner.x + 1 || row < self.last_inner.y {
            return false;
        }
        let Some((line, start, _)) = self.text_row((row - self.last_inner.y) as usize) else {
            return false;
        };
        // The chevron draws only on a header's first visual row.
        if self.wrap_enabled() && start != 0 {
            return false;
        }
        if self.is_foldable(line) {
            self.toggle_fold(line);
            return true;
        }
        false
    }

    /// If `(col, row)` lands on a test fn's gutter play glyph, the test's
    /// name — the app runs it. Same geometry as the fold chevron, one cell
    /// left (`inner.x`, the sign margin the breakpoint dot shares).
    pub fn test_glyph_at(&self, col: u16, row: u16) -> Option<String> {
        match self.sign_glyph(self.sign_line_at(col, row)?) {
            Some(SignGlyph::TestPlay(name)) => Some(name),
            _ => None,
        }
    }

    /// Whether `(col, row)` lands on the AI-stream stop button in the sign
    /// margin: same geometry as [`test_glyph_at`](Self::test_glyph_at), only
    /// where the square is drawn. A debugger glyph that outranks it on the
    /// streamed line takes the click instead.
    pub fn stream_stop_at(&self, col: u16, row: u16) -> bool {
        self.stream_stop_line.is_some()
            && self
                .sign_line_at(col, row)
                .is_some_and(|line| self.sign_glyph(line) == Some(SignGlyph::StreamStop))
    }

    /// The 0-based line whose sign cell is at `(col, row)`: the gutter's
    /// first column, on a line's first visual row, where its glyph draws.
    fn sign_line_at(&self, col: u16, row: u16) -> Option<usize> {
        if col != self.last_inner.x || row < self.last_inner.y {
            return None;
        }
        let (line, start, _) = self.text_row((row - self.last_inner.y) as usize)?;
        (!self.wrap_enabled() || start == 0).then_some(line)
    }

    /// The glyph the sign cell of 0-based `line` shows, the one rule the
    /// render pass paints and a click answers by. One glyph per cell,
    /// highest rank first: the debugger's stop arrow and a breakpoint
    /// report machine state and are never hidden, then the AI stream's stop
    /// square, then the reader's bookmark, then a test's play bead.
    pub(super) fn sign_glyph(&self, line: usize) -> Option<SignGlyph> {
        let here = line + 1; // the debugger's lines are 1-based
        if let Some(path) = self.path.as_deref() {
            if self
                .stop_line
                .as_ref()
                .is_some_and(|(p, l)| p == path && *l == here)
            {
                return Some(SignGlyph::Stop);
            }
            if self
                .breakpoints
                .get(path)
                .is_some_and(|s| s.contains(&here))
            {
                return Some(SignGlyph::Breakpoint(self.breakpoint_mark(path, here)));
            }
        }
        if self.stream_stop_line == Some(line) {
            return Some(SignGlyph::StreamStop);
        }
        if self.is_bookmarked(line) {
            return Some(SignGlyph::Bookmark);
        }
        crate::testing::locate::test_fn_on_line(self.path.as_deref(), &self.lines, line)
            .map(SignGlyph::TestPlay)
    }

    /// Which breakpoint glyph `path`'s 1-based line `here` wears: hollow
    /// when the adapter could not bind it, a diamond for a condition or a
    /// logpoint, a dot for a plain one.
    fn breakpoint_mark(&self, path: &Path, here: usize) -> BreakpointMark {
        if self
            .unverified_breakpoints
            .get(path)
            .is_some_and(|s| s.contains(&here))
        {
            return BreakpointMark::Unverified;
        }
        // A hit count is a condition on the pause (#611), so it wears the
        // conditional diamond too, as in VS Code.
        if self
            .breakpoint_conditions
            .get(path)
            .is_some_and(|c| c.contains_key(&here))
            || self
                .breakpoint_hit_conditions
                .get(path)
                .is_some_and(|h| h.contains_key(&here))
        {
            return BreakpointMark::Conditional;
        }
        if self
            .breakpoint_logs
            .get(path)
            .is_some_and(|m| m.contains_key(&here))
        {
            return BreakpointMark::Logpoint;
        }
        BreakpointMark::Plain
    }

    /// This frame's screen row of `line`'s first visual row, when visible
    /// (None = scrolled off or folded away). Anchors line-tied popups.
    pub fn screen_row_of_line(&self, line: usize) -> Option<u16> {
        self.last_wrap_rows
            .iter()
            .position(|r| matches!(r, VisRow::Text { line: l, .. } if *l == line))
            .map(|idx| self.last_inner.y.saturating_add(idx as u16))
    }

    /// Toggle a bookmark on `line` (1-based, like breakpoints) in the open
    /// file. Returns
    /// `Some(true)` when a bookmark was added, `Some(false)` when one was
    /// removed, and `None` when no file is open (a scratch buffer has no
    /// path to key the set by).
    pub fn toggle_bookmark_at(&mut self, line: usize) -> Option<bool> {
        let path = self.path.clone()?;
        // Settle the existing marks against the buffer before adding one, so
        // the shadow seeded below describes every mark in the set.
        self.sync_bookmarks_to_buffer();
        let set = self.bookmarks.entry(path.clone()).or_default();
        let added = if set.contains(&line) {
            set.remove(&line);
            false
        } else {
            set.insert(line);
            true
        };
        // Drop the empty set so `bookmarked_lines` and the gutter never walk
        // a map full of hollow entries for files whose marks were all cleared.
        if set.is_empty() {
            self.bookmarks.remove(&path);
        }
        self.seed_bookmark_shadow();
        Some(added)
    }

    /// Toggle a bookmark on the cursor's line in the open file; see
    /// [`Self::toggle_bookmark_at`].
    pub fn toggle_bookmark(&mut self) -> Option<bool> {
        self.toggle_bookmark_at(self.cursor_row + 1)
    }

    /// 1-based bookmark lines for the open file, ascending. Empty when no
    /// file is open or the file has no marks.
    pub fn bookmarked_lines(&self) -> Vec<usize> {
        // A mark past the last line is never a target: jumping to it would
        // clamp back onto the last row, and from there `next` would pick it
        // again forever instead of wrapping. The edit paths clamp the set
        // already; this keeps navigation and the count honest regardless.
        let len = self.lines.len();
        self.path
            .as_deref()
            .and_then(|p| self.bookmarks.get(p))
            .map(|s| {
                s.iter()
                    .copied()
                    .filter(|&l| (1..=len).contains(&l))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Take over a saved set of bookmarks (a restored session) for the open
    /// file, trimmed to the lines it still has.
    pub fn restore_bookmarks(
        &mut self,
        marks: &std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>,
    ) {
        if marks.is_empty() {
            return;
        }
        self.bookmarks = marks.clone();
        self.sync_bookmarks_to_buffer();
    }

    /// Take `path`'s marks as another tab holds them (`None`: it has none),
    /// so every tab keeps one set per file. Trimmed to the buffer when
    /// `path` is the open file, whose later edits then carry them.
    pub fn adopt_bookmarks(
        &mut self,
        path: &Path,
        marks: Option<&std::collections::BTreeSet<usize>>,
    ) {
        match marks {
            Some(set) if !set.is_empty() => {
                self.bookmarks.insert(path.to_path_buf(), set.clone());
            }
            _ => {
                self.bookmarks.remove(path);
            }
        }
        if self.path.as_deref() == Some(path) {
            self.sync_bookmarks_to_buffer();
        }
    }

    /// This tab's bookmarks on the files as saved: the open file's marks
    /// follow its unsaved edits, so with the tab closing on those edits
    /// each goes back to its line on disk, decoded as the tab decoded it.
    pub fn bookmarks_on_disk(
        &self,
    ) -> std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>> {
        let mut marks = self.bookmarks.clone();
        if self.dirty
            && let Some(path) = self.path.as_deref()
            && let Some(set) = marks.get_mut(path)
            && let Ok(bytes) = std::fs::read(path)
        {
            let (text, _, _) = self.encoding.decode(&bytes);
            *set = carry_marks(set, &self.lines, &split_into_lines(&text));
        }
        marks.retain(|_, set| !set.is_empty());
        marks
    }

    /// Whether 0-based `line` carries a bookmark in the open file.
    pub fn is_bookmarked(&self, line: usize) -> bool {
        self.path
            .as_deref()
            .and_then(|p| self.bookmarks.get(p))
            .is_some_and(|s| s.contains(&(line + 1)))
    }

    /// Move the cursor to the next bookmark below it, wrapping to the first
    /// one at the top of the file. Returns the 1-based line jumped to, or
    /// `None` when the file has no bookmarks. A file with a single bookmark
    /// on the cursor's own line stays put and reports that line, so the key
    /// never looks broken.
    pub fn goto_next_bookmark(&mut self) -> Option<usize> {
        let marks = self.bookmarked_lines();
        let here = self.cursor_row + 1;
        let target = *marks
            .iter()
            .find(|&&l| l > here)
            .or_else(|| marks.first())?;
        self.jump_to_bookmark(target);
        Some(target)
    }

    /// Mirror of [`goto_next_bookmark`](Self::goto_next_bookmark) walking
    /// upwards, wrapping to the last bookmark in the file.
    pub fn goto_prev_bookmark(&mut self) -> Option<usize> {
        let marks = self.bookmarked_lines();
        let here = self.cursor_row + 1;
        let target = *marks
            .iter()
            .rev()
            .find(|&&l| l < here)
            .or_else(|| marks.last())?;
        self.jump_to_bookmark(target);
        Some(target)
    }

    /// Drop every bookmark in the open file. Returns how many were removed.
    pub fn clear_bookmarks(&mut self) -> usize {
        self.bookmark_shadow = None;
        self.path
            .clone()
            .and_then(|p| self.bookmarks.remove(&p))
            .map(|s| s.len())
            .unwrap_or(0)
    }

    /// Whether the open buffer has anything keyed by line that must follow
    /// edits: bookmarks or breakpoint state for its file, or collapsed folds
    /// (#1509), which an untitled buffer has too.
    pub(super) fn has_line_marks(&self, path: Option<&Path>) -> bool {
        !self.folded.is_empty()
            || path.is_some_and(|p| self.bookmarks.contains_key(p) || self.has_breakpoints_in(p))
    }

    /// Whether `path` has any line-keyed breakpoint state to carry through
    /// edits (see [`Self::sync_bookmarks_to_buffer`]).
    pub(super) fn has_breakpoints_in(&self, path: &Path) -> bool {
        self.breakpoints.contains_key(path)
            || self.breakpoint_conditions.contains_key(path)
            || self.breakpoint_logs.contains_key(path)
            || self.breakpoint_hit_conditions.contains_key(path)
    }

    /// Record the text the open buffer's marks describe, ahead of an edit
    /// (called from [`push_undo`](Self::push_undo), which every user edit
    /// passes through before it mutates). A no-op when the buffer has no
    /// marks or the shadow already belongs to it. Keyed by the optional path
    /// so an untitled buffer's folds follow edits too.
    pub(super) fn seed_bookmark_shadow(&mut self) {
        let path = self.path.as_deref();
        if !self.has_line_marks(path)
            || self
                .bookmark_shadow
                .as_ref()
                .is_some_and(|(p, _)| p.as_deref() == path)
        {
            return;
        }
        self.bookmark_shadow = Some((self.path.clone(), self.lines.clone()));
    }

    /// Re-align the open file's bookmarks, breakpoints and collapsed folds
    /// with the buffer after it changed.
    /// The single choke point for line shifts: called from
    /// [`mark_buffer_changed`](Self::mark_buffer_changed) (every edit),
    /// [`restore_snapshot`](Self::restore_snapshot) (undo / redo) and the
    /// file loaders (reload, reopen with encoding). When the shadow
    /// describes this buffer, marks are mapped through [`line_mapper`];
    /// either way, any mark left past the last line is dropped, so a change
    /// whose mapping is unknown (a reload with no shadow) still never leaves
    /// a mark beyond the buffer.
    pub(super) fn sync_bookmarks_to_buffer(&mut self) {
        if self.bookmark_sync_paused {
            return;
        }
        let path = self.path.clone();
        if !self.has_line_marks(path.as_deref()) {
            self.bookmark_shadow = None;
            return;
        }
        let len = self.lines.len();
        let mut folds_moved = false;
        let mut shadow = match self.bookmark_shadow.take() {
            Some((shadow_path, old)) if shadow_path == path => {
                let map = line_mapper(&old, &self.lines);
                if let Some(path) = &path {
                    if let Some(set) = self.bookmarks.get_mut(path) {
                        *set = set.iter().filter_map(|&l| map(l)).collect();
                    }
                    // Breakpoints follow the same edits: they were plain line
                    // numbers, so an Enter above one left it on the line that
                    // moved into its place, and the debugger stopped there.
                    if let Some(set) = self.breakpoints.get_mut(path) {
                        *set = set.iter().filter_map(|&l| map(l)).collect();
                    }
                    for per_line in [
                        &mut self.breakpoint_conditions,
                        &mut self.breakpoint_logs,
                        &mut self.breakpoint_hit_conditions,
                    ] {
                        if let Some(m) = per_line.get_mut(path) {
                            *m = std::mem::take(m)
                                .into_iter()
                                .filter_map(|(l, v)| map(l).map(|n| (n, v)))
                                .collect();
                        }
                    }
                }
                // Folds too: their headers are line numbers, and the render
                // guard dropped every fold on any change in line count. An
                // edit that keeps the count (lines deleted on one side of a
                // fold and as many inserted on the other) can still move a
                // header, so remap whenever the text changed.
                if !self.folded.is_empty() && old != self.lines {
                    let moved: std::collections::BTreeSet<usize> = self
                        .folded
                        .iter()
                        .filter_map(|&h| map(h + 1).map(|l| l - 1))
                        .collect();
                    if moved != self.folded || old.len() != len {
                        self.folded = moved;
                        folds_moved = true;
                    }
                }
                drop(map);
                old
            }
            _ => Vec::new(),
        };
        if folds_moved {
            self.fold_epoch_lines = len;
            self.refresh_fold_tables();
            self.rebuild_hidden_ranges();
        }
        if let Some(path) = &path {
            let in_range = |l: &usize| (1..=len).contains(l);
            if let Some(set) = self.bookmarks.get_mut(path) {
                set.retain(in_range);
                if set.is_empty() {
                    self.bookmarks.remove(path);
                }
            }
            if let Some(set) = self.breakpoints.get_mut(path) {
                set.retain(in_range);
                if set.is_empty() {
                    self.breakpoints.remove(path);
                }
            }
            for per_line in [
                &mut self.breakpoint_conditions,
                &mut self.breakpoint_logs,
                &mut self.breakpoint_hit_conditions,
            ] {
                if let Some(m) = per_line.get_mut(path) {
                    m.retain(|l, _| in_range(l));
                    if m.is_empty() {
                        per_line.remove(path);
                    }
                }
            }
        }
        if !self.has_line_marks(path.as_deref()) {
            return;
        }
        // Bring the shadow up to date in place rather than cloning the whole
        // buffer: this runs on every keystroke in a marked file, and a
        // keystroke changes one row. Only differing rows are copied, into
        // the allocations they already have.
        shadow.truncate(len);
        for (old, new) in shadow.iter_mut().zip(&self.lines) {
            if old != new {
                old.clone_from(new);
            }
        }
        let have = shadow.len();
        shadow.extend(self.lines[have..].iter().cloned());
        self.bookmark_shadow = Some((path, shadow));
    }

    /// Put the cursor on 1-based `line`, clamped to the buffer, unfolding
    /// anything hiding it and centring it in the viewport — a bookmark
    /// inside a collapsed region must still be reachable. The selection and
    /// any secondary carets go, so the next keystroke edits only that line.
    pub fn jump_to_bookmark(&mut self, line: usize) {
        if self.lines.is_empty() {
            return;
        }
        self.selection = None;
        self.collapse_carets();
        self.goto_line_centered(line.saturating_sub(1));
        self.reveal_cursor_fold();
        self.ensure_cursor_col_visible();
    }

    /// 1-based breakpoint lines for `path`, ascending, for a DAP
    /// `setBreakpoints` request.
    pub fn breakpoint_lines(&self, path: &Path) -> Vec<u32> {
        self.breakpoints
            .get(path)
            .map(|s| s.iter().map(|&l| l as u32).collect())
            .unwrap_or_default()
    }

    /// Breakpoints for `path` as DAP `SourceBreakpoint`s, attaching any stored
    /// per-line condition and log message. `lines` is the path's breakpoint
    /// set (passed in to avoid a second lookup at the call site).
    pub fn source_breakpoints(
        &self,
        path: &Path,
        lines: &std::collections::BTreeSet<usize>,
    ) -> Vec<crate::dap::session::SourceBreakpoint> {
        let conds = self.breakpoint_conditions.get(path);
        let logs = self.breakpoint_logs.get(path);
        let hits = self.breakpoint_hit_conditions.get(path);
        lines
            .iter()
            .map(|&l| crate::dap::session::SourceBreakpoint {
                line: l as u32,
                condition: conds.and_then(|c| c.get(&l)).cloned(),
                log_message: logs.and_then(|m| m.get(&l)).cloned(),
                hit_condition: hits.and_then(|h| h.get(&l)).cloned(),
            })
            .collect()
    }

    /// Install (or clear) the git-gutter HEAD baseline for `path`. The app
    /// calls this once per file (and again after HEAD moves) with the
    /// committed version's lines; `None` means untracked / not a repo. Forces
    /// the per-line marks to recompute on the next render.
    pub fn set_git_head_lines(&mut self, path: PathBuf, head: Option<PackedLines>) {
        self.git_baseline_for = Some(path);
        self.git_head_lines = head;
        self.git_marks_seq = u64::MAX;
    }

    /// Install (or clear) per-line git blame for `path`. The app fetches this
    /// off-thread once per file and after HEAD moves.
    pub fn set_blame(&mut self, path: PathBuf, blame: Option<crate::git::Blame>) {
        self.blame_for = Some(path);
        self.blame_lines = blame;
    }

    /// The GitLens-style inline annotation for the cursor's line, or `None`
    /// when blame is off, unavailable, or the line was edited past the blamed
    /// range. Committed lines read `author, age • summary`; a line git blames
    /// against the zero hash (a working-tree edit) reads `Uncommitted changes`.
    pub fn current_line_blame_annotation(&self) -> Option<String> {
        let note = self.committed_blame_annotation();
        if !self.provenance_overlay {
            return note;
        }
        // With the lens on, the seat that typed the cursor's line joins the
        // annotation (`Vitali, 1 minute ago • fix • you`), or stands alone
        // on a line git has nothing to say about. A line with no record
        // adds nothing: it is unknown, not "you".
        // A `None` from the git half can mean "suppressed here" (a conflict
        // block, whose [Accept …] actions own this cell) as well as "nothing
        // to say", so the seat never stands alone on those rows.
        let in_conflict =
            crate::merge::conflict_containing(&self.conflicts, self.cursor_row).is_some();
        match (note, self.provenance.seat(self.cursor_row)) {
            (Some(n), Some(seat)) => Some(format!("{n} • {}", seat.label())),
            (Some(n), None) => Some(n),
            (None, Some(seat)) if self.blame_enabled && !in_conflict => Some(seat.label()),
            (None, _) => None,
        }
    }

    /// The git half of the annotation: author, age and summary of the
    /// cursor's line, or the uncommitted marker.
    pub(super) fn committed_blame_annotation(&self) -> Option<String> {
        Some(match self.cursor_line_blame()? {
            LineBlame::Uncommitted => "Uncommitted changes".to_string(),
            LineBlame::Commit(commit) => format!(
                "{}, {} • {}",
                commit.author,
                crate::git::humanize_age(commit.age_secs),
                commit.summary
            ),
        })
    }

    /// What the inline blame says of the cursor's line, or `None` when it
    /// says nothing: blame is off or unavailable, or the line was edited
    /// past the blamed range. Git: Open Commit for Line and the annotation's
    /// hover and click act on its commit (#1556).
    pub fn cursor_line_blame(&self) -> Option<LineBlame<'_>> {
        if !self.blame_enabled {
            return None;
        }
        // Inside a conflict block the annotation is noise (markers have no
        // meaningful author) and on the header row it painted over the
        // [Accept …] actions — the accept affordance wins.
        if crate::merge::conflict_containing(&self.conflicts, self.cursor_row).is_some() {
            return None;
        }
        // A reused tab (the preview) switches path before the new fetch
        // lands; the old file's blame must not paint on the new one.
        if self.blame_for != self.path {
            return None;
        }
        let blame = self.blame_lines.as_ref()?;
        // A line the git gutter flags as added/modified diverges from HEAD, so
        // its committed author is meaningless — show the uncommitted marker
        // (this also covers unsaved edits, since the gutter recomputes per
        // edit while `blame` is only refetched per file / HEAD change).
        if matches!(
            self.git_mark_at(self.cursor_row),
            Some(GitMark::Added | GitMark::Modified)
        ) {
            return Some(LineBlame::Uncommitted);
        }
        // Insertions/deletions shift line indices out from under the blamed
        // snapshot; only trust the 1:1 mapping while the line counts match.
        if blame.len() != self.lines.len() {
            return None;
        }
        let (commit, text) = blame.line(self.cursor_row)?;
        // Same count is not same lines: a cut and paste further down keeps
        // the count and shifts every row between, each then wearing the
        // blame of the line that used to sit there.
        if text.is_some_and(|t| Some(t) != self.lines.get(self.cursor_row).map(String::as_str)) {
            return None;
        }
        Some(if commit.uncommitted {
            LineBlame::Uncommitted
        } else {
            LineBlame::Commit(commit)
        })
    }

    /// The hash of the commit the blame annotation painted this frame names
    /// at screen cell `(col, row)`, if it is there (#1556).
    pub fn blame_commit_at(&self, col: u16, row: u16) -> Option<&str> {
        self.blame_span
            .as_ref()
            .filter(|(y, xs, _)| *y == row && xs.contains(&col))
            .map(|(_, _, hash)| hash.as_str())
    }

    /// Recompute the per-line git marks from the cached HEAD baseline if the
    /// buffer has changed since they were last built. Cheap no-op when the
    /// `edit_seq` is unchanged, so the render loop can call it every frame.
    pub(super) fn refresh_git_marks(&mut self) {
        use crate::widgets::diff::DiffRow;
        if self.git_marks_seq == self.edit_seq {
            return;
        }
        self.git_marks_seq = self.edit_seq;
        self.git_marks.clear();
        let Some(head) = self.git_head_lines.as_ref() else {
            return;
        };
        // `build_diff_rows` pairs consecutive delete+insert into `Replaced`, so
        // a bare `Removed` run is a real deletion: attach a `Deleted` marker to
        // the next surviving line (the gutter's deletion boundary, VS Code-style).
        let head: Vec<&str> = head.iter().collect();
        let rows = crate::widgets::diff::build_diff_rows(&head, &self.lines);
        let mut pending_delete = false;
        for row in &rows {
            match *row {
                DiffRow::Added { right } => {
                    self.git_marks.insert(right, GitMark::Added);
                    pending_delete = false;
                }
                DiffRow::Replaced { right, .. } => {
                    self.git_marks.insert(right, GitMark::Modified);
                    pending_delete = false;
                }
                DiffRow::Removed { .. } => pending_delete = true,
                DiffRow::Equal { right, .. } => {
                    if pending_delete {
                        self.git_marks.insert(right, GitMark::Deleted);
                        pending_delete = false;
                    }
                }
            }
        }
        // A deletion at end of file has no surviving line after it: mark the
        // last line so a trailing removal still shows.
        if pending_delete && !self.lines.is_empty() {
            self.git_marks
                .entry(self.lines.len() - 1)
                .or_insert(GitMark::Deleted);
        }
    }

    /// The overview ruler's ticks for a track `track_rows` cells tall: one
    /// slot per track row, holding the most urgent mark of every buffer line
    /// that maps to it (see [`OverviewMark`]'s `Ord`).
    ///
    /// The whole file is summarised, not just the viewport — that is the point
    /// of a ruler. Lines are compressed onto the track proportionally, the same
    /// mapping the scrollbar thumb uses, so a tick sits beside the part of the
    /// thumb's travel that would bring it on screen.
    pub fn overview_ruler_rows(&mut self, track_rows: u16) -> Vec<Option<OverviewMark>> {
        self.overview_ruler_rows_wrapped(track_rows, None)
    }

    /// [`overview_ruler_rows`](Self::overview_ruler_rows) for a buffer
    /// soft-wrapped at `wrap_width` columns. The thumb travels over visual
    /// rows, not lines, so a tick goes where its line's first visual row
    /// sits: with long lines above it, line 50 of 100 can start near the
    /// bottom of the content, and a line-count mapping would put it where
    /// the thumb reveals something else.
    pub fn overview_ruler_rows_wrapped(
        &mut self,
        track_rows: u16,
        wrap_width: Option<usize>,
    ) -> Vec<Option<OverviewMark>> {
        let rows = track_rows as usize;
        let mut out = vec![None; rows];
        if rows == 0 || self.lines.is_empty() {
            return out;
        }
        // Content position of each line's first row, plus the total. In wrap
        // mode the text rows come from a cache that only an edit or a width
        // change invalidates; comment-box rows (few) are added per call.
        let n = self.lines.len();
        let (starts, boxes): (Vec<usize>, Vec<(usize, usize)>) = match wrap_width {
            Some(w) => {
                let fresh = matches!(
                    &self.ruler_starts_cache,
                    Some((seq, cw, _)) if *seq == self.edit_seq && *cw == w
                );
                if !fresh {
                    let mut acc = 0;
                    let text: Vec<usize> = (0..=n)
                        .map(|l| {
                            let at = acc;
                            if l < n {
                                acc += self.line_visual_rows(l, w);
                            }
                            at
                        })
                        .collect();
                    self.ruler_starts_cache = Some((self.edit_seq, w, text));
                }
                let mut boxes: Vec<(usize, usize)> = (0..self.comment_boxes.len())
                    .map(|i| (self.comment_boxes[i].line, self.comment_box_height(i, w)))
                    .collect();
                boxes.sort_unstable();
                let starts = self
                    .ruler_starts_cache
                    .as_ref()
                    .map(|(_, _, t)| t.clone())
                    .unwrap_or_default();
                (starts, boxes)
            }
            None => ((0..=n).collect(), Vec::new()),
        };
        // Box rows hang under their line, so they push down every later line.
        let box_rows_before = |line: usize| -> usize {
            boxes
                .iter()
                .take_while(|(l, _)| *l < line)
                .map(|(_, h)| h)
                .sum()
        };
        let total = (starts[n] + box_rows_before(n)).max(1);
        let last = n - 1;
        // Only lines the search panel would actually highlight count, so the
        // ruler and the yellow runs in the text can never disagree: both ask
        // `split_for_highlight` the same question with the same options.
        let search_hits: Vec<usize> = match self.search_highlight.clone().filter(|n| !n.is_empty())
        {
            Some(needle) => {
                let opts = self.search_highlight_opts;
                let fresh = matches!(
                    &self.ruler_search_cache,
                    Some((seq, cn, co, _)) if *seq == self.edit_seq && *cn == needle && *co == opts
                );
                if !fresh {
                    let hits = self
                        .lines
                        .iter()
                        .enumerate()
                        .filter(|(_, line)| {
                            crate::widgets::search::split_for_highlight(line, &needle, opts)
                                .iter()
                                // A zero-width match (`^`, `\b`, `q*`) paints no
                                // cell in the text, so it gets no tick either.
                                .any(|(text, is_match)| *is_match && !text.is_empty())
                        })
                        .map(|(i, _)| i)
                        .collect();
                    self.ruler_search_cache = Some((self.edit_seq, needle, opts, hits));
                }
                self.ruler_search_cache
                    .as_ref()
                    .map(|(_, _, _, h)| h.clone())
                    .unwrap_or_default()
            }
            None => Vec::new(),
        };
        self.refresh_git_marks();
        let mut put = |line: usize, mark: OverviewMark| {
            let line = line.min(last);
            let row = ((starts[line] + box_rows_before(line)) * rows / total).min(rows - 1);
            out[row] = Some(out[row].map_or(mark, |cur: OverviewMark| cur.max(mark)));
        };

        for (&line, mark) in &self.git_marks {
            put(
                line,
                match mark {
                    GitMark::Added => OverviewMark::GitAdded,
                    GitMark::Modified => OverviewMark::GitModified,
                    GitMark::Deleted => OverviewMark::GitDeleted,
                },
            );
        }

        // Only lines the search panel would actually highlight count, so the
        // ruler and the yellow runs in the text can never disagree: both ask
        // `split_for_highlight` the same question with the same options.
        for &i in &search_hits {
            put(i, OverviewMark::SearchMatch);
        }

        // Diagnostics outrank everything, so they are applied last — `put`
        // keeps the max, but ordering the loops this way keeps the intent
        // readable rather than relying on the comparison alone.
        for (i, spans) in self.diagnostic_spans.iter().enumerate() {
            for (_, _, severity) in spans {
                let mark = match severity {
                    crate::lsp::manager::DiagnosticSeverity::Error => OverviewMark::Error,
                    crate::lsp::manager::DiagnosticSeverity::Warning => OverviewMark::Warning,
                    // Information and Hint are advisory; they would crowd the
                    // one column they share with real problems.
                    _ => continue,
                };
                put(i, mark);
            }
        }
        out
    }

    /// Go to Next / Previous Change in an editor tab (#1352): the first
    /// line of the next (or previous) run of change-bar lines, wrapping at
    /// either end as the diff view's F7 does. `None` when the file has no
    /// change bars.
    pub fn git_change_from(&mut self, row: usize, forward: bool) -> Option<usize> {
        self.refresh_git_marks();
        let mut lines: Vec<usize> = self.git_marks.keys().copied().collect();
        lines.sort_unstable();
        let starts: Vec<usize> = lines
            .iter()
            .copied()
            .filter(|&l| l == 0 || !self.git_marks.contains_key(&(l - 1)))
            .collect();
        if forward {
            starts
                .iter()
                .copied()
                .find(|&s| s > row)
                .or_else(|| starts.first().copied())
        } else {
            // From inside a run, "previous" is the run before this one.
            let here = starts.iter().copied().rev().find(|&s| s <= row);
            let from = match here {
                Some(s) if self.git_marks.contains_key(&row) => s,
                _ => row,
            };
            starts
                .iter()
                .copied()
                .rev()
                .find(|&s| s < from)
                .or_else(|| starts.last().copied())
        }
    }

    /// The git-gutter mark for 0-based buffer line `line`, if any. Reads the
    /// last computed marks (call after a render, or after `refresh_git_marks`).
    pub fn git_mark_at(&self, line: usize) -> Option<GitMark> {
        self.git_marks.get(&line).copied()
    }

    /// `git_mark_at` after bringing the marks up to date with the buffer,
    /// for callers outside the render pass that computes them.
    pub fn current_git_mark_at(&mut self, line: usize) -> Option<GitMark> {
        self.refresh_git_marks();
        self.git_mark_at(line)
    }

    /// The number the gutter shows for `line` (#1576), `None` for none: its
    /// 1-based number (`on`); nothing (`off`); the caret's line its own
    /// number and every other line its distance from it, counting a folded
    /// block as one line as Vim's `relativenumber` does (`relative`); or
    /// every tenth line and the caret's (`interval`), as VS Code does.
    pub fn gutter_number(&self, line: usize) -> Option<usize> {
        match self.line_numbers {
            LineNumbers::On => Some(line + 1),
            LineNumbers::Off => None,
            LineNumbers::Relative if line == self.cursor_row => Some(line + 1),
            LineNumbers::Relative => Some(self.visible_line_distance(self.cursor_row, line)),
            LineNumbers::Interval => {
                ((line + 1).is_multiple_of(10) || line == self.cursor_row).then_some(line + 1)
            }
        }
    }
}

/// Gutter line numbers (#1576, VS Code `editor.lineNumbers`): `On` (the
/// default) numbers every line, `Relative` shows each line's distance from
/// the caret's (for Vim counts such as `5j`), `Interval` every tenth line,
/// and `Off` no number column at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineNumbers {
    #[default]
    On,
    Relative,
    Interval,
    Off,
}

impl LineNumbers {
    /// The persisted config id (`line_numbers` in config.json).
    pub fn pref_id(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Relative => "relative",
            Self::Interval => "interval",
            Self::Off => "off",
        }
    }

    /// Parse the persisted id; anything unrecognised (including the empty
    /// string an older config deserializes to) is `on`.
    pub fn from_pref(id: &str) -> Self {
        match id {
            "relative" => Self::Relative,
            "interval" => Self::Interval,
            "off" => Self::Off,
            _ => Self::On,
        }
    }

    /// The palette command's cycle order: on, relative, interval, off.
    pub fn next(self) -> Self {
        match self {
            Self::On => Self::Relative,
            Self::Relative => Self::Interval,
            Self::Interval => Self::Off,
            Self::Off => Self::On,
        }
    }
}

/// Map 1-based bookmark `marks` across a buffer change from `old` to `new`.
///
/// The common prefix and suffix are trimmed first (cheap, and usually the
/// whole answer for a keystroke), then the changed middle is line-diffed, so
/// an edit that touched several places at once (multi-cursor line delete,
/// LSP formatting, Replace All with newlines, or the undo of any of them)
/// moves each mark by what changed above IT, not by one span's delta.
///
/// Per diff op: an unchanged line carries its mark to its new row; within a
/// replaced run a mark keeps its offset while that row still exists and
/// otherwise collapses onto the run's last new line (the join line a
/// backspace or a partial-line selection delete leaves behind); a mark on a
/// line deleted outright is dropped.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn shift_bookmark_lines(
    marks: &std::collections::BTreeSet<usize>,
    old: &[String],
    new: &[String],
) -> std::collections::BTreeSet<usize> {
    let map = shift_line_mapper(old, new);
    marks.iter().filter_map(|&line| map(line)).collect()
}

/// Where each 1-based line of `old` lands in `new` after an edit, for any
/// line-keyed marks (bookmarks, breakpoints): through [`shift_bookmark_lines`]'s
/// diff when the line count changed, through [`follow_moved_bookmark_lines`]'s
/// move tracking when it did not. None for a line deleted outright.
pub(super) fn line_mapper<'a>(
    old: &'a [String],
    new: &'a [String],
) -> Box<dyn Fn(usize) -> Option<usize> + 'a> {
    if old.len() != new.len() {
        shift_line_mapper(old, new)
    } else {
        Box::new(move |line| {
            let one = std::collections::BTreeSet::from([line]);
            follow_moved_bookmark_lines(&one, old, new)
                .into_iter()
                .next()
        })
    }
}

/// `marks` (1-based lines of `old`) carried to the lines they sit on in
/// `new`, another copy of the same file: a second tab of it whose unsaved
/// edits differ, or the file on disk under a buffer whose edits are being
/// discarded. Each mark moves as an edit from `old` to `new` would move it
/// (see [`line_mapper`]): one on a line deleted outright is dropped, and one
/// on a line edited in place stays on that line, as it does while typing.
pub fn carry_marks(
    marks: &std::collections::BTreeSet<usize>,
    old: &[String],
    new: &[String],
) -> std::collections::BTreeSet<usize> {
    if old == new {
        return marks.clone();
    }
    let map = line_mapper(old, new);
    marks.iter().filter_map(|&line| map(line)).collect()
}

/// The line mapping behind [`shift_bookmark_lines`].
pub(super) fn shift_line_mapper<'a>(
    old: &'a [String],
    new: &'a [String],
) -> Box<dyn Fn(usize) -> Option<usize> + 'a> {
    use similar::DiffOp;
    let common = old.len().min(new.len());
    let prefix = old
        .iter()
        .zip(new)
        .take_while(|(a, b)| a == b)
        .count()
        .min(common);
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(common - prefix);
    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    // A deadline bounds a pathological middle; past it the diff is coarser
    // but still a valid diff, so marks still land on real rows.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(20);
    let ops = similar::capture_diff_slices_deadline(
        similar::Algorithm::Myers,
        &old[prefix..old_end],
        &new[prefix..new_end],
        Some(deadline),
    );
    let map_middle = move |row: usize| -> Option<usize> {
        let r = row - prefix;
        for op in &ops {
            match *op {
                DiffOp::Equal {
                    old_index,
                    new_index,
                    len,
                } if (old_index..old_index + len).contains(&r) => {
                    return Some(prefix + new_index + (r - old_index));
                }
                DiffOp::Delete {
                    old_index, old_len, ..
                } if (old_index..old_index + old_len).contains(&r) => return None,
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } if (old_index..old_index + old_len).contains(&r) => {
                    return Some(prefix + new_index + (r - old_index).min(new_len - 1));
                }
                _ => {}
            }
        }
        None
    };
    Box::new(move |line: usize| {
        let row = line.checked_sub(1)?;
        let mapped = if row < prefix {
            row
        } else if row >= old_end {
            row - old_end + new_end
        } else {
            map_middle(row)?
        };
        Some(mapped + 1)
    })
}

/// Marks after an edit that kept the line count: a marked line whose text
/// now sits, once, elsewhere in the changed span moved there (Move Line
/// Up/Down, a swap, undoing either); any other mark keeps its row, as a
/// line edited in place does.
pub(super) fn follow_moved_bookmark_lines(
    marks: &std::collections::BTreeSet<usize>,
    old: &[String],
    new: &[String],
) -> std::collections::BTreeSet<usize> {
    let Some(first) = old.iter().zip(new).position(|(a, b)| a != b) else {
        return marks.clone();
    };
    let last = old.len()
        - 1
        - old
            .iter()
            .rev()
            .zip(new.iter().rev())
            .position(|(a, b)| a != b)
            .unwrap_or(0);
    marks
        .iter()
        .map(|&line| {
            let row = line - 1;
            if !(first..=last).contains(&row) || old.get(row) == new.get(row) {
                return line;
            }
            let text = &old[row];
            let mut hits = (first..=last).filter(|&j| &new[j] == text);
            match (hits.next(), hits.next()) {
                (Some(j), None) => j + 1,
                _ => line,
            }
        })
        .collect()
}

/// Paint the overview ruler's ticks over an already-rendered scrollbar track.
///
/// The tick is a foreground glyph, not a background fill, so the thumb still
/// shows through underneath it: the ruler must not cost you the ability to see
/// where you are in the file, which is what the scrollbar is for.
pub(super) fn paint_overview_ruler(
    buf: &mut Buffer,
    area: Rect,
    ticks: &[Option<OverviewMark>],
    theme: crate::theme::Theme,
) {
    if area.width == 0 {
        return;
    }
    for (row, tick) in ticks.iter().enumerate() {
        let Some(mark) = tick else { continue };
        let y = area.y.saturating_add(row as u16);
        if y >= area.y.saturating_add(area.height) {
            break;
        }
        let cell = &mut buf[(area.x, y)];
        cell.set_symbol("\u{2501}"); // ━ heavy horizontal: a tick across the track
        cell.set_fg(mark.color(theme));
    }
}
