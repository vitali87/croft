//! Diff editor upkeep and navigation, merge conflicts and the three-way merge editor,
//! the history scrubber, local history and the who-wrote-this lens.

use super::*;

impl App {
    /// F7 / Shift+F7: move the caret to the next / previous conflict header,
    /// wrapping, and say which of how many the caret landed on.
    pub(super) fn jump_conflict(&mut self, backwards: bool) {
        let blocks = self.editor.conflicts().to_vec();
        let from = self.editor.cursor_row;
        let hit = if backwards {
            crate::merge::prev_conflict(&blocks, from)
        } else {
            crate::merge::next_conflict(&blocks, from)
        };
        let Some((idx, row)) = hit else {
            return;
        };
        self.editor.clear_selection();
        self.editor.cursor_row = row;
        self.editor.cursor_col = 0;
        self.editor.scroll_col = 0;
        self.editor.ensure_cursor_col_visible();
        self.status = format!("Conflict {} of {}", idx + 1, blocks.len());
    }

    /// The Quick Fix picker for a merge conflict at the cursor: VS Code's
    /// three accept actions, applied locally with no LSP involvement.
    pub(super) fn open_merge_conflict_picker(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let rows = vec![
            ListRow {
                id: String::from("current"),
                label: String::from("Accept Current Change"),
            },
            ListRow {
                id: String::from("incoming"),
                label: String::from("Accept Incoming Change"),
            },
            ListRow {
                id: String::from("both"),
                label: String::from("Accept Both Changes"),
            },
        ];
        self.open_list_picker(
            ListPicker::new(ListPurpose::MergeConflict, "Resolve Merge Conflict", rows),
            "No merge conflict at the cursor",
        );
    }

    /// Resolve the merge conflict at the cursor, reporting VS Code's
    /// wording in the status line. Backs both the palette's Merge Conflict
    /// commands and the Quick Fix picker rows.
    pub(super) fn resolve_merge_at_cursor(&mut self, res: crate::merge::Resolution) {
        // Palette commands reach here on any tab; a non-text view (image,
        // sheet, diff) must refuse rather than consult the conflict cache.
        if !self.editor_is_text() {
            self.status = String::from("Merge conflict actions need a text file");
            return;
        }
        let row = self.editor.cursor_row;
        let untrimmed = match res {
            crate::merge::Resolution::Both => self.untrimmed_both_at(row),
            _ => None,
        };
        let resolved = match untrimmed {
            Some((rows, both)) => {
                self.editor.splice_result_rows(rows.start, rows.end, both);
                true
            }
            None => self.editor.resolve_conflict_at(row, res),
        };
        if !resolved {
            self.status = String::from("No merge conflict at the cursor");
            return;
        }
        let accepted = match res {
            crate::merge::Resolution::Current => "Accepted current change",
            crate::merge::Resolution::Incoming => "Accepted incoming change",
            crate::merge::Resolution::Both => "Accepted both changes",
        };
        self.status = if self.editor.conflicts().is_empty() {
            format!(
                "{accepted} — all conflicts resolved; \"Merge: Complete Merge\" stages the file"
            )
        } else {
            format!("{accepted} — {} left", self.editor.conflicts().len())
        };
    }

    /// Accept Both for the conflict at `row` with what git trimmed off it
    /// put back (see `merge::untrimmed_both`), from the file's git stages
    /// merged again in diff3 style. None for a file git does not hold
    /// unmerged, an add/add with no base, or a block no conflict fits;
    /// Accept Both then joins the sections as written.
    pub(super) fn untrimmed_both_at(
        &mut self,
        row: usize,
    ) -> Option<(std::ops::Range<usize>, Vec<String>)> {
        let block = crate::merge::conflict_containing(self.editor.conflicts(), row)?;
        let path = self.editor.path.clone()?;
        // Stage specs are relative to the toplevel of the file's own repo
        // (#139). Not `scm_root`: Source Control can be pinned to another
        // folder of a multi-root workspace.
        let root = path.parent().and_then(crate::git::repo_toplevel)?;
        let canon_root = std::fs::canonicalize(&root).unwrap_or(root);
        let canon_path = std::fs::canonicalize(&path).ok()?;
        let rel = canon_path.strip_prefix(&canon_root).ok()?.to_string_lossy();
        let merged = crate::git::remerge_diff3(&canon_root, &rel).ok()?;
        let merged: Vec<String> = merged.lines().map(str::to_string).collect();
        crate::merge::untrimmed_both(
            &self.editor.lines,
            &block,
            &crate::merge::whole_sides(&merged),
        )
    }

    /// VS Code's "Accept All Current / Incoming": resolve every block in the
    /// buffer the same way, back to front so each replacement is one undo
    /// step and earlier block indices stay valid.
    pub(super) fn resolve_all_merge(&mut self, res: crate::merge::Resolution) {
        if !self.editor_is_text() {
            self.status = String::from("Merge conflict actions need a text file");
            return;
        }
        let total = self.editor.conflicts().len();
        if total == 0 {
            self.status = String::from("No merge conflicts in this file");
            return;
        }
        while let Some(block) = self.editor.conflicts().last().copied() {
            if !self.editor.resolve_conflict_at(block.ours_start, res) {
                break;
            }
        }
        let side = match res {
            crate::merge::Resolution::Current => "current",
            crate::merge::Resolution::Incoming => "incoming",
            crate::merge::Resolution::Both => "both sides",
        };
        self.status = format!(
            "Accepted {side} for {total} conflict{} — \"Merge: Complete Merge\" stages the file",
            if total == 1 { "" } else { "s" }
        );
    }

    /// VS Code's "Complete Merge": once every block is resolved, persist the
    /// buffer and stage the file so it leaves MERGE CONFLICTS and rejoins the
    /// ordinary commit flow. Refuses while conflicts remain.
    pub(super) fn complete_merge(&mut self) {
        if !self.editor_is_text() {
            self.status = String::from("Merge conflict actions need a text file");
            return;
        }
        // Merge editor (#253): the Result has no markers, so the gate is
        // the tracked regions, not the marker scan below.
        if self.editor.merge.is_some() {
            self.merge_view_sync();
            let unresolved = self
                .editor
                .merge
                .as_ref()
                .map(|mv| mv.unresolved_count())
                .unwrap_or(0);
            if unresolved > 0 {
                self.status = format!(
                    "{unresolved} conflict{} still unresolved — F7 jumps to the next one",
                    if unresolved == 1 { "" } else { "s" }
                );
                return;
            }
        }
        let left = self.editor.conflicts().len();
        if left > 0 {
            self.status = format!(
                "{left} conflict{} still unresolved — F7 jumps to the next one",
                if left == 1 { "" } else { "s" }
            );
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return;
        };
        // A modify/delete conflict resolved to the deleting side leaves an
        // empty Result: the resolution is "no file", so remove it from the
        // index and the disk, as `git rm` would (#1244).
        if self
            .editor
            .merge
            .as_ref()
            .is_some_and(|mv| mv.deleted.is_some())
            && self.editor.lines.iter().all(|l| l.is_empty())
        {
            let rel = self.repo_rel(&path);
            let root = self.scm_root();
            match crate::git::remove_path(&root, &rel) {
                Ok(()) => {
                    self.close_tabs_with_path_everywhere(&path);
                    self.refresh_source_control();
                    self.status = format!("Merge complete: removed {rel}");
                }
                Err(e) => self.status = format!("Remove failed: {e}"),
            }
            return;
        }
        // The merge of an agent's proposal with unsaved edits (#1353): the
        // save is the approval, and the file is croft's scratch copy, which
        // no repository should stage.
        if self
            .approval_edit
            .as_ref()
            .is_some_and(|(_, scratch)| *scratch == path)
        {
            if self.editor.dirty {
                self.save();
            } else {
                self.approve_saved_edit(&path);
            }
            return;
        }
        if self.editor.dirty {
            self.save();
        }
        // Canonicalize before the relative-path strip: a workspace under
        // a symlink (macOS /var → /private/var) otherwise falls back to
        // the absolute path, which reads wrong in the status and would
        // refuse in stricter git contexts.
        let merge_root = self
            .roots
            .owning_root(&path)
            .unwrap_or_else(|| self.roots.primary())
            .to_path_buf();
        let canon_root = std::fs::canonicalize(&merge_root).unwrap_or_else(|_| merge_root.clone());
        let canon_path = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let rel = canon_path
            .strip_prefix(&canon_root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| self.status_path(&path));
        match crate::git::stage_path(&canon_root, &rel) {
            Ok(()) => {
                self.refresh_source_control();
                self.status = format!("Merge complete: staged {rel}");
            }
            Err(e) => {
                let why = self.git_error_line("add", &e);
                self.status = format!("Stage failed: {why}");
            }
        }
    }

