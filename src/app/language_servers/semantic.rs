//! Semantic highlighting: semantic-token replies handed to every editor showing the file,
//! the refresh a server asks for, which of two overlapping replies wins, and swapping in
//! each tab's finished background syntax pass.

use super::*;

impl App {
    /// Drain semantic-token batches and hand each to whichever editor group
    /// currently shows that file. Returns true when an overlay changed and
    /// the next frame should redraw.
    pub fn drain_lsp_semantic_tokens(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        // Collect first so the immutable borrow of `lsp` ends before the
        // editors are mutated below.
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_semantic_tokens() {
            updates.push(u);
        }
        let mut changed = false;
        for u in updates {
            // Drop a batch measured against text the buffer has moved past —
            // an external reload swaps the whole document while a request is
            // still in its retry backoff. Decoding it over the new lines
            // paints the old file's colours at the wrong offsets, and the
            // `is_full` cache store below would persist that under the NEW
            // content's key, replaying it on every future open.
            if !semantic_reply_is_current(
                self.semantic_generation_seen.get(&u.path).copied(),
                u.generation,
            ) {
                continue;
            }
            let mut applied_anywhere = false;
            for group in self.editor_layout.all_groups_mut(&mut self.editor) {
                if group.path.as_deref() == Some(u.path.as_path()) && group.edit_seq == u.seq {
                    group.apply_semantic_tokens(
                        u.path.clone(),
                        u.data.clone(),
                        u.legend.clone(),
                        u.is_full,
                    );
                    changed = true;
                    applied_anywhere = true;
                }
            }
            // Persist whole-document batches so the next open of this exact
            // content paints these colours instantly instead of waiting out the
            // server's cold analysis. Only when a visible editor holds the file
            // unchanged, so the cache key matches the bytes the next open reads.
            if applied_anywhere {
                self.semantic_generation_seen
                    .insert(u.path.clone(), u.generation);
            }
            if u.is_full && applied_anywhere {
                let mut clean_text = (self.editor.path.as_deref() == Some(u.path.as_path()))
                    .then(|| self.editor.clean_cache_text())
                    .flatten();
                if clean_text.is_none() {
                    clean_text = self
                        .editor_layout
                        .inactive_groups_mut()
                        .find(|g| g.path.as_deref() == Some(u.path.as_path()))
                        .and_then(|g| g.clean_cache_text());
                }
                if let Some(text) = clean_text {
                    crate::lsp::semantic_cache::store(&u.path, &text, &u.legend, &u.data);
                }
            }
        }
        changed
    }

    /// Re-pull semantic tokens when a server asked us to via
    /// `workspace/semanticTokens/refresh`. rust-analyzer answers the first
    /// request from partial syntax analysis, then sends refresh once its
    /// cargo/crate-graph analysis resolves the richer type-aware tokens; this
    /// is how that upgraded set reaches the editor without an edit. Only the
    /// visible editor(s) are re-requested (matching VS Code), and only for
    /// files the manager has open. The fresh full batch replaces the partial
    /// one via the editor's `is_full` guard.
    pub fn refresh_semantic_tokens_if_requested(&self) {
        let Some(lsp) = self.lsp.as_ref() else {
            return;
        };
        if !lsp.take_semantic_refresh() {
            return;
        }
        let paths: Vec<PathBuf> = self
            .editor_layout
            .all_groups(&self.editor)
            .filter_map(|g| g.path.clone())
            .collect();
        for p in paths {
            // Re-request at the seq the doc was last synced at, so a reply
            // arriving after an external reload is recognised as stale.
            if let Some(&seq) = self.lsp_last_seen.get(&p) {
                lsp.request_semantic_tokens(p, seq);
            }
        }
    }
}

impl App {
    /// Swap in every tab's background syntax pass that finished since the
    /// last tick (#850), in every editor group. True when any landed, so the
    /// frame repaints in the new colors.
    pub fn poll_highlights(&mut self) -> bool {
        let groups = self.editor_layout.all_groups_mut(&mut self.editor);
        let mut landed = false;
        for ed in groups.flat_map(|g| g.editors.iter_mut()) {
            landed |= ed.poll_highlights();
        }
        landed
    }
}

/// Whether a semantic-token reply may be applied, given the newest request
/// generation already applied for that path. Replies for one path overlap:
/// `sync_lsp` and a server-driven refresh can both have a full request in
/// flight at the same `seq`, and the request's retry loop emits an EMPTY
/// batch once its backoff is exhausted. Without this ordering check a late
/// empty loser blanks a good overlay and — being `is_full` — is persisted to
/// the content-keyed cache and replayed on every future open.
pub(in crate::app) fn semantic_reply_is_current(seen: Option<u64>, incoming: u64) -> bool {
    seen.is_none_or(|s| incoming >= s)
}
