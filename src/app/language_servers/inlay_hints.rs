//! Inlay hints: replies routed to every editor showing the file (the seq-gated fan-out
//! that links and colors share), the refresh a server asks for, the range each request
//! covers, and Editor: Toggle Inlay Hints.

use super::*;

impl App {
    /// Route drained inlay-hint batches to whichever visible editor holds the
    /// file. A reply whose `seq` no longer matches the buffer's last-synced
    /// edit seq is dropped: its anchors were computed against older text, and
    /// the request fired for the current seq is still in flight.
    pub fn drain_lsp_inlay_hints(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_inlay_hints() {
            updates.push(u);
        }
        let mut changed = false;
        for u in updates {
            changed |= self.fan_out_batch(&u.path, u.seq, |e| {
                e.apply_inlay_hints(u.path.clone(), u.hints.clone())
            });
        }
        changed
    }

    /// Hand a seq-stamped batch for `path` to every editor showing the file,
    /// the active one and each inactive group, through `apply`. A batch whose
    /// `seq` no longer matches the buffer's last-synced edit seq is dropped:
    /// its anchors were computed against older text, and the request fired
    /// for the current seq is still in flight. Returns whether any editor
    /// took it.
    pub(in crate::app) fn fan_out_batch(
        &mut self,
        path: &Path,
        seq: u64,
        mut apply: impl FnMut(&mut EditorTabs),
    ) -> bool {
        if self.lsp_last_seen.get(path) != Some(&seq) {
            return false;
        }
        let mut applied = false;
        for group in self.editor_layout.all_groups_mut(&mut self.editor) {
            if group.path.as_deref() == Some(path) {
                apply(group);
                applied = true;
            }
        }
        applied
    }

    /// Re-request inlay hints when a server asked us to via
    /// `workspace/inlayHint/refresh` (rust-analyzer sends it after a config
    /// reload or when cross-file analysis changes the hints without an edit).
    /// Mirrors [`refresh_semantic_tokens_if_requested`].
    pub fn refresh_inlay_hints_if_requested(&mut self) {
        let Some(lsp) = self.lsp.as_ref() else {
            return;
        };
        if !lsp.take_inlay_refresh() {
            return;
        }
        self.request_inlay_hints_for_open_editors();
    }

    /// Fire an inlay-hint request for every visible editor's file at its
    /// current edit seq. Shared by the server-driven refresh and the palette
    /// toggle's re-enable path.
    pub(in crate::app) fn request_inlay_hints_for_open_editors(&mut self) {
        let Some(lsp) = self.lsp.as_ref() else {
            return;
        };
        for (p, end) in self.inlay_hint_targets() {
            if !self.inlay_hints_enabled_for(path_scope_id(&p)) {
                continue;
            }
            if let Some(&seq) = self.lsp_last_seen.get(&p) {
                lsp.request_inlay_hints(p, end, seq);
            }
        }
    }

    /// Every visible editor's file with where its buffer ends: the range an
    /// inlay-hint request covers (#1490).
    pub(in crate::app) fn inlay_hint_targets(&self) -> Vec<(PathBuf, lsp_types::Position)> {
        self.editor_layout
            .all_groups(&self.editor)
            .filter_map(|g| Some((g.path.clone()?, g.document_end())))
            .collect()
    }

    /// Editor: Toggle Inlay Hints — flips the hint display live and persists
    /// the choice. Off clears every editor's hints; on re-requests them for
    /// the visible files.
    pub(in crate::app) fn toggle_inlay_hints(&mut self) {
        self.inlay_hints_enabled = !self.inlay_hints_enabled;
        if self.inlay_hints_enabled {
            self.request_inlay_hints_for_open_editors();
            self.status = String::from("Inlay Hints: on");
        } else {
            for group in self.editor_layout.all_groups_mut(&mut self.editor) {
                group.clear_inlay_hints();
            }
            self.status = String::from("Inlay Hints: off");
        }
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_inlay_hints(self.inlay_hints_enabled));
        }
    }
}