    /// Open the three-way merge editor (#253) for `path`. Inputs come
    /// from the git index stages (`:1:` base, `:2:` ours, `:3:` theirs)
    /// when the path is unmerged; otherwise the sides are synthesized
    /// from conflict markers in the file itself, so the editor also
    /// works on a plain marker file outside any merge. The tab is opened
    /// through the normal text route first (language, LSP, undo all
    /// attach), then the buffer is transformed into the initial Result
    /// in one undo step — the marker text stays one Undo away. Work the
    /// user already did in the file is kept (#906): a file resolved by
    /// hand is the Result as it stands, and a partly resolved one opens
    /// on the markers it still has.
    pub(super) fn open_merge_editor_for(&mut self, path: &Path) -> bool {
        // Stage specs are repo-toplevel-relative (#139): see `repo_rel`.
        let root = self.scm_root();
        let canon_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let rel = self.repo_rel(path);
        let to_lines = |s: String| -> Vec<String> { s.lines().map(str::to_string).collect() };
        let ours = crate::git::read_file_at_stage(&canon_root, &rel, 2).map(to_lines);
        let theirs = crate::git::read_file_at_stage(&canon_root, &rel, 3).map(to_lines);
        let mut deleted = None;
        let sides = match (ours, theirs) {
            // Neither stage exists: the path is not unmerged in git at
            // all — fall through to marker synthesis below.
            (Err(_), Err(_)) => None,
            // Unmerged: a missing single stage is a deleted side (DU/UD)
            // and a missing base is added-by-both (AA). Both read as an
            // empty side; a deleted one is also remembered, so taking it
            // removes the file rather than staging an empty one (#1244).
            (o, t) => {
                let base = crate::git::read_file_at_stage(&canon_root, &rel, 1).map(&to_lines);
                if base.is_ok() {
                    deleted = match (&o, &t) {
                        (Err(_), Ok(_)) => Some(crate::merge_editor::CheckSide::Current),
                        (Ok(_), Err(_)) => Some(crate::merge_editor::CheckSide::Incoming),
                        _ => None,
                    };
                }
                Some((
                    base.unwrap_or_default(),
                    o.unwrap_or_default(),
                    t.unwrap_or_default(),
                ))
            }
        };
        let built = match sides {
            Some((base, ours, theirs)) => Some(crate::merge_editor::MergeView::new(
                base, ours, theirs, false,
            )),
            None => std::fs::read_to_string(path).ok().and_then(|text| {
                let lines: Vec<String> = text.lines().map(str::to_string).collect();
                crate::merge_editor::view_from_markers(&lines)
            }),
        };
        let Some((mut mv, result)) = built else {
            self.status = format!("{rel} has no merge conflicts");
            return false;
        };
        mv.deleted = deleted;
        if let Err(e) = self.editor.open_pinned(path) {
            self.status = format!("Open failed: {e}");
            return false;
        }
        self.focus_pane(Pane::Editor);
        // Git keeps listing a file the user resolved in the text flow as
        // conflicted, and the stages alone would throw that work away. The
        // buffer, not the disk, holds it, unsaved edits included. A
        // modify/delete keeps the stages: its file is just the side that
        // kept it, with no markers to tell a decision from git's copy.
        let (mut mv, result) = if mv.from_markers || mv.deleted.is_some() {
            (mv, Some(result))
        } else {
            mv.open_on_file(result, &self.editor.lines)
        };
        if let Some(result) = result {
            let end = self.editor.lines.len();
            self.editor.splice_result_rows(0, end, result);
        }
        mv.synced_len = self.editor.lines.len();
        mv.synced_seq = self.editor.edit_seq;
        let n = mv.conflicts.len();
        let left = mv.unresolved_count();
        let idx = mv.conflicts.iter().position(|c| !c.state.resolved());
        let first = mv.conflicts.get(idx.unwrap_or(0)).map(|c| c.result_start);
        mv.anchor_panes_on(idx.unwrap_or(0));
        self.editor.merge = Some(mv);
        if let Some(row) = first {
            self.editor.clear_selection();
            self.editor.cursor_row = row.min(self.editor.lines.len().saturating_sub(1));
            self.editor.cursor_col = 0;
            self.editor.ensure_cursor_col_visible();
        }
        let done = match n - left {
            0 => String::new(),
            k => format!(", {k} already resolved in the file"),
        };
        self.status = format!(
            "Merge editor: {n} conflict{}{done} — F7 jumps, checkboxes or Merge commands accept, \"Reopen as Text\" for the marker flow",
            if n == 1 { "" } else { "s" }
        );
        true
    }

    /// `path` relative to the repo toplevel, both sides canonicalized —
    /// a workspace under a symlink (macOS /var → /private/var) otherwise
    /// fails the strip and yields an absolute "relative" path, which git
    /// stage specs refuse and status messages shouldn't show.
    pub(super) fn repo_rel(&self, path: &Path) -> String {
        let root = self.scm_root();
        let canon_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let canon_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        canon_path
            .strip_prefix(&canon_root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| self.status_path(path))
    }

    /// Reconcile the merge view's tracked regions with any buffer edit it
    /// has not seen yet (the render loop does this each frame; command
    /// paths call it first so they also work headless, e.g. under test).
    pub(super) fn merge_view_sync(&mut self) {
        let (len, seq, row) = (
            self.editor.lines.len(),
            self.editor.edit_seq,
            self.editor.merge_edit_row,
        );
        if let Some(mv) = self.editor.merge.as_mut() {
            mv.sync_with_buffer(len, seq, row);
        }
    }

    /// The conflict an accept action targets: the one under the cursor,
    /// else the one navigation last landed on.
    pub(super) fn merge_target_conflict(&self) -> Option<usize> {
        let mv = self.editor.merge.as_ref()?;
        mv.conflict_at_result_row(self.editor.cursor_row)
            .or_else(|| (mv.active < mv.conflicts.len()).then_some(mv.active))
    }

    /// Set conflict `idx` to `state`, splicing its replacement into the
    /// Result as one undo step and updating the tracked regions.
    pub(super) fn merge_set_conflict(
        &mut self,
        idx: usize,
        state: crate::merge_editor::ConflictState,
    ) {
        self.merge_view_sync();
        let Some(mv) = self.editor.merge.as_ref() else {
            return;
        };
        let Some(c) = mv.conflicts.get(idx) else {
            return;
        };
        let (start, end) = (c.result_start, c.result_start + c.result_len);
        let repl = c.replacement(state);
        let new_len = repl.len();
        self.editor.splice_result_rows(start, end, repl);
        let (len, seq) = (self.editor.lines.len(), self.editor.edit_seq);
        let mv = self.editor.merge.as_mut().expect("checked above");
        mv.note_accept(idx, new_len, state, len, seq);
        let (resolved, total) = (mv.resolved_count(), mv.conflicts.len());
        self.status = format!(
            "Conflict {} of {total}: {} ({resolved}/{total} resolved)",
            idx + 1,
            match state {
                crate::merge_editor::ConflictState::Current => "accepted current",
                crate::merge_editor::ConflictState::Incoming => "accepted incoming",
                crate::merge_editor::ConflictState::Both => "accepted both (current first)",
                crate::merge_editor::ConflictState::BothReverse => "accepted both (incoming first)",
                crate::merge_editor::ConflictState::Base => "kept base",
                crate::merge_editor::ConflictState::Unresolved => "back to unresolved",
                crate::merge_editor::ConflictState::Manual => "manual",
            }
        );
    }

    /// Accept-action entry for the palette commands: target the cursor's
    /// conflict (falling back to the active one).
    pub(super) fn merge_apply(&mut self, state: crate::merge_editor::ConflictState) {
        self.merge_view_sync();
        match self.merge_target_conflict() {
            Some(idx) => self.merge_set_conflict(idx, state),
            None => self.status = String::from("No conflict at the cursor — F7 jumps to one"),
        }
    }

    /// Accept every remaining unresolved conflict as `state` (the
    /// Accept-All commands in the merge editor).
    pub(super) fn merge_accept_all(&mut self, state: crate::merge_editor::ConflictState) {
        self.merge_view_sync();
        let Some(mv) = self.editor.merge.as_ref() else {
            return;
        };
        // Back-to-front so earlier indices' spans stay valid inside each
        // splice's own bookkeeping (note_accept shifts only later ones).
        let targets: Vec<usize> = (0..mv.conflicts.len())
            .rev()
            .filter(|&i| !mv.conflicts[i].state.resolved())
            .collect();
        let n = targets.len();
        for idx in targets {
            self.merge_set_conflict(idx, state);
        }
        let total = self
            .editor
            .merge
            .as_ref()
            .map(|m| m.conflicts.len())
            .unwrap_or(0);
        self.status = format!(
            "Accepted {n} conflict{} ({total} total)",
            if n == 1 { "" } else { "s" }
        );
    }

    /// Checkbox click: toggle one side's inclusion, VS Code's model —
    /// both checked is Accept Combination, neither reverts to base.
    pub(super) fn merge_toggle_side(&mut self, idx: usize, side: crate::merge_editor::CheckSide) {
        use crate::merge_editor::{CheckSide, ConflictState as S};
        let Some(mv) = self.editor.merge.as_ref() else {
            return;
        };
        let Some(c) = mv.conflicts.get(idx) else {
            return;
        };
        let (mut cur, mut inc) = match c.state {
            S::Current => (true, false),
            S::Incoming => (false, true),
            S::Both | S::BothReverse => (true, true),
            S::Unresolved | S::Base | S::Manual => (false, false),
        };
        match side {
            CheckSide::Current => cur = !cur,
            CheckSide::Incoming => inc = !inc,
        }
        let state = match (cur, inc) {
            (true, false) => S::Current,
            (false, true) => S::Incoming,
            (true, true) => S::Both,
            (false, false) => S::Unresolved,
        };
        self.merge_set_conflict(idx, state);
    }

    /// F7 / Shift+F7 in the merge editor: hop conflicts in the Result and
    /// re-anchor every source pane on the same conflict.
    pub(super) fn merge_jump(&mut self, backwards: bool) {
        self.merge_view_sync();
        let row = self.editor.cursor_row;
        let Some(mv) = self.editor.merge.as_mut() else {
            return;
        };
        let Some(idx) = mv.next_conflict(row, backwards) else {
            self.status = String::from("No conflicts");
            return;
        };
        mv.anchor_panes_on(idx);
        let target = mv.conflicts[idx].result_start;
        let total = mv.conflicts.len();
        self.editor.clear_selection();
        self.editor.cursor_row = target.min(self.editor.lines.len().saturating_sub(1));
        self.editor.cursor_col = 0;
        self.editor.ensure_cursor_col_visible();
        self.status = format!("Conflict {} of {total}", idx + 1);
    }

    /// Cycle the open diff's ignore-whitespace mode (off → leading → all).
    /// Per-view only: the `diff_ignore_whitespace` pref supplies the starting
    /// mode for each new diff and is not rewritten here, so an experiment in
    /// one view never changes what the next file opens with.
    pub fn diff_cycle_whitespace_mode(&mut self) {
        let Some(diff) = self.editor.diff.as_mut() else {
            self.status = String::from("Ignore whitespace applies to a diff view");
            return;
        };
        let mode = diff.ws_mode.next();
        diff.set_whitespace_mode(mode);
        self.status = format!("Diff: ignore whitespace {}", mode.label());
    }

    /// Turn the group-by-seat lens on or off for the diff in view (#349),
    /// so a change can be read as "the agent's part" and "my part".
    ///
    /// Offered only on the Source Control HEAD-vs-working view, the one diff
    /// whose right side IS the working tree: there the row's `right` index
    /// is a real file line and the seat map is keyed on the same index. A
    /// diff parsed out of raw `git diff` text numbers its rows by position
    /// in that text, so the same join would paint confident wrong seats.
    pub fn diff_toggle_group_by_seat(&mut self) {
        let history_root = self.history_root.clone();
        let Some(diff) = self.editor.diff.as_mut() else {
            self.status = String::from("Group by seat applies to a diff view");
            return;
        };
        if !diff.left_is_git_head {
            self.status =
                String::from("Group by seat applies to a Source Control diff of the working tree");
            return;
        }
        if diff.right_is_unsaved {
            // The seat map is keyed on the saved file's lines (#1572).
            self.status = String::from("Save the file first: group by seat reads the saved file");
            return;
        }
        diff.group_by_seat = !diff.group_by_seat;
        if !diff.group_by_seat {
            self.status = String::from("Diff: group by seat off");
            return;
        }
        // Read the map fresh each time the lens comes on: the working file
        // may have been saved since the view was built, and a map from
        // before that describes text this diff no longer shows.
        diff.seats = crate::provenance::Provenance::new();
        let path = diff.right_path.clone();
        let right_lines = diff.right_lines.clone();
        let seats = Self::seats_for_working_file(&history_root, &path, &right_lines);
        let Some(diff) = self.editor.diff.as_mut() else {
            return;
        };
        if let Some(seats) = seats {
            diff.seats = seats;
        }
        self.status = format!("Diff: group by seat \u{2022} {}", diff.seat_summary());
    }

    /// Reload the group-by-seat map of a rebuilt view against its new rows
    /// (#349). `carry_view_from` keeps the lens ON but deliberately drops
    /// its map, which named line indices the rebuilt rows have renumbered;
    /// this puts back a map that describes the rows now on screen, or none.
    /// A no-op for a view whose lens is off.
    pub(super) fn reload_seats_for(history_root: &Path, diff: &mut crate::widgets::diff::DiffData) {
        // The same gate the toggle applies, restated rather than assumed:
        // the map is keyed on working-file line indices, so it may only be
        // joined onto a view whose right side IS the working tree.
        if !diff.group_by_seat || !diff.left_is_git_head {
            return;
        }
        let right_lines = diff.right_lines.clone();
        diff.seats = Self::seats_for_working_file(history_root, &diff.right_path, &right_lines)
            .unwrap_or_default();
    }

    /// The persisted provenance map for a working-tree file, for the rows a
    /// diff view is actually showing (#349). Both halves of the rule
    /// `sync_provenance_for` applies: the newest snapshot must still hold the
    /// bytes on disk, AND those bytes must be the lines this view shows.
    ///
    /// The second half is not redundant. A diff's `right_lines` were read
    /// when the view was built, and a same-length rewrite inside one
    /// filesystem timestamp tick moves the file without moving the
    /// `(mtime, len)` stamp that triggers a rebuild - so the view can be
    /// showing older lines than the snapshot describes. Joining a newer map
    /// onto them would credit lines it never saw, which is the one outcome
    /// the module forbids. Either half failing reads as no map at all.
    pub(super) fn seats_for_working_file(
        history_root: &Path,
        path: &Path,
        right_lines: &[String],
    ) -> Option<crate::provenance::Provenance> {
        let newest = crate::history::entries_in(history_root, path)
            .into_iter()
            .next()?;
        let mut seats = crate::history::seats_for(history_root, path, newest.millis)?;
        let (on_disk, snapshot) = (std::fs::read(path).ok()?, std::fs::read(&newest.file).ok()?);
        if on_disk != snapshot {
            return None;
        }
        // Split the way the DIFF OPENER splits (`str::lines`), NOT the way
        // `Editor::open` does: a lone `\r` is one line to the opener and two
        // to the editor's splitter, and the comparison has to be against the
        // lines this view was actually built from. The bare UTF-8 decode
        // matches the opener too, which reads the right side with
        // `read_to_string` and so never builds a view for a file this
        // cannot decode.
        let text = String::from_utf8(snapshot).ok()?;
        if !text.lines().eq(right_lines.iter().map(String::as_str)) {
            return None;
        }
        // Defensive: the comparison above already forces equal lengths, so
        // this drops nothing today. It keeps the map bounded by the rows if
        // that gate is ever loosened.
        seats.truncate(right_lines.len());
        Some(seats)
    }

    /// Record on the diff view the opener just made active how it is to be
    /// rebuilt when a side moves (#471), and take its first disk stamps.
    pub(super) fn tag_open_diff(&mut self, source: crate::widgets::diff::DiffSource) {
        if let Some(diff) = self.editor.diff.as_mut() {
            diff.source = source;
            diff.stamp_sides();
        }
    }

    /// The unsaved text of `path` in whichever editor group holds it dirty
    /// (#1572): a split shows the same file in several groups, and the dirty
    /// copy may sit in an inactive one while the active group's is clean.
    pub(super) fn unsaved_text_in_any_group(&self, path: &Path) -> Option<String> {
        self.editor_layout
            .all_groups(&self.editor)
            .find_map(|group| group.unsaved_text_of(path))
    }

    /// Open a two-file diff in the active group, each side's unsaved text
    /// resolved across every editor group.
    pub(super) fn open_diff_across_groups(
        &mut self,
        left: &Path,
        right: &Path,
    ) -> anyhow::Result<()> {
        let left_unsaved = self.unsaved_text_in_any_group(left);
        let right_unsaved = self.unsaved_text_in_any_group(right);
        self.editor
            .open_diff_resolved(left, right, left_unsaved, right_unsaved)
    }

    /// Rebuild every open diff view, in every split group, whose sides may
    /// have moved (#471), keeping each reader's viewport. Returns true when
    /// any view's content actually changed, so the caller owes a redraw.
    ///
    /// Two triggers, two costs. A filesystem event or poll (`git_too` false)
    /// re-reads only file-backed sides, and only those whose stat stamp
    /// moved, so an idle tick is a stat per open diff and nothing more. A
    /// git-status drain (`git_too` true; the worker debounces it) also
    /// re-runs the `git diff` behind a staged / branch / previous-commit
    /// view, whose input is the index or a ref rather than a file, and
    /// re-reads the HEAD blob behind a Source Control view for the roots in
    /// `heads_moved` -- the drain already knows whose HEAD oid changed, and
    /// a `git show` per open diff per status refresh would otherwise run
    /// during every write burst for a HEAD that had not moved.
    /// A view whose content came back identical is left untouched: no reset
    /// of selection or find, no redraw.
    pub fn refresh_open_diff_views(
        &mut self,
        git_too: bool,
        heads_moved: &[PathBuf],
        changed_files: &std::collections::BTreeSet<PathBuf>,
    ) -> bool {
        // Bound before the borrow of `self.editor` below, for the seat map a
        // carried group-by-seat lens needs against its rebuilt rows (#349).
        let self_history_root = self.history_root.clone();
        // The watcher's own list of written paths bypasses the stamp gate: a
        // same-length rewrite that lands within the filesystem's mtime
        // granularity (or restores the mtime) shows the same `(mtime, len)`
        // as before, and the event is the only evidence it happened.
        // Canonicalised once, so a path the watcher reports through a
        // symlinked root (macOS's /tmp -> /private/tmp) still matches the
        // path the view was opened with.
        let touched: Vec<PathBuf> = changed_files
            .iter()
            .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
            .collect();
        let mut changed = false;
        // The active group's active tab, by address: a rebuild there is the
        // one that owes the find bar a recompute. A diff refreshing in a
        // background tab or split must not touch the view the reader is on.
        let active_ptr: *const crate::widgets::editor::Editor = &*self.editor;
        let mut active_changed = false;
        for group in self.editor_layout.all_groups_mut(&mut self.editor) {
            for ed in &mut group.editors {
                let Some(old) = ed.diff.as_ref() else {
                    continue;
                };
                let Some(mut fresh) = rebuild_diff_view(old, git_too, heads_moved, &touched) else {
                    continue;
                };
                if fresh.same_content_as(old) {
                    // Same rows: keep the reader's selection and find
                    // state, but take the new stamps so the next tick does
                    // not re-read the file again for a no-op write.
                    if let Some(cur) = ed.diff.as_mut() {
                        cur.left_stamp = fresh.left_stamp;
                        cur.right_stamp = fresh.right_stamp;
                        // A rebuild reads the right side from disk: rows
                        // identical to an unsaved view mean the save landed,
                        // so hunk actions apply again (#1572).
                        cur.right_is_unsaved = fresh.right_is_unsaved;
                    }
                    continue;
                }
                fresh.carry_view_from(old);
                // The carried lens needs a map for the REBUILT rows:
                // `carry_view_from` deliberately drops the old one, which
                // named lines these rows have renumbered (#349).
                Self::reload_seats_for(&self_history_root, &mut fresh);
                ed.diff = Some(fresh);
                changed = true;
                if std::ptr::eq(&*ed, active_ptr) {
                    active_changed = true;
                }
            }
        }
        // A rebuilt ACTIVE diff with a find bar open gets its match set and
        // active match recomputed against the new rows, the same way a
        // typed query would; `carry_view_from` deliberately dropped the old
        // active match because it named rows that may be gone.
        if active_changed
            && self.editor.diff.is_some()
            && self
                .editor_find
                .as_ref()
                .is_some_and(|s| !s.query.is_empty())
        {
            let pos = self.diff_find_current_pos();
            self.diff_find_apply(pos);
        }
        changed
    }

    /// Open the history scrubber over the current branch (#371).
    ///
    /// Parks at the WORKING TREE rather than at HEAD, so opening the
    /// scrubber changes nothing about what the user is looking at — the
    /// first step is theirs to take. That also makes the gesture free to
    /// try: a user who opens it by accident is exactly where they were.
    pub(super) fn scrub_history(&mut self) {
        // `branch_history`, NOT `commit_graph`: the latter logs
        // `--branches --tags HEAD` to draw the repo-wide graph, so with any
        // other branch present index 0 is whatever topological order put
        // first rather than HEAD, and stepping back from the working tree
        // lands on a commit the current branch may not even contain.
        let commits = crate::git::branch_history(self.workspace_root(), SCRUB_COMMIT_LIMIT);
        self.scrub_view = None;
        self.scrub_view_key = None;
        self.tree.scrub_tree = None;
        self.scrub_names.clear();
        if commits.is_empty() {
            self.status = String::from("No commits to scrub through");
            return;
        }
        let n = commits.len();
        self.scrubber = Some(crate::scrubber::Scrubber::new(commits));
        self.status = format!(
            "Scrubbing {n} commits — arrows step, Enter opens a commit's version, Home returns to your working tree"
        );
        self.prefetch_scrub_views();
    }

    /// One scrubber keystroke. Returns whether it was consumed (#371).
    ///
    /// Returning `false` for anything else matters: the scrubber is a view,
    /// not a mode that swallows the keyboard, so a key it does not use keeps
    /// its normal meaning rather than being silently eaten.
    pub(super) fn handle_scrubber_key(&mut self, code: KeyCode) -> bool {
        let Some(scrub) = self.scrubber.as_mut() else {
            return false;
        };
        match code {
            KeyCode::Left => scrub.older(),
            KeyCode::Right => scrub.newer(),
            KeyCode::Home => scrub.home(),
            // Enter at a commit opens that version as a tab of its own; at
            // the working tree there is nothing to open, so it passes
            // through to the live buffer like any other typing.
            KeyCode::Enter if scrub.commit().is_some() => {
                self.scrub_open_here();
                return true;
            }
            KeyCode::Esc => {
                // Closing returns to the live buffer by construction: the
                // scrubber never replaced it, so there is nothing to put
                // back and no path where unsaved edits could be lost.
                self.scrubber = None;
                self.scrub_view = None;
                self.scrub_view_key = None;
                self.scrub_views.clear();
                self.scrub_plain.clear();
                self.scrub_outlines.clear();
                self.scrub_builder = None;
                self.sync_scrub_tree();
                self.status = String::from("Left the history scrubber");
                return true;
            }
            _ => return false,
        }
        // git's own short hash, not a byte slice of the full one: that
        // slice is safe only while the string is ASCII hex, and the width
        // git abbreviates to is per-repo rather than always seven.
        self.status = match self.scrubber.as_ref().and_then(|s| s.commit()) {
            Some(c) => format!("At {} — {}", c.short_hash, c.summary),
            None => String::from("At your working tree"),
        };
        self.rebuild_scrub_view();
        true
    }

    /// The active file at the scrubber's commit (#371): its path, its
    /// workspace-relative name, the commit, and its text there (`None` when
    /// the file did not exist yet). `None` with a status line when there is
    /// no scrubbed commit or no file to show.
    pub(super) fn scrubbed_file(
        &mut self,
    ) -> Option<(
        PathBuf,
        String,
        crate::git::GraphCommit,
        Option<std::sync::Arc<str>>,
    )> {
        let Some(commit) = self.scrubber.as_ref().and_then(|s| s.commit()).cloned() else {
            self.status = String::from("Step the scrubber to a commit first");
            return None;
        };
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file is open to scrub");
            return None;
        };
        let root = self.workspace_root().to_path_buf();
        let Ok(rel) = path
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().into_owned())
        else {
            self.status = String::from("The open file is outside the workspace");
            return None;
        };
        let name = self.scrub_name_at(&rel, &commit.hash);
        let text = self
            .scrub_cache
            .entry((commit.hash.clone(), rel.clone()))
            .or_insert_with(|| {
                crate::git::read_file_at_rev(&root, &commit.hash, &name)
                    .ok()
                    .map(std::sync::Arc::from)
            })
            .clone();
        Some((path, rel, commit, text))
    }

    /// Leave the scrubber for a tab that shows what it was showing.
    pub(super) fn close_scrubber_for_tab(&mut self) {
        self.scrubber = None;
        self.scrub_view = None;
        self.scrub_view_key = None;
        self.scrub_views.clear();
        self.scrub_plain.clear();
        self.scrub_outlines.clear();
        self.scrub_builder = None;
        self.sync_scrub_tree();
        self.focus_pane(Pane::Editor);
    }

    /// **Open here** (#371): the scrubbed version of the file as a normal
    /// tab, highlighted as its language, labelled with the commit. It has no
    /// file on disk behind it, so nothing reloads or overwrites it.
    pub(super) fn scrub_open_here(&mut self) {
        let Some((path, rel, commit, text)) = self.scrubbed_file() else {
            return;
        };
        let Some(text) = text else {
            self.status = format!("{rel} did not exist at {}", commit.short_hash);
            return;
        };
        let label = PathBuf::from(format!("{rel} @ {}", commit.short_hash));
        if let Err(e) = self.editor.open_text_buffer(&label, &text) {
            self.status = format!("Could not open {rel} at {}: {e}", commit.short_hash);
            return;
        }
        // Highlighting only: no language server hears about a tab with no
        // file behind it.
        self.editor
            .set_language(crate::highlight::lang_for_path(&path));
        self.close_scrubber_for_tab();
        self.status = format!("{rel} at {} — {}", commit.short_hash, commit.summary);
    }

    /// **Diff to working tree** (#371): the scrubbed version against the
    /// working copy (its open tab's unsaved edits, else the file on disk),
    /// in the side-by-side diff.
    pub(super) fn scrub_diff_to_working_tree(&mut self) {
        let Some((path, rel, commit, text)) = self.scrubbed_file() else {
            return;
        };
        let label = PathBuf::from(format!("{rel} @ {}", commit.short_hash));
        let unsaved = self.unsaved_text_in_any_group(&path);
        // A file the commit predates diffs as all added, which is what
        // happened to it since.
        let text = text.unwrap_or_default();
        if let Err(e) = self
            .editor
            .open_head_diff_with_text_resolved(label, &text, &path, false, unsaved)
        {
            self.status = format!("Could not diff {rel}: {e}");
            return;
        }
        self.tag_open_diff(crate::widgets::diff::DiffSource::FixedLeft {
            left_text: text.to_string(),
        });
        self.close_scrubber_for_tab();
        self.status = format!("{rel}: {} → working tree", commit.short_hash);
    }

    /// Seek the scrubber to the stop under screen column `col` on the
    /// slider, and show that commit.
    pub(super) fn scrub_seek_column(&mut self, col: u16) {
        let track = self.scrub_slider;
        if track.width == 0 {
            return;
        }
        let Some(scrub) = self.scrubber.as_mut() else {
            return;
        };
        let span = track.width.saturating_sub(1).max(1) as f32;
        let f = (col.saturating_sub(track.x) as f32 / span).clamp(0.0, 1.0);
        let before = scrub.position();
        scrub.seek_fraction(f);
        if scrub.position() == before {
            return;
        }
        self.status = match self.scrubber.as_ref().and_then(|s| s.commit()) {
            Some(c) => format!("At {} — {}", c.short_hash, c.summary),
            None => String::from("At your working tree"),
        };
        self.rebuild_scrub_view();
    }

    /// The history scrubber's slider (#371), on the editor body's last row
    /// while scrubbing: where the cursor is (a commit's short hash and its
    /// place in the history, or the working tree), then a track from the
    /// oldest loaded commit on the left to the working tree on the right,
    /// with the handle where the view is.
    pub(super) fn render_scrub_slider(&mut self, frame: &mut ratatui::Frame) {
        self.scrub_slider = Rect::default();
        let Some(scrub) = self.scrubber.as_ref() else {
            return;
        };
        let body = self.editor.last_body;
        if body.width < 20 || body.height < 2 {
            return;
        }
        let row = Rect {
            x: body.x,
            y: body.y + body.height - 1,
            width: body.width,
            height: 1,
        };
        // Before a rename, say what the file was called then (#1304).
        let rel = self.editor.path.as_ref().and_then(|p| {
            p.strip_prefix(self.workspace_root())
                .ok()
                .map(|r| r.to_string_lossy().into_owned())
        });
        let label = match (scrub.position(), scrub.commit()) {
            (crate::scrubber::Position::At(i), Some(c)) => {
                let then = rel.as_ref().and_then(|rel| {
                    self.scrub_names
                        .get(rel)?
                        .get(&c.hash)
                        .filter(|n| *n != rel)
                });
                let n = scrub.len() - i;
                match then {
                    Some(old) => format!(" {} {n}/{} \u{b7} {old} ", c.short_hash, scrub.len()),
                    None => format!(" {} {n}/{} ", c.short_hash, scrub.len()),
                }
            }
            _ => String::from(" working tree "),
        };
        let label_w = (label.chars().count() as u16).min(row.width / 2);
        let track = Rect {
            x: row.x + label_w + 1,
            y: row.y,
            width: row.width.saturating_sub(label_w + 2),
            height: 1,
        };
        let bar_bg = self.theme.ui(Color::Rgb(0x1c, 0x21, 0x2b));
        let buf = frame.buffer_mut();
        buf.set_style(row, Style::default().bg(bar_bg));
        buf.set_stringn(
            row.x,
            row.y,
            &label,
            label_w as usize,
            Style::default()
                .fg(self.theme.accent())
                .bg(bar_bg)
                .add_modifier(Modifier::BOLD),
        );
        let handle = (scrub.fraction() * track.width.saturating_sub(1) as f32).round() as u16;
        for dx in 0..track.width {
            let (sym, fg) = if dx == handle {
                ("\u{25cf}", self.theme.accent())
            } else {
                ("\u{2500}", self.theme.ui(Color::Rgb(0x5a, 0x63, 0x73)))
            };
            buf.set_string(
                track.x + dx,
                track.y,
                sym,
                Style::default().fg(fg).bg(bar_bg),
            );
        }
        self.scrub_slider = track;
    }

    /// While a commit is on screen (#371), an Outline or breadcrumb jump
    /// lands in the historical view, whose symbols they list, not in the
    /// hidden live buffer. Returns whether it did.
    pub(super) fn jump_in_scrub_view(&mut self, line: u32) -> bool {
        let Some(view) = self.scrub_view.as_mut() else {
            return false;
        };
        let line = (line as usize).min(view.lines.len().saturating_sub(1));
        view.cursor_row = line;
        view.cursor_col = 0;
        let rows = (self.editor.last_body.height as usize).max(1);
        view.scroll = line.saturating_sub(rows / 3);
        true
    }

    /// Point the Explorer's dimming at the scrubber's commit (#371), or
    /// clear it at the working tree and once the scrubber closes. The tree
    /// listing is cached per commit like the file text, so stepping back
    /// over a visited commit costs no git process.
    pub(super) fn sync_scrub_tree(&mut self) {
        let Some(commit) = self.scrubber.as_ref().and_then(|s| s.commit()) else {
            self.tree.scrub_tree = None;
            return;
        };
        let root = self.workspace_root().to_path_buf();
        let present = self
            .scrub_trees
            .entry(commit.hash.clone())
            .or_insert_with(|| crate::git::tree_paths(&root, &commit.hash).map(std::sync::Arc::new))
            .clone();
        self.tree.scrub_tree = present.map(|set| (root, set));
    }

    /// Build the read-only view of the active file at the scrubber's commit
    /// (#371), or drop it at the working tree. File text is cached per
    /// (commit, path), so stepping back and forth after the first visit
    /// costs no git process.
    pub(super) fn rebuild_scrub_view(&mut self) {
        self.sync_scrub_tree();
        self.scrub_for = self.editor.path.clone();
        // Read before the view is parked: the next one looks where it did.
        let anchor = self.scrub_view_anchor();
        self.park_scrub_view();
        let Some(commit) = self.scrubber.as_ref().and_then(|s| s.commit()).cloned() else {
            self.scrub_view = None;
            self.scrub_view_key = None;
            // At the working tree: have the first steps back ready.
            self.prefetch_scrub_views();
            return;
        };
        let Some(path) = self.editor.path.clone() else {
            self.scrub_view = None;
            self.scrub_view_key = None;
            return;
        };
        let root = self.workspace_root().to_path_buf();
        let Ok(rel) = path
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().into_owned())
        else {
            self.scrub_view = None;
            self.scrub_view_key = None;
            return;
        };
        let key = crate::scrubber::ViewKey {
            hash: commit.hash.clone(),
            rel: rel.clone(),
        };
        // A finished view when one is ready; otherwise a plain stand-in (the
        // builder keeps those ready around the cursor) and the finished view
        // when it lands, so a step never waits on highlighting a big file.
        let take = |kept: &mut crate::scrubber::KeptViews| {
            kept.iter()
                .position(|(k, _)| *k == key)
                .and_then(|i| kept.remove(i))
                .map(|(_, v)| v)
        };
        let (view, finished) = match take(&mut self.scrub_views) {
            Some(v) => (Some(v), true),
            None => (take(&mut self.scrub_plain), false),
        };
        // Neither ready (the first step, or a jump): build the stand-in here.
        let name = self.scrub_name_at(&rel, &commit.hash);
        let view = view.or_else(|| {
            let text = self
                .scrub_cache
                .entry((commit.hash.clone(), rel.clone()))
                .or_insert_with(|| {
                    crate::git::read_file_at_rev(&root, &commit.hash, &name)
                        .ok()
                        .map(std::sync::Arc::from)
                })
                .clone();
            Some(crate::scrubber::plain_view(
                &path,
                &rel,
                &commit.short_hash,
                text.as_deref(),
            ))
        });
        let Some(mut view) = view else { return };
        Self::align_scrub_view(&mut view, anchor);
        self.scrub_view = Some(view);
        self.scrub_view_key = Some((key, finished));
        self.request_scrub_views(&root, &path, &rel);
    }

    /// The name `rel` had at each commit the scrubber walks (#1304), keyed
    /// by full hash; a commit missing from it had no older name.
    pub(super) fn scrub_names(
        &mut self,
        rel: &str,
    ) -> std::sync::Arc<std::collections::HashMap<String, String>> {
        if let Some(names) = self.scrub_names.get(rel) {
            return std::sync::Arc::clone(names);
        }
        let Some(scrub) = self.scrubber.as_ref() else {
            return Default::default();
        };
        let touched = crate::git::follow_names(self.workspace_root(), rel, SCRUB_COMMIT_LIMIT);
        let names =
            std::sync::Arc::new(crate::scrubber::names_by_commit(scrub.commits(), &touched));
        self.scrub_names
            .insert(rel.to_string(), std::sync::Arc::clone(&names));
        names
    }

    /// The name to read `rel` by at commit `hash`: its name there when the
    /// file was renamed since, else `rel` itself.
    pub(super) fn scrub_name_at(&mut self, rel: &str, hash: &str) -> String {
        self.scrub_names(rel)
            .get(hash)
            .cloned()
            .unwrap_or_else(|| rel.to_string())
    }

    /// Put the finished view being left back among the kept ones, so
    /// stepping back to it is free; drop the least recently used beyond
    /// `SCRUB_VIEWS_KEPT` (a big file's view holds its whole text and
    /// highlighting).
    pub(super) fn park_scrub_view(&mut self) {
        let Some((key, finished)) = self.scrub_view_key.take() else {
            return;
        };
        let Some(view) = self.scrub_view.take() else {
            return;
        };
        if finished {
            self.keep_scrub_view(key, view);
        } else {
            Self::keep_view(&mut self.scrub_plain, key, view);
        }
    }

    pub(super) fn keep_scrub_view(
        &mut self,
        key: crate::scrubber::ViewKey,
        view: crate::widgets::editor::Editor,
    ) {
        // Its stand-in is no longer needed.
        if let Some(i) = self.scrub_plain.iter().position(|(k, _)| *k == key)
            && let Some(stale) = self.scrub_plain.remove(i)
        {
            fs_watch::offload_drop(stale);
        }
        Self::keep_view(&mut self.scrub_views, key, view);
    }

    /// Keep `view` as the most recently used in `kept`, dropping the least
    /// recently used beyond `SCRUB_VIEWS_KEPT`. Dropped off the UI thread:
    /// a big file's view is tens of thousands of strings, and freeing them
    /// is a frame's worth of work a held arrow key cannot spare.
    pub(super) fn keep_view(
        kept: &mut crate::scrubber::KeptViews,
        key: crate::scrubber::ViewKey,
        view: crate::widgets::editor::Editor,
    ) {
        if let Some(i) = kept.iter().position(|(k, _)| *k == key)
            && let Some(old) = kept.remove(i)
        {
            fs_watch::offload_drop(old);
        }
        kept.push_back((key, view));
        while kept.len() > SCRUB_VIEWS_KEPT {
            if let Some(evicted) = kept.pop_front() {
                fs_watch::offload_drop(evicted);
            }
        }
    }

    /// Request the views around the cursor for the active file, when it is
    /// one the scrubber can show.
    pub(super) fn prefetch_scrub_views(&mut self) {
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let root = self.workspace_root().to_path_buf();
        let Ok(rel) = path
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().into_owned())
        else {
            return;
        };
        self.request_scrub_views(&root, &path, &rel);
    }

    /// Where a new view of the active file should look: where the view on
    /// screen was looking (a symbol jump moves it), else where the live
    /// buffer is. `(scroll, cursor_row)`.
    pub(super) fn scrub_view_anchor(&self) -> (usize, usize) {
        match self.scrub_view.as_ref() {
            Some(prev) if prev.path.is_some() && prev.path == self.editor.path => {
                (prev.scroll, prev.cursor_row)
            }
            _ => (self.editor.scroll, self.editor.cursor_row),
        }
    }

    /// Look at the stretch of the file `anchor` names.
    pub(super) fn align_scrub_view(
        view: &mut crate::widgets::editor::Editor,
        (scroll, row): (usize, usize),
    ) {
        let last = view.lines.len().saturating_sub(1);
        view.scroll = scroll.min(last);
        view.cursor_row = row.min(last);
    }

    /// Ask the builder for what the commits around the cursor still lack:
    /// stand-ins for the ones with neither a stand-in nor a finished view,
    /// and finished views for the ones without one; most urgent first.
    pub(super) fn request_scrub_views(&mut self, root: &Path, path: &Path, rel: &str) {
        let names = self.scrub_names(rel);
        let Some(scrub) = self.scrubber.as_ref() else {
            return;
        };
        let jobs: Vec<crate::scrubber::ViewJob> = scrub
            .around()
            .into_iter()
            .map(|c| crate::scrubber::ViewJob {
                key: crate::scrubber::ViewKey {
                    hash: c.hash.clone(),
                    rel: rel.to_string(),
                },
                path: path.to_path_buf(),
                short: c.short_hash.clone(),
                read_rel: names.get(&c.hash).map_or(rel, String::as_str).to_string(),
                // A parent past the walked commits keeps the commit's name.
                parent_rel: c
                    .parents
                    .first()
                    .and_then(|p| names.get(p))
                    .or_else(|| names.get(&c.hash))
                    .map_or(rel, String::as_str)
                    .to_string(),
                parent: c.parents.first().cloned(),
                text: self
                    .scrub_cache
                    .get(&(c.hash.clone(), rel.to_string()))
                    .cloned(),
                parent_text: c
                    .parents
                    .first()
                    .and_then(|p| self.scrub_cache.get(&(p.clone(), rel.to_string())).cloned()),
            })
            .collect();
        let shown = self.scrub_view_key.as_ref();
        let has_finished = |key: &crate::scrubber::ViewKey| {
            self.scrub_views.iter().any(|(k, _)| k == key)
                || shown.is_some_and(|(k, f)| k == key && *f)
        };
        let has_plain = |key: &crate::scrubber::ViewKey| {
            self.scrub_plain.iter().any(|(k, _)| k == key) || shown.is_some_and(|(k, _)| k == key)
        };
        let finished: Vec<crate::scrubber::ViewJob> = jobs
            .iter()
            .filter(|j| !has_finished(&j.key))
            .cloned()
            .collect();
        let plain: Vec<crate::scrubber::ViewJob> = jobs
            .into_iter()
            .filter(|j| !has_finished(&j.key) && !has_plain(&j.key))
            .collect();
        let builder = self
            .scrub_builder
            .get_or_insert_with(|| crate::scrubber::ViewBuilder::start(root.to_path_buf()));
        builder.want(plain, finished);
    }

    /// Take in what the builder produced (#371): keep each view, and swap a
    /// finished one in for the stand-in on screen. True when the screen
    /// changed.
    pub fn drain_scrub_views(&mut self) -> bool {
        let Some(builder) = self.scrub_builder.as_ref() else {
            return false;
        };
        let built = builder.drain();
        let mut changed = false;
        for b in built {
            let rel = b.key.rel.clone();
            self.scrub_cache
                .entry((b.key.hash.clone(), rel.clone()))
                .or_insert(b.text);
            if let Some((parent, text)) = b.parent_text {
                self.scrub_cache.entry((parent, rel)).or_insert(text);
            }
            if let Some(outline) = b.outline {
                // Symbols are small next to a view, but a long session visits
                // many commits: keep a bounded set.
                if self.scrub_outlines.len() >= SCRUB_OUTLINES_KEPT {
                    self.scrub_outlines.clear();
                }
                self.scrub_outlines.insert(b.key.clone(), outline);
                changed = true;
            }
            let mut view = b.view;
            let on_screen = self.scrubber.is_some()
                && self.scrub_view.is_some()
                && self
                    .scrub_view_key
                    .as_ref()
                    .is_some_and(|(k, _)| *k == b.key);
            if b.finished {
                if on_screen && matches!(self.scrub_view_key, Some((_, false))) {
                    Self::align_scrub_view(&mut view, self.scrub_view_anchor());
                    if let Some(stand_in) = self.scrub_view.replace(view) {
                        fs_watch::offload_drop(stand_in);
                    }
                    self.scrub_view_key = Some((b.key, true));
                    changed = true;
                } else if on_screen {
                    fs_watch::offload_drop(view);
                } else {
                    self.keep_scrub_view(b.key, view);
                }
            } else if !on_screen && !self.scrub_views.iter().any(|(k, _)| *k == b.key) {
                Self::keep_view(&mut self.scrub_plain, b.key, view);
            } else {
                fs_watch::offload_drop(view);
            }
        }
        changed
    }

    /// `Editor: Toggle Provenance` (#349): flip the lens for the session and
    /// push it onto the active editor now (the per-tick sync covers the
    /// next tab focused).
    pub(super) fn toggle_provenance(&mut self) {
        self.provenance_overlay = !self.provenance_overlay;
        self.editor.provenance_overlay = self.provenance_overlay;
        self.status = if self.provenance_overlay {
            String::from("Provenance: on")
        } else {
            String::from("Provenance: off")
        };
    }

    /// Restore persisted provenance maps onto the open buffers (#349), each
    /// once per buffer generation: when a tab is a clean text buffer that
    /// knows nothing yet, and the newest history snapshot holds exactly the
    /// bytes on disk, the seats recorded beside that snapshot describe this
    /// very text and come back. Any difference (an edit since, a save croft
    /// never saw, a file with no history) leaves every line unknown: a map is
    /// never applied to text it did not describe. Every tab of every group,
    /// not only the focused one: the lens paints in every pane
    /// (`sync_focus_flags` carries the flag there), so a map restored onto
    /// the focused tab alone would leave a split showing blanks beside it.
    pub(super) fn sync_provenance(&mut self) {
        let root = self.history_root.clone();
        let groups = self.editor_layout.all_groups_mut(&mut self.editor);
        for ed in groups.flat_map(|g| g.editors.iter_mut()) {
            Self::sync_provenance_for(&root, ed);
        }
    }

    /// The per-buffer body of [`Self::sync_provenance`].
    pub(super) fn sync_provenance_for(root: &Path, ed: &mut crate::widgets::editor::Editor) {
        let Some(path) = ed.path.clone() else {
            return;
        };
        let seq = ed.edit_seq;
        if ed.provenance_synced_seq == Some(seq) {
            return;
        }
        // Not eligible yet is not the same as considered: a dirty buffer, one
        // that already knows its seats, or a viewer tab (the file shown
        // through its own path, `lines` a placeholder no map should credit;
        // a markdown preview is NOT one, its `lines` are the file's own text)
        // leaves the marker unset, so a later tick, after a save or a swap
        // back to text, reconsiders instead of staying burnt for the session.
        if ed.dirty || ed.provenance.attributed() > 0 || ed.has_non_text_view() {
            return;
        }
        let Some(newest) = crate::history::entries_in(root, &path).into_iter().next() else {
            return;
        };
        // The sidecar first: most files have a snapshot and no record, and
        // the reads below are only worth paying on the UI thread when there
        // is a map to apply. Nothing above this point burns the marker: a
        // save records its snapshot and then its sidecar from a worker
        // thread, so a tick landing in that window must be free to look
        // again rather than deciding this buffer has no map.
        let Some(mut seats) = crate::history::seats_for(root, &path, newest.millis) else {
            return;
        };
        ed.provenance_synced_seq = Some(seq);
        // Then sizes: this runs on the render thread, and two whole-file
        // reads of a large file on the frame a tab is switched to would be a
        // visible stall for what is usually a mismatch.
        let (Ok(disk_meta), Ok(snap_meta)) =
            (std::fs::metadata(&path), std::fs::metadata(&newest.file))
        else {
            return;
        };
        if disk_meta.len() != snap_meta.len() {
            return;
        }
        let (Ok(on_disk), Ok(snapshot)) = (std::fs::read(&path), std::fs::read(&newest.file))
        else {
            return;
        };
        if on_disk != snapshot {
            return;
        }
        // And this buffer holds that very text. A clean buffer can still be
        // stale: when one tab of a split saves, the other keeps the pre-save
        // lines, and the snapshot then matches DISK while describing text
        // that tab never held. The disk stamp cannot tell the two apart when
        // both writes land inside one filesystem timestamp tick, so the
        // comparison is against the buffer's own bytes.
        if !ed.holds_exactly(&snapshot) {
            return;
        }
        seats.truncate(ed.lines.len());
        ed.provenance = seats;
    }

    /// Record a local-history snapshot of a file that just hit the disk, and
    /// refresh the TIMELINE when it's the active file so the new version shows.
    /// Reads back what was written so the snapshot matches the on-disk bytes
    /// exactly (encoding / EOL included). The IO runs off-thread — with auto
    /// save this fires every second, and a large file's read+write on the
    /// render thread is a visible stall.
    ///
    /// `seats` is who typed each line of the buffer that was saved, recorded
    /// beside the snapshot (#349). The caller reads it off that tab
    /// (`Editor::provenance_to_record`) at the save, on the UI thread: never
    /// looked up by path here, since a split can hold the same file in a
    /// second buffer with a different map, and a snapshot restore records
    /// the restored snapshot's own seats, which no tab holds at that moment.
    ///
    /// Records the seats with the bytes the map describes.
    /// The worker re-reads the file, which is what makes the snapshot
    /// byte-exact, and between the save and that read another tab's save of
    /// the same path can land - so the map is recorded only while the bytes
    /// on disk are still the ones it describes.
    ///
    /// `described` comes from the caller because only the caller knows WHICH
    /// buffer the map came from: a search by path finds the focused tab,
    /// which need not be the tab that saved. `None` records the map
    /// unconditionally, for a caller whose map has no live buffer - a
    /// snapshot restore, whose seats come from the restored snapshot and
    /// describe the bytes it has just written itself.
    pub(super) fn record_history_snapshot_of(
        &mut self,
        path: &Path,
        seats: crate::provenance::Provenance,
        described: Option<Vec<u8>>,
    ) {
        // Every save lands here, so this is where a watched test scope
        // hears about it (#263). A file git ignores (build output, a `.env`)
        // is not source, so its save reruns nothing; git is only asked while
        // something is watched.
        if self.testing.watch.scope().is_some()
            && path.starts_with(&self.active_test_root)
            && !crate::git::is_ignored(&self.active_test_root, path)
        {
            self.testing
                .watch
                .on_saved(path, &self.active_test_root, std::time::Instant::now());
        }
        self.spawn_history_record(path, seats, described, false);
        // Every save passes through here: breakpoints that followed edits in
        // the buffer are handed to running sessions now that the file on disk
        // has the same lines, or they stay bound to the old line numbers.
        self.resend_breakpoints(path);
    }

    /// Record each dirty tab of `paths` (any group) in Local History as a
    /// kept snapshot, before a reload throws its edits away (#910). The
    /// number recorded.
    pub(super) fn keep_unsaved_buffers_in_history(&mut self, paths: &[PathBuf]) -> usize {
        let millis = now_millis();
        let mut buffers: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        let groups = self.editor_layout.all_groups(&self.editor);
        for tab in groups.flat_map(|g| g.iter_tabs()) {
            let Some(path) = tab.path.as_ref().filter(|p| paths.contains(p)) else {
                continue;
            };
            if tab.dirty
                && !buffers.iter().any(|(p, _)| p == path)
                && let Some(bytes) = tab.bytes_for_disk()
            {
                buffers.push((path.clone(), bytes));
            }
        }
        buffers
            .iter()
            .filter(|(path, bytes)| {
                crate::history::record_kept_in(&self.history_root, path, bytes, millis).is_ok()
            })
            .count()
    }

    /// [`Self::record_history_snapshot_of`] as a kept snapshot (a restore).
    pub(super) fn record_history_snapshot_of_kept(
        &mut self,
        path: &Path,
        seats: crate::provenance::Provenance,
    ) {
        self.spawn_history_record(path, seats, None, true);
    }

    pub(super) fn spawn_history_record(
        &mut self,
        path: &Path,
        seats: crate::provenance::Provenance,
        described: Option<Vec<u8>>,
        keep: bool,
    ) {
        let root = self.history_root.clone();
        let path = path.to_path_buf();
        let tx = self.history_done_tx.clone();
        let millis = now_millis();
        std::thread::spawn(move || {
            // A save too large to snapshot is not read at all (#1011).
            let skip = !keep
                && std::fs::metadata(&path)
                    .is_ok_and(|m| crate::history::too_large_to_snapshot(m.len()));
            if !skip && let Ok(bytes) = std::fs::read(&path) {
                // Someone else's bytes are on disk now: record the snapshot
                // alone, every line unknown, which is the safe direction.
                let seats = match described {
                    Some(want) if want != bytes => crate::provenance::Provenance::new(),
                    _ => seats,
                };
                let _ = if keep {
                    crate::history::record_kept_with_seats_in(&root, &path, &bytes, millis, &seats)
                } else {
                    crate::history::record_with_seats_in(&root, &path, &bytes, millis, &seats)
                };
            }
            let _ = tx.send(path);
        });
    }

    /// Go to Next / Previous Change (#1352): in a diff tab its hunks, in a
    /// buffer with merge conflicts its conflicts, otherwise the editor's
    /// change bars, wrapping at the ends.
    pub(super) fn jump_editor_change(&mut self, forward: bool) {
        if self.editor.diff.is_some() {
            self.jump_diff_change(forward);
            return;
        }
        if !self.editor.conflicts().is_empty() {
            self.jump_conflict(!forward);
            return;
        }
        let row = self.editor.cursor_row;
        match self.editor.git_change_from(row, forward) {
            Some(line) => {
                self.editor.clear_selection();
                self.editor.goto_line_centered(line);
                self.status = format!("Change at line {}", line + 1);
            }
            None => self.status = String::from("No changes in this file"),
        }
    }

    /// Scroll the active diff to the next change hunk (forward=true) or
    /// the previous one (forward=false), wrapping around the ends so a
    /// user can keep clicking the same arrow to cycle through every
    /// hunk. Mirrors the F7 / Shift+F7 key path so click and keybinding
    /// stay in sync.
    pub(super) fn jump_diff_change(&mut self, forward: bool) {
        let Some(diff) = self.editor.diff.as_mut() else {
            return;
        };
        if forward {
            diff.jump_next_change();
        } else {
            diff.jump_prev_change();
        }
    }

    /// Open the working-tree file the diff caret sits on at its line, the
    /// Enter action in the diff view. Resolves the row to a (file, line)
    /// via `DiffData::jump_target`, then reuses the explorer's pinned-tab
    /// open so the diff tab is left intact beside the opened file.
    pub(super) fn jump_to_diff_file_under_caret(&mut self) {
        let target = {
            let Some(diff) = self.editor.diff.as_ref() else {
                return;
            };
            // A git-produced diff carries toplevel-relative `+++ b/` paths
            // (#139); croft's own snapshot diffs stay workspace-anchored.
            let root = if diff.left_is_git_head {
                self.scm_root()
            } else {
                self.tree.root.clone()
            };
            let row = diff.caret_row().unwrap_or(diff.scroll);
            diff.jump_target(row, &root)
        };
        let Some(j) = target else {
            return;
        };
        match self.editor.open_pinned(&j.path) {
            Ok(()) => {
                self.editor.goto_line(j.line);
                self.focus_pane(Pane::Editor);
                self.sync_open_file_poll_mtime();
                self.poke_cursor();
                self.status = format!("{}:{}", self.status_path(&j.path), j.line);
            }
            Err(err) => {
                self.status = format!("Could not open {}: {err}", j.path.display());
            }
        }
    }

    pub(super) fn handle_diff_key(&mut self, key: KeyEvent) {
        // Cmd+C / Ctrl+Shift+C on a diff selection: copy the dragged
        // substring of whichever column the selection is anchored on.
        // Must run BEFORE the scroll match below so the keystroke isn't
        // dropped as "no scroll mapping". The general editor key handler
        // short-circuits to this function when a diff is open, so without
        // this branch Cmd+C in a diff view literally never copies.
        if is_editor_copy_key(key) {
            self.copy_editor_selection();
            return;
        }
        // Enter opens the file under the caret at the matching line. The
        // caret is the selection head a click leaves behind; with no
        // selection yet we fall back to the top visible row.
        if matches!(key.code, KeyCode::Enter) {
            self.jump_to_diff_file_under_caret();
            return;
        }
        // Hunk actions on the change under the caret (VS Code's Stage /
        // Unstage / Discard Selected Ranges, one keystroke each since the
        // diff pane owns no text input). R is destructive and routes
        // through the confirm popup.
        if let KeyCode::Char(c) = key.code
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
        {
            match c.to_ascii_lowercase() {
                's' => {
                    self.stage_hunk_at_caret();
                    return;
                }
                'u' => {
                    self.unstage_hunk_at_caret();
                    return;
                }
                'r' => {
                    self.request_revert_hunk_at_caret();
                    return;
                }
                _ => {}
            }
        }
        // Page = inner viewport rows minus the header + footer the diff
        // renderer reserves. Falls back to a sane default when the editor
        // hasn't laid out yet.
        let page = (self.editor.last_inner.height as usize)
            .saturating_sub(2)
            .max(1);
        let Some(diff) = self.editor.diff.as_mut() else {
            return;
        };
        // F7 / Shift+F7: jump to next / previous change hunk. Matches the
        // VS Code keybinding for "Next/Previous Change in Diff Editor" so
        // users can scan a long diff without scrolling line-by-line.
        if matches!(key.code, KeyCode::F(7)) {
            // Shared backend with the click arrows: the jump anchors on the
            // hunk the previous jump landed on (tracked in `nav_anchor`),
            // not on `scroll` — `render_diff` clamps `scroll` at the bottom,
            // which used to pin the forward arrow on the last hunk. Both
            // directions wrap so F7 at the last hunk loops to the first.
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                diff.jump_prev_change();
            } else {
                diff.jump_next_change();
            }
            return;
        }
        match key.code {
            KeyCode::Up => diff.scroll_up_by(1),
            KeyCode::Down => diff.scroll_down_by(1),
            KeyCode::PageUp => diff.scroll_up_by(page),
            KeyCode::PageDown => diff.scroll_down_by(page),
            KeyCode::Home => diff.scroll_home(),
            KeyCode::End => diff.scroll_end(),
            KeyCode::Left => diff.scroll_left_by(4),
            KeyCode::Right => diff.scroll_right_by(4),
            _ => {}
        }
    }
}

/// Build the up-to-date version of an open diff view from its recorded
/// source (#471), or `None` when there is nothing new to build: a `Static`
/// view, a file-backed view whose sides have not moved on disk, a git-command
/// view on a filesystem-only trigger, or a side that can no longer be read
/// (the reader keeps the last good view). The result carries fresh disk
/// stamps but none of the reader's view state; `DiffData::carry_view_from`
/// adds that.
pub(super) fn rebuild_diff_view(
    old: &crate::widgets::diff::DiffData,
    git_too: bool,
    heads_moved: &[PathBuf],
    touched: &[PathBuf],
) -> Option<crate::widgets::diff::DiffData> {
    use crate::widgets::diff::{DiffData, DiffSource, GitDiffKind};
    // A side the watcher just reported written, matched on the canonical
    // path so a symlinked root cannot hide it. Bypasses the stamp gate.
    let written = |side: &Path| {
        if touched.is_empty() {
            return false;
        }
        let canon = std::fs::canonicalize(side).unwrap_or_else(|_| side.to_path_buf());
        touched.iter().any(|t| *t == canon || t == side)
    };
    let lines = |t: &str| -> Vec<String> { t.lines().map(str::to_string).collect() };
    let two_sided = |left_text: &str, right_text: &str| {
        DiffData::build_with_byte_check(
            old.left_path.clone(),
            old.right_path.clone(),
            lines(left_text),
            lines(right_text),
            Some(left_text),
            Some(right_text),
        )
    };
    let mut fresh = match &old.source {
        DiffSource::Static => return None,
        DiffSource::HeadVsWorking { root, rel } => {
            // Canonical on both sides: git canonicalises `--show-toplevel`
            // (macOS's /tmp -> /private/tmp), while a root the app carries
            // as typed may not be, so the same repository can be spelled two
            // ways between the tag and the drain's record.
            let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            let head_moved = git_too
                && heads_moved
                    .iter()
                    .any(|r| r == root || canon(r) == canon(root));
            if !head_moved && !written(&old.right_path) && !old.sides_moved_on_disk() {
                return None;
            }
            let enc = old.text_encoding;
            let head = crate::git::read_bytes_at_head(root, rel).ok()?;
            let head = crate::widgets::editor::decode_blob(&head, enc)?;
            let right = std::fs::read(&old.right_path).ok()?;
            let right = crate::widgets::editor::decode_blob(&right, enc)?;
            two_sided(&head, &right)
        }
        DiffSource::FixedLeft { left_text } => {
            if !written(&old.right_path) && !old.sides_moved_on_disk() {
                return None;
            }
            let right = std::fs::read_to_string(&old.right_path).ok()?;
            two_sided(left_text, &right)
        }
        DiffSource::TwoFiles => {
            if !written(&old.left_path) && !written(&old.right_path) && !old.sides_moved_on_disk() {
                return None;
            }
            let left = std::fs::read_to_string(&old.left_path).ok()?;
            let right = std::fs::read_to_string(&old.right_path).ok()?;
            DiffData::build(
                old.left_path.clone(),
                old.right_path.clone(),
                lines(&left),
                lines(&right),
            )
        }
        DiffSource::GitCommand { root, kind } => {
            if !git_too {
                return None;
            }
            let raw = match kind {
                GitDiffKind::Staged => crate::git::diff_staged(root),
                GitDiffKind::AgainstBranch(branch) => crate::git::diff_against_branch(root, branch),
                GitDiffKind::PreviousCommit => crate::git::diff_previous_commit(root),
            }
            .ok()?;
            DiffData::build_side_by_side_from_git_text(old.left_path.clone(), &raw)
        }
    };
    fresh.source = old.source.clone();
    fresh.left_is_real_file = old.left_is_real_file;
    fresh.text_encoding = old.text_encoding;
    fresh.stamp_sides();
    Some(fresh)
}
