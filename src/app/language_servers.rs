//! Language servers: sync, messages and progress, decorations, go-to and peek,
//! hierarchies, rename, code actions, formatting, linked editing, smart select,
//! highlights, symbol pickers, code lens, outline, breadcrumbs, sticky scroll, restart.

use super::*;

mod breadcrumbs;
mod capabilities;
mod decorations;
mod hierarchy;
mod inlay_hints;
mod messages;
mod navigation;
mod peek;
mod rust_analyzer;
mod semantic;
mod sync;
#[cfg(test)]
pub(super) use breadcrumbs::breadcrumb_symbol_chain;
#[cfg(test)]
pub(super) use breadcrumbs::sibling_symbols;
#[cfg(test)]
pub(super) use peek::peek_excerpt;
#[cfg(test)]
pub(super) use semantic::semantic_reply_is_current;
pub(super) use sync::just_installed_covers_path;
pub(super) use sync::store_diagnostics_update;
pub(super) use sync::tool_on_path;
use sync::*;

/// What to do with one on-type formatting reply (#254). `NotOurs` leaves the
/// armed request alone — a superseded reply must not disarm the newer one
/// still in flight — while `Stale` and `Apply` both consume it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum OnTypeReply {
    NotOurs,
    Stale,
    Apply,
}

/// What a breadcrumb crumb stands for (#1608).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CrumbTarget {
    Folder(PathBuf),
    File(PathBuf),
    /// An index into the outline's symbols.
    Symbol(usize),
}

/// The file the OUTLINE shows and the edit seq it was synced at.
pub(super) type OutlineKey = (PathBuf, Option<u64>);

/// Where an off-thread outline parse delivers its symbols (#1150).
pub(super) type OutlineParse = std::sync::mpsc::Receiver<Vec<crate::lsp::manager::OutlineSymbol>>;

/// A place Peek References can show: path, 0-based line, UTF-16 column.
pub(super) type PeekLocation = (PathBuf, u32, u32);

/// How long a buffer must sit unedited before its code lenses are asked
/// for (#608).
pub(super) const CODE_LENS_SETTLE: std::time::Duration = std::time::Duration::from_millis(600);

/// Maximum pinned sticky-scroll header rows (VS Code's default is 5).
pub(super) const STICKY_SCROLL_MAX: usize = 5;

impl App {
    /// Keep the OUTLINE in sync with the active editor: request fresh symbols
    /// when the file or its edit-seq changed (once per change), settle a
    /// loaded-empty outline for files no language server tracks, and advance
    /// follow-cursor. Returns whether the highlight moved (for redraw).
    pub fn sync_outline(&mut self) -> bool {
        let Some(path) = self.editor.path.clone() else {
            // Back on the welcome screen: drop any stale outline.
            if self.outline.path.is_some() {
                self.outline.clear();
            }
            self.outline_synced = None;
            return false;
        };
        // A tab switch while scrubbing is noticed by render, which runs
        // after this: rebuild here first, or the new file's key would be
        // paired with the previous file's historical text.
        if self.scrubber.is_some() && self.scrub_for.as_ref() != Some(&path) {
            self.rebuild_scrub_view();
        }
        // Scrubbing (#371): the outline is the file as it was at the commit
        // on screen, from its own syntax tree; no language server knows that
        // text. Parsing a big file takes far longer than a frame, so the
        // scrubber's builder parses it off the UI thread with the finished
        // view, and until that lands the previous outline stays. Leaving
        // drops the key, so the live outline is recomputed.
        let commit = self
            .scrubber
            .as_ref()
            .and_then(|s| s.commit())
            .map(|c| c.hash.clone());
        if let (Some(row), Some(commit)) = (self.scrub_view.as_ref().map(|v| v.cursor_row), commit)
        {
            let key = (path.clone(), commit.clone());
            if self.outline_scrub_key.as_ref() != Some(&key) {
                let rel = path
                    .strip_prefix(self.workspace_root())
                    .map(|p| p.to_string_lossy().into_owned());
                let symbols = rel.ok().and_then(|rel| {
                    self.scrub_outlines
                        .get(&crate::scrubber::ViewKey { hash: commit, rel })
                        .cloned()
                });
                if let Some(symbols) = symbols {
                    self.outline.set_syntax_symbols(path, symbols, false);
                    self.outline_scrub_key = Some(key);
                    self.outline_synced = None;
                } else if self.outline.path.as_ref() != Some(&path) {
                    // Another file's outline must never stand in for this
                    // one's: empty until its own lands.
                    self.outline.set_syntax_symbols(path, Vec::new(), false);
                    self.outline_scrub_key = None;
                    self.outline_synced = None;
                }
            }
            return self.outline.follow_caret(row as u32);
        }
        if self.outline_scrub_key.take().is_some() {
            self.outline_synced = None;
        }
        // `lsp_last_seen` carries the edit seq the manager last saw for an open
        // doc; absent means no language server tracks this file.
        let seq = self.lsp_last_seen.get(&path).copied();
        let key = (path.clone(), seq);
        if self.outline_synced.as_ref() != Some(&key) {
            self.outline_synced = Some(key);
            // Tree-sitter paints the outline instantly from the buffer's own
            // syntax tree, before any language server replies. The LSP request
            // below refines it with richer kinds/detail when (and if) it lands.
            let lsp_pending = seq.is_some();
            if outline_parses_inline(&self.editor.lines) {
                let syntax = self.compute_syntax_outline(&path);
                self.outline
                    .set_syntax_symbols(path.clone(), syntax, lsp_pending);
            } else if self.outline.path.as_ref() != Some(&path) {
                // A large buffer is parsed off the UI thread (#1150); until
                // that lands, another file's outline must never stand in.
                self.outline
                    .set_syntax_symbols(path.clone(), Vec::new(), true);
            }
            if lsp_pending && let (Some(lsp), Some(seq)) = (self.lsp.as_ref(), seq) {
                lsp.request_document_symbols(path, seq);
            }
        }
        let landed = self.drain_outline_parse();
        self.start_outline_parse();
        // Follow-cursor every tick; cheap (O(symbols)) and only repaints when
        // the enclosing symbol actually changes.
        self.outline.follow_caret(self.editor.cursor_row as u32) | landed
    }

    /// Parse a large buffer's syntax outline on a worker (#1150): inline, a
    /// multi-megabyte file re-parsed from scratch on every keystroke and each
    /// character took seconds to appear. Starts only when no parse is
    /// running and the buffer moved past the last one started, so a burst of
    /// typing costs one parse when it ends, not one per key.
    pub(super) fn start_outline_parse(&mut self) {
        if self.outline_parse.is_some() || self.outline_parsed_for == self.outline_synced {
            return;
        }
        let Some(key) = self.outline_synced.clone() else {
            return;
        };
        if self.editor.path.as_ref() != Some(&key.0) || outline_parses_inline(&self.editor.lines) {
            return;
        }
        let (path, lines) = (key.0.clone(), self.editor.lines.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(syntax_outline_for(&path, &lines));
        });
        self.outline_parsed_for = Some(key.clone());
        self.outline_parse = Some((key, rx));
    }

    /// Apply a finished off-thread outline parse, if the buffer is still the
    /// one it parsed and the language server's outline of that same buffer
    /// has not landed first. True when the outline changed.
    pub(super) fn drain_outline_parse(&mut self) -> bool {
        let Some((key, rx)) = &self.outline_parse else {
            return false;
        };
        let symbols = match rx.try_recv() {
            Ok(symbols) => symbols,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.outline_parse = None;
                return false;
            }
        };
        let key = key.clone();
        self.outline_parse = None;
        if self.outline_synced.as_ref() != Some(&key) {
            return false;
        }
        let (path, seq) = key;
        if seq.is_some_and(|seq| self.outline_lsp_landed == Some((path.clone(), seq))) {
            return false;
        }
        self.outline
            .set_syntax_symbols(path, symbols, seq.is_some());
        true
    }

    /// Apply a `documentSymbol` reply to the OUTLINE panel, but only when its
    /// `path` is still the active editor file — a reply for a file the user
    /// navigated away from is dropped. Refreshes follow-cursor on success.
    /// Returns whether the outline changed (for redraw).
    pub(super) fn apply_outline_symbols(
        &mut self,
        path: std::path::PathBuf,
        seq: u64,
        symbols: Vec<crate::lsp::manager::OutlineSymbol>,
    ) -> bool {
        if self.editor.path.as_ref() != Some(&path) {
            return false;
        }
        // Drop a reply answering an older buffer than the newest request: its
        // ranges are shifted by the edits made since, the caret falls outside
        // them, and the breadcrumb's symbol crumb flickers off and on with
        // every keystroke (visible against a slow remote rust-analyzer while
        // tree-sitter and the late reply take turns owning the outline). Only
        // enforced when a request seq is tracked for this file; an untracked
        // file has no newer request to be stale against.
        if let Some((p, Some(newest))) = self.outline_synced.as_ref()
            && *p == path
            && seq != *newest
        {
            return false;
        }
        // An empty reply (server lacks `documentSymbol`, or answered before it
        // finished indexing) must not blank an outline tree-sitter already
        // filled. Keep the syntactic symbols rather than flashing "No symbols".
        if symbols.is_empty() && !self.outline.is_empty() {
            return false;
        }
        if !symbols.is_empty() {
            self.outline_lsp_landed = Some((path.clone(), seq));
        }
        self.outline.set_symbols(path, symbols);
        self.outline.follow_caret(self.editor.cursor_row as u32);
        true
    }

    /// Compute the active editor's outline directly from its syntax tree, so
    /// the OUTLINE panel can paint before a (possibly cold) language server
    /// answers `documentSymbol`. Empty for files whose language has no outline
    /// query; those fall back to the LSP reply.
    pub(super) fn compute_syntax_outline(
        &self,
        path: &std::path::Path,
    ) -> Vec<crate::lsp::manager::OutlineSymbol> {
        syntax_outline_for(path, &self.editor.lines)
    }

    /// Drain pending `documentSymbol` replies and offer every one to
    /// `apply_outline_symbols`, which is the sole judge of what applies.
    ///
    /// Deliberately no pre-selection. The drain used to collapse a tick's
    /// replies to whichever ARRIVED last and discard the rest, which lost the
    /// active file's reply whenever a slow one for the file just left landed
    /// after it — the guard then rejected the survivor on path, and
    /// `sync_outline` never re-requests because its `(path, seq)` key has not
    /// changed. Picking the highest seq instead would be just as wrong: `seq`
    /// is a per-`Editor` counter that RESTARTS at zero when a file lands in a
    /// fresh editor group (a split, or close-and-reopen), so the outstanding
    /// request can carry a lower number than a reply still in flight from the
    /// old group. Offering all of them keeps the decision in the one place
    /// that knows which request is outstanding.
    pub fn drain_lsp_document_symbols(&mut self) -> bool {
        let mut replies = Vec::new();
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(r) = lsp.drain_document_symbols() {
                replies.push((r.path, r.seq, r.symbols));
            }
        }
        let mut applied = false;
        for (path, seq, symbols) in replies {
            applied |= self.apply_outline_symbols(path, seq, symbols);
        }
        applied
    }

    /// On-type formatting (#254), run once per frame: when enabled and the
    /// last edit was a keystroke in the server's advertised trigger set,
    /// fire `textDocument/onTypeFormatting` at the caret. The typed record
    /// is consumed here — examined exactly once, on the frame right after
    /// the keystroke — so a later tab switch or settings toggle can never
    /// replay a stale trigger. Never fires unadvertised: the trigger set
    /// comes from the capability the server declared, and `None` (no server
    /// probed, or no provider) means stay inert.
    pub fn tick_on_type_formatting(&mut self) {
        let taken = self.editor.last_typed.take();
        // A keystroke and a tab switch can arrive in the same input burst,
        // draining before this tick runs — the record is then armed on a tab
        // that is no longer active, and nothing else ever clears it. Sweep
        // every editor in every group (the active one is already drained by
        // the `take` above) so a trigger can only ever fire from the tab
        // that was active at its tick.
        let groups =
            std::iter::once(&mut self.editor).chain(self.editor_layout.inactive_groups_mut());
        for ed in groups.flat_map(|g| g.editors.iter_mut()) {
            ed.last_typed = None;
        }
        let Some((ch, seq)) = taken else {
            return;
        };
        if !self.format_on_type_for(editor_scope_id(&self.editor)) || !self.editor_is_text() {
            return;
        }
        if seq != self.editor.edit_seq {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lang) = crate::lsp::Language::from_path(&path) else {
            return;
        };
        let triggers = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.language_on_type_triggers(lang));
        if !triggers.is_some_and(|t| t.contains(ch)) {
            return;
        }
        let (line, character) = self
            .editor
            .pos_to_utf16(self.editor.cursor_row, self.editor.cursor_col);
        let (tab_size, insert_spaces) = self.editor.indent_preference();
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_on_type_formatting(
            path.clone(),
            line,
            character,
            ch,
            tab_size,
            insert_spaces,
        );
        self.on_type_request = Some((id, seq, path));
    }

    /// What to do with one on-type formatting reply. Extracted from the
    /// drain loop so the three-way decision is testable without a live
    /// server: the channel's sender lives inside the LSP worker, so a test
    /// cannot inject a `FormatResult` through it.
    pub(super) fn on_type_reply_disposition(
        &self,
        result: &crate::lsp::manager::FormatResult,
    ) -> OnTypeReply {
        let Some((id, seq, ref requested)) = self.on_type_request else {
            return OnTypeReply::NotOurs;
        };
        // A superseded reply must NOT clear the armed request: a newer one
        // is in flight and its own reply still has to apply.
        if result.request_id != id {
            return OnTypeReply::NotOurs;
        }
        // `edit_seq` is per-tab, so the counter comparison only means
        // "buffer unchanged" when the active tab is still the one the
        // request was computed against — require the path to match too.
        // Content-unchanged is also the whole test on purpose: caret-only
        // motion (arrow keys, auto-pair type-over) leaves the counter
        // alone, and that is correct — the server's edits are document
        // edits computed against text that is still byte-identical, so
        // they apply cleanly wherever the caret has wandered.
        if self.editor.edit_seq != seq
            || requested != &result.path
            || self.editor.path.as_deref() != Some(result.path.as_path())
        {
            return OnTypeReply::Stale;
        }
        OnTypeReply::Apply
    }

    /// Apply an on-type formatting reply — quietly, and only while the
    /// buffer has not moved since the request (the edits were computed
    /// against exactly that text; continued typing makes them stale).
    pub fn drain_lsp_on_type_format(&mut self) -> bool {
        let mut results = Vec::new();
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_on_type_formatting() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            match self.on_type_reply_disposition(&result) {
                OnTypeReply::NotOurs => continue,
                OnTypeReply::Stale => {
                    self.on_type_request = None;
                    continue;
                }
                OnTypeReply::Apply => self.on_type_request = None,
            }
            if let Some(edits) = result.edits.filter(|e| !e.is_empty())
                && self.apply_on_type_edits(&result.path, &edits)
            {
                changed = true;
            }
        }
        changed
    }

    /// Apply an on-type formatting reply for `path`, the active tab's file
    /// (the reply's disposition checked that), keeping the caret beside the
    /// text that was just typed.
    pub(crate) fn apply_on_type_edits(
        &mut self,
        path: &std::path::Path,
        edits: &[crate::widgets::editor::TextSpanEdit],
    ) -> bool {
        if self.editor.path.as_deref() == Some(path) {
            return self.editor.apply_span_edits_moving_caret(edits) > 0;
        }
        let applied = self.editor.apply_rename_to_open_tab(path, edits).is_some();
        self.editor.clamp_cursor();
        applied
    }

    /// Linked editing (#254): replay the last keystroke across the
    /// sibling tag ranges, keep the set alive only while the caret
    /// stays inside one, and (debounced on caret idle) ask the server
    /// for the set at a new caret position. Never fires a request for a
    /// language the LSP layer isn't tracking; a server without the
    /// provider simply never answers, and the feature stays inert.
    pub fn tick_linked_editing(&mut self) -> bool {
        const LINKED_IDLE: std::time::Duration = std::time::Duration::from_millis(250);
        let mirrored = self.editor.mirror_linked_edit();
        let eligible = self.editor_is_text() && self.lsp.is_some();
        let Some(path) = self.editor.path.clone().filter(|_| eligible) else {
            self.linked_observed = None;
            self.linked_request = None;
            self.linked_asked = false;
            self.editor.clear_linked_ranges();
            return mirrored;
        };
        let (row, col) = (self.editor.cursor_row, self.editor.cursor_col);
        // While the caret stays inside the live set, keep it — the whole
        // point is typing inside a range. (The mirror above re-synced
        // positions for this frame's keystroke.)
        if self.editor.has_linked_ranges() {
            if self.editor.linked_ranges_contain(row, col) {
                return mirrored;
            }
            self.editor.clear_linked_ranges();
        }
        let cur = (path, row, col, self.editor.edit_seq);
        if self.linked_observed.as_ref() != Some(&cur) {
            self.linked_observed = Some(cur);
            self.linked_observed_at = std::time::Instant::now();
            self.linked_request = None;
            self.linked_asked = false;
            return mirrored;
        }
        if self.linked_asked || self.linked_observed_at.elapsed() < LINKED_IDLE {
            return mirrored;
        }
        let (uline, uchar) = self.editor.cursor_position_utf16();
        let Some(lsp) = self.lsp.as_mut() else {
            return mirrored;
        };
        let id = lsp.request_linked_editing(cur.0.clone(), uline, uchar);
        self.linked_request = Some((id, cur.0, cur.3));
        self.linked_asked = true;
        mirrored
    }

    /// Install a linkedEditingRange reply, gated on the in-flight id,
    /// the path, and the edit seq the spans were computed against.
    pub fn drain_lsp_linked_editing(&mut self) -> bool {
        let mut results = Vec::new();
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_linked_editing() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            let Some((id, path, seq)) = self.linked_request.as_ref() else {
                continue;
            };
            if result.request_id != *id
                || self.editor.path.as_deref() != Some(path.as_path())
                || self.editor.edit_seq != *seq
            {
                continue;
            }
            self.linked_request = None;
            self.editor.set_linked_ranges(&result.ranges);
            changed = true;
        }
        changed
    }

    /// Expand Selection (#254): grow every cursor to the next
    /// semantically meaningful range. Cached chain steps apply
    /// instantly; otherwise the LSP `selectionRange` chains are fetched
    /// (one request, all cursors), with tree-sitter node ancestry as the
    /// serverless fallback — so the gesture works with no LSP at all.
    pub(super) fn expand_selection(&mut self) {
        if !self.editor_is_text() {
            return;
        }
        self.editor.validate_expand_stacks();
        if self.editor.expand_selection_from_stack() {
            return;
        }
        let lang_known = self.editor_language().is_some();
        if lang_known
            && self.lsp.is_some()
            && let Some(path) = self.editor.path.clone()
        {
            let seq = self.editor.edit_seq;
            let positions = self.editor.cursor_positions_utf16();
            // Reuse the pending request only when it was fired for these
            // exact cursors; a caret move re-fires for the new spot.
            if let Some(pending) = self.selection_range_request.as_mut()
                && pending.path == path
                && pending.seq == seq
                && pending.positions == positions
            {
                pending.presses += 1;
                return;
            }
            if let Some(lsp) = self.lsp.as_mut() {
                let id = lsp.request_selection_ranges(path.clone(), positions.clone());
                self.selection_range_request = Some(PendingSelectionRanges {
                    id,
                    path,
                    seq,
                    presses: 1,
                    positions,
                });
            }
            return;
        }
        if !self.editor.expand_selection_syntax() {
            self.status = String::from("No larger range to select");
        }
    }

    /// Shrink Selection: retrace the expand stack one step. Purely
    /// local — the stack already remembers the exact ranges.
    pub(super) fn shrink_selection(&mut self) {
        if !self.editor_is_text() {
            return;
        }
        self.editor.shrink_selection_step();
    }

    /// Drain `selectionRange` replies (#254): triple-gated like
    /// occurrences (id + path + edit_seq), then either install the
    /// server's chains or resolve the queued presses via tree-sitter
    /// when the request came back unsupported/empty.
    pub(super) fn drain_lsp_selection_ranges(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut results = Vec::new();
        while let Some(r) = lsp.drain_selection_ranges() {
            results.push(r);
        }
        let mut changed = false;
        for r in results {
            let Some(pending) = self.selection_range_request.as_ref() else {
                continue;
            };
            // Gate on id + path + edit_seq AND the cursors the request
            // was fired for: a click between request and reply leaves
            // `edit_seq` unchanged, but the chains describe the OLD
            // caret and must not apply at the new one.
            if r.request_id != pending.id
                || self.editor.path.as_deref() != Some(pending.path.as_path())
                || self.editor.edit_seq != pending.seq
                || self.editor.cursor_positions_utf16() != pending.positions
            {
                if r.request_id == pending.id {
                    self.selection_range_request = None;
                }
                continue;
            }
            let presses = pending.presses;
            self.selection_range_request = None;
            if r.unsupported || r.chains.is_empty() {
                for _ in 0..presses {
                    if !self.editor.expand_selection_syntax() {
                        break;
                    }
                }
                changed = true;
                continue;
            }
            self.editor.install_selection_chains(r.chains);
            for _ in 0..presses {
                if !self.editor.expand_selection_from_stack() {
                    break;
                }
            }
            changed = true;
        }
        changed
    }

    /// Idle-caret occurrences trigger, run once per frame: when the caret has
    /// rested on one buffer position for `OCCURRENCES_IDLE`, fire ONE
    /// `documentHighlight` request for it; the moment it moves (or the buffer
    /// edits), drop the painted set. Mirrors VS Code's word highlight, which
    /// follows the cursor with a small debounce rather than every keystroke.
    pub fn tick_occurrences(&mut self) -> bool {
        const OCCURRENCES_IDLE: std::time::Duration = std::time::Duration::from_millis(250);
        let eligible = self.editor.diff.is_none()
            && self.editor.sheet.is_none()
            && self.editor.image.is_none()
            && self.lsp.is_some();
        let Some(path) = self.editor.path.clone().filter(|_| eligible) else {
            self.occ_observed = None;
            self.occurrences_request = None;
            return self.editor.clear_occurrences();
        };
        let cur = (
            path,
            self.editor.cursor_row,
            self.editor.cursor_col,
            self.editor.edit_seq,
        );
        if self.occ_observed.as_ref() != Some(&cur) {
            self.occ_observed = Some(cur);
            self.occ_observed_at = std::time::Instant::now();
            // The old symbol's tints are stale the moment the caret leaves it
            // (an edit already cleared them via mark_buffer_changed), and an
            // in-flight reply for the old spot must never paint late.
            self.occurrences_request = None;
            return self.editor.clear_occurrences();
        }
        let already_requested = self
            .occurrences_request
            .as_ref()
            .is_some_and(|(_, p, r, c, s)| (p, r, c, s) == (&cur.0, &cur.1, &cur.2, &cur.3));
        if already_requested || self.occ_observed_at.elapsed() < OCCURRENCES_IDLE {
            return false;
        }
        let (uline, uchar) = self.editor.pos_to_utf16(cur.1, cur.2);
        let Some(lsp) = self.lsp.as_mut() else {
            return false;
        };
        let id = lsp.request_document_highlights(cur.0.clone(), uline, uchar);
        self.occurrences_request = Some((id, cur.0, cur.1, cur.2, cur.3));
        false
    }

    /// Apply a documentHighlight reply to the editor, dropping it unless it
    /// answers the in-flight request AND the editor still shows that file at
    /// that edit seq (the columns were computed against that text).
    pub fn drain_lsp_document_highlights(&mut self) -> bool {
        let mut results = Vec::new();
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_document_highlights() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            let Some((id, path, _row, _col, seq)) = self.occurrences_request.as_ref() else {
                continue;
            };
            if result.request_id != *id
                || self.editor.path.as_deref() != Some(path.as_path())
                || self.editor.edit_seq != *seq
            {
                continue;
            }
            self.editor.apply_occurrences(result.items);
            changed = true;
        }
        changed
    }
}

impl App {
    /// VS Code "Rename Symbol" (F2): prompt for a new name pre-filled with the
    /// identifier under the cursor; committing fires an LSP `rename` request.
    pub(super) fn start_rename_symbol(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return;
        };
        if self.lsp.is_none() {
            self.status = String::from("No language server for this file");
            return;
        }
        // Prepare-rename first (#254): the server validates the position
        // and hands back the exact range + placeholder, so the prompt
        // pre-fills the real symbol and an invalid position fails fast
        // with the server's own message. The worker always answers;
        // an `unsupported` verdict falls back to the word-under-cursor
        // prompt below (today's behavior, kept for servers without the
        // capability).
        let (row, col) = (self.editor.cursor_row, self.editor.cursor_col);
        let (line, character) = self.editor.pos_to_utf16(row, col);
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_prepare_rename(path.clone(), line, character);
        // Overwriting also disarms any previous in-flight prepare: its caret
        // is abandoned, and a late verdict for it must not reopen a prompt
        // carrying the old (row, col) into the rename request.
        self.prepare_rename_request = Some((id, path, row, col, self.editor.edit_seq));
        self.status = String::from("Preparing rename…");
    }

    /// The pre-#254 rename prompt: pre-fill with the word under the
    /// cursor. The fallback when no server advertises `prepareProvider`
    /// (or it answers "default behavior").
    pub(super) fn open_rename_prompt_fallback(&mut self, path: PathBuf, row: usize, col: usize) {
        let Some((start, end)) = self.editor.word_at(row, col) else {
            self.status = String::from("No symbol under cursor");
            return;
        };
        let word: String = self.editor.lines[row]
            .chars()
            .skip(start)
            .take(end - start)
            .collect();
        self.prompt = Some(Prompt {
            label: format!("Rename Symbol '{word}'"),
            buffer: word,
            caret: usize::MAX,
            kind: PromptKind::RenameSymbol { path, row, col },
            target_dir: PathBuf::new(),
            error: None,
        });
    }

    /// Resolve a prepare-rename verdict (#254) into the prompt, an
    /// error, or the fallback. Gated on id + path + edit_seq like every
    /// deferred caret request.
    pub fn drain_lsp_prepare_rename(&mut self) -> bool {
        let mut results = Vec::new();
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_prepare_rename() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            changed |= self.apply_prepare_rename_verdict(result);
        }
        changed
    }

    /// Resolve one prepare-rename verdict into the prompt, a status, or the
    /// word-under-cursor fallback. Split out of the drain loop so the four
    /// verdict shapes are testable: the reply channel's sender lives inside
    /// the LSP worker, so a test cannot inject a `PrepareRenameResult`
    /// through it. Returns whether the screen needs a repaint.
    pub(super) fn apply_prepare_rename_verdict(
        &mut self,
        result: crate::lsp::manager::PrepareRenameResult,
    ) -> bool {
        let Some((id, path, row, col, seq)) = self.prepare_rename_request.clone() else {
            return false;
        };
        if result.request_id != id {
            return false;
        }
        self.prepare_rename_request = None;
        // The verdict is in, whichever way it went: "Preparing rename…" no
        // longer describes anything (it stayed up beside the open prompt).
        if self.status == "Preparing rename…" {
            self.status.clear();
        }
        if self.editor.path.as_deref() != Some(path.as_path()) || self.editor.edit_seq != seq {
            // The caret moved on (an edit, or a tab switch — which also
            // bumps edit_seq). Say so: bailing silently would strand
            // "Preparing rename…" on screen with no way to tell the
            // rename died.
            self.status = String::from("Rename cancelled");
            return true;
        }
        if result.unsupported {
            self.open_rename_prompt_fallback(path, row, col);
            return true;
        }
        if let Some(msg) = result.error {
            self.status = format!("Rename rejected: {msg}");
            return true;
        }
        let Some(((sl, sc, el, ec), placeholder)) = result.target else {
            self.status = String::from("Nothing renameable at the cursor");
            return true;
        };
        // Placeholder wins; otherwise the validated range's text.
        let word = placeholder.unwrap_or_else(|| {
            let (sr, er) = (sl as usize, el as usize);
            let scol = self.editor.utf16_col_to_char_pub(sr, sc);
            let ecol = self.editor.utf16_col_to_char_pub(er, ec);
            if sr == er {
                self.editor
                    .lines
                    .get(sr)
                    .map(|l| {
                        l.chars()
                            .skip(scol)
                            .take(ecol.saturating_sub(scol))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            }
        });
        if word.is_empty() {
            self.open_rename_prompt_fallback(path, row, col);
            return true;
        }
        self.prompt = Some(Prompt {
            label: format!("Rename Symbol '{word}'"),
            buffer: word,
            caret: usize::MAX,
            kind: PromptKind::RenameSymbol { path, row, col },
            target_dir: PathBuf::new(),
            error: None,
        });
        true
    }

    pub fn drain_lsp_rename(&mut self) -> bool {
        let mut arrived = false;
        let mut edits: Option<Vec<(PathBuf, Vec<crate::widgets::editor::TextSpanEdit>)>> = None;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(result) = lsp.drain_rename() {
                if Some(result.request_id) != self.rename_request_id {
                    continue;
                }
                arrived = true;
                edits = result.edits;
            }
        }
        if !arrived {
            return false;
        }
        self.rename_request_id = None;
        self.land_rename(edits);
        true
    }

    /// Apply a rename reply, unless a buffer it edits was typed into after
    /// the request went out (#1151): its positions were computed against
    /// the text as it was then, and would land on whatever sits there now.
    pub(super) fn land_rename(
        &mut self,
        edits: Option<Vec<(PathBuf, Vec<crate::widgets::editor::TextSpanEdit>)>>,
    ) {
        let baseline = std::mem::take(&mut self.rename_baseline);
        match edits {
            Some(files) => {
                if let Some(file) = self.stale_edit_target(&baseline, &files) {
                    self.status = format!(
                        "Rename cancelled: {file} changed while the server was working; press F2 again"
                    );
                    return;
                }
                match self.apply_rename_edits(&files) {
                    Ok((file_count, occ)) => {
                        self.status =
                            format!("Renamed {occ} occurrence(s) across {file_count} file(s)");
                    }
                    Err(e) => self.status = format!("Rename failed: {e}"),
                }
            }
            None => self.status = String::from("Rename produced no changes"),
        }
    }

    /// The first file of a workspace edit whose open buffer changed after
    /// `baseline` was taken, named for the status bar (#1151).
    pub(super) fn stale_edit_target(
        &self,
        baseline: &[(PathBuf, u64)],
        files: &[(PathBuf, Vec<crate::widgets::editor::TextSpanEdit>)],
    ) -> Option<String> {
        files
            .iter()
            .filter(|(_, edits)| !edits.is_empty())
            .find(|(path, _)| {
                self.buffer_group(path)
                    .is_some_and(|group| group.edited_since(baseline, path))
            })
            .map(|(path, _)| self.status_path(path))
    }

    /// Every open tab's file and `edit_seq`, in every split group: the
    /// baseline a workspace edit is checked against when it lands (#1151).
    pub(super) fn edit_seqs_in_every_group(&self) -> Vec<(PathBuf, u64)> {
        std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|group| group.edit_seqs())
            .collect()
    }

    /// The editor group whose tab holds `path`'s text, the active group
    /// first: the one [`Self::apply_edits_to_open_buffer`] edits. A split
    /// may show only a diff of the file while another holds the file
    /// itself (#1113).
    pub(super) fn buffer_group(&self, path: &Path) -> Option<&EditorTabs> {
        std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .find(|group| group.find_buffer_tab(path).is_some())
    }

    /// Apply workspace `edits` to the tab holding `path`'s text in any
    /// split group, the active group first. `None` when no group holds it,
    /// so the caller edits the file on disk.
    pub(super) fn apply_edits_to_open_buffer(
        &mut self,
        path: &Path,
        edits: &[crate::widgets::editor::TextSpanEdit],
    ) -> Option<usize> {
        if let Some(n) = self.editor.apply_rename_to_open_tab(path, edits) {
            return Some(n);
        }
        self.editor_layout
            .inactive_groups_mut()
            .into_iter()
            .find_map(|group| group.apply_rename_to_open_tab(path, edits))
    }

    /// Apply an LSP rename `WorkspaceEdit` (already normalised to per-file
    /// char-indexed spans). Open tabs, in any split group, are edited
    /// in-memory as one undo step and left dirty; closed files are rewritten
    /// on disk (the fs watcher refreshes the tree) and the servers are told
    /// at once (#908), so the next Go to Definition sees the new name.
    /// Returns (files touched, occurrences replaced).
    pub(super) fn apply_rename_edits(
        &mut self,
        files: &[(PathBuf, Vec<crate::widgets::editor::TextSpanEdit>)],
    ) -> Result<(usize, usize)> {
        let mut file_count = 0;
        let mut occ_count = 0;
        let mut written = std::collections::BTreeSet::new();
        for (path, edits) in files {
            if edits.is_empty() {
                continue;
            }
            if let Some(n) = self.apply_edits_to_open_buffer(path, edits) {
                occ_count += n;
            } else if self.is_collab_guest() && collab_file_key(&self.tree.root, path).is_some() {
                // Closed-file rename edits are raw disk writes; a collab
                // guest never writes shared files (open tabs above are fine:
                // those edits flow to the owner as ops).
                continue;
            } else {
                let content = std::fs::read_to_string(path)?;
                let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();
                occ_count += crate::widgets::editor::apply_span_edits_to_lines(&mut lines, edits);
                std::fs::write(path, lines.join("\n"))?;
                written.insert(path.clone());
            }
            file_count += 1;
        }
        self.forward_disk_changes_to_lsp(&fs_watch::FsDrain {
            changed_files: written,
            ..Default::default()
        });
        Ok((file_count, occ_count))
    }

    /// VS Code "Format Document" (Cmd+Opt+Shift+F): ask the language server to
    /// reformat the active buffer. No-op on read-only diff / sheet / image
    /// tabs. The reply is applied in [`drain_lsp_format`].
    pub(super) fn start_format_document(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return;
        };
        // A Prettier project formats with its own Prettier and config, as
        // VS Code's Prettier extension does; tsserver's formatter knows
        // neither (#1255).
        if let Some(bin) = crate::prettier::for_file(&path) {
            let sent = self.editor.document_text();
            self.prettier_job = Some(crate::prettier::Job::spawn(bin, path, sent));
            self.status = String::from("Formatting document with Prettier");
            return;
        }
        if self.lsp.is_none() {
            self.status = String::from("No language server for this file");
            return;
        }
        // Mirror the editor's indentation preference; servers that carry their
        // own config (rustfmt, ruff) ignore these but the fields are
        // required by the LSP request.
        let (tab_size, insert_spaces) = self.editor.indent_preference();
        let seq = self.format_target_seq(&path);
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_formatting(path, tab_size, insert_spaces);
        self.format_request_id = Some(id);
        self.format_request_seq = seq;
        self.format_request_selection = false;
        self.status = String::from("Formatting document");
    }

    /// The `edit_seq` of the tab a format reply for `path` lands in (the
    /// one `apply_rename_to_open_tab` picks), or `None` when no tab holds it.
    pub(super) fn format_target_seq(&self, path: &std::path::Path) -> Option<u64> {
        let idx = self.editor.find_buffer_tab(path)?;
        Some(self.editor.editors[idx].edit_seq)
    }

    /// "Format Selection" (#254): `textDocument/rangeFormatting` over the
    /// primary selection, applied through the same reply path as
    /// whole-document formatting.
    pub(super) fn format_selection(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let Some(sel) = self.editor.selection.filter(|s| s.has_area()) else {
            self.status = String::from("Select something to format");
            return;
        };
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return;
        };
        let ((sr, sc), (er, ec)) = sel.normalised();
        let start = self.editor.pos_to_utf16(sr, sc);
        let end = self.editor.pos_to_utf16(er, ec);
        let (tab_size, insert_spaces) = self.editor.indent_preference();
        let seq = self.format_target_seq(&path);
        let Some(lsp) = self.lsp.as_mut() else {
            self.status = String::from("No language server for this file");
            return;
        };
        let id = lsp.request_range_formatting(path, start, end, tab_size, insert_spaces);
        self.format_request_id = Some(id);
        self.format_request_seq = seq;
        self.format_request_selection = true;
        self.status = String::from("Formatting selection");
    }

    /// Apply a finished Prettier run to the tab it formatted, then let the
    /// save it held go ahead. A buffer edited while Prettier ran keeps the
    /// edit: the result only fits the text that was sent.
    pub fn drain_prettier_format(&mut self) -> bool {
        let Some(reply) = self
            .prettier_job
            .as_ref()
            .and_then(|job| job.poll(std::time::Instant::now()))
        else {
            return false;
        };
        let Some(job) = self.prettier_job.take() else {
            return false;
        };
        let formatted = match reply {
            Ok(formatted) => formatted,
            Err(e) => {
                // Said after the save, which would otherwise cover it with
                // "Saved".
                let saving = self.save_after_format.is_some();
                self.complete_pending_save();
                self.status = match saving {
                    true => format!("Prettier failed: {e}; saved without formatting"),
                    false => format!("Prettier failed: {e}"),
                };
                return true;
            }
        };
        let tab = self.editor.find_buffer_tab(&job.path);
        let buffer = tab
            .map(|i| &self.editor.editors[i])
            .map(|e| (e.document_text(), e.lines.clone(), e.has_final_newline()));
        self.status = match buffer {
            Some((text, lines, final_newline)) if text == job.sent => {
                if formatted == job.sent {
                    String::from("Document already formatted")
                } else {
                    // The buffer keeps no line for a final newline it already
                    // has; one Prettier adds becomes the empty last line that
                    // writes it.
                    let body = match final_newline {
                        true => formatted.strip_suffix('\n').unwrap_or(&formatted),
                        false => &formatted,
                    };
                    let edits = crate::prettier::edits_for(&lines, body);
                    self.editor.apply_rename_to_open_tab(&job.path, &edits);
                    self.editor.clamp_cursor();
                    String::from("Formatted document with Prettier")
                }
            }
            Some(_) => String::from("Prettier result dropped: the file changed while it ran"),
            None => String::from("Format failed: document not open"),
        };
        self.complete_pending_save();
        true
    }

    pub fn drain_lsp_format(&mut self) -> bool {
        self.drain_lsp_format_at(std::time::Instant::now())
    }

    /// Apply a formatter reply that has arrived, or, once a format-on-save
    /// has waited `FORMAT_ON_SAVE_TIMEOUT` with none, write its file
    /// unformatted: a busy, hung or unreachable server left it unsaved for
    /// good (#1491). Returns true when the next frame should redraw.
    pub(super) fn drain_lsp_format_at(&mut self, now: std::time::Instant) -> bool {
        let mut arrived = false;
        let mut edits: Option<Vec<crate::widgets::editor::TextSpanEdit>> = None;
        let mut unsupported = false;
        let mut path: Option<PathBuf> = None;
        let mut server: Option<String> = None;
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(result) = lsp.drain_formatting() {
                if Some(result.request_id) != self.format_request_id {
                    continue;
                }
                arrived = true;
                edits = result.edits;
                unsupported = result.unsupported;
                path = Some(result.path);
                server = result.server;
            }
        }
        if !arrived {
            let overdue = self.save_after_format.is_some()
                && self
                    .format_save_deadline
                    .is_some_and(|deadline| now >= deadline);
            if overdue {
                let secs = FORMAT_ON_SAVE_TIMEOUT.as_secs();
                self.save_without_formatting(&format!("the formatter did not answer in {secs} s"));
            }
            return overdue;
        }
        self.format_request_id = None;
        self.land_format(edits, unsupported, path, server.as_deref());
        true
    }

    /// Write a format-on-save's file now, unformatted, and drop the reply
    /// it was waiting on, which would otherwise land on the saved text.
    pub(super) fn save_without_formatting(&mut self, reason: &str) {
        let Some(path) = self.save_after_format.clone() else {
            return;
        };
        self.format_request_id = None;
        self.format_request_seq = None;
        self.format_request_selection = false;
        self.complete_pending_save();
        // A refused write (a disk conflict, a lossy encoding) or a skipped
        // one (the tab was closed meanwhile) keeps the status it set.
        if self.path_is_saved(&path) {
            self.status = format!(
                "Saved {} without formatting: {reason}",
                self.status_path(&path)
            );
        }
    }

    /// Whether `path` is open in some tab, in any split, and none of its
    /// tabs has unsaved edits. A closed file was not written, so it is not
    /// saved either.
    pub(super) fn path_is_saved(&self, path: &Path) -> bool {
        let mut tabs = std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .filter(|e| e.path.as_deref() == Some(path))
            .peekable();
        tabs.peek().is_some() && tabs.all(|e| !e.dirty)
    }

    /// Apply a formatter's reply to the request in flight, then finish a
    /// format-on-save. Split from [`Self::drain_lsp_format`] so tests can
    /// deliver a reply without a server.
    pub(super) fn land_format(
        &mut self,
        edits: Option<Vec<crate::widgets::editor::TextSpanEdit>>,
        unsupported: bool,
        path: Option<PathBuf>,
        server: Option<&str>,
    ) {
        let selection = std::mem::take(&mut self.format_request_selection);
        let requested_at = self.format_request_seq.take();
        // bash-language-server formats only through shfmt; without it, it
        // answers every request with no edits (#1511), which is not "already
        // formatted". Another formatter's empty reply is its own answer.
        let shfmt_missing = server == Some(BASH_SERVER)
            && path.as_deref().is_some_and(is_shell_script)
            && !tool_on_path("shfmt", self.tool_search_path.as_deref());
        if unsupported {
            self.status = if selection {
                String::from("No range formatter available for this file")
            } else {
                String::from("No formatter available for this file")
            };
            self.complete_pending_save();
            return;
        }
        match (edits, path) {
            // The edits are positions in the text the server was sent; typed
            // into since, they land on the wrong lines (#1127). Drop them, as
            // VS Code does, and let a format-on-save write what is there.
            (Some(edits), Some(path))
                if !edits.is_empty() && self.format_target_seq(&path) != requested_at =>
            {
                let file = self.status_path(&path);
                let saving = self.save_after_format.is_some();
                self.complete_pending_save();
                self.status = if saving {
                    format!(
                        "Saved {file} without formatting: it changed while the formatter was running"
                    )
                } else {
                    format!(
                        "Format skipped: {file} changed while the formatter was running; format again"
                    )
                };
                return;
            }
            (Some(edits), Some(path)) if !edits.is_empty() => {
                match self.editor.apply_rename_to_open_tab(&path, &edits) {
                    Some(_) => {
                        self.editor.clamp_cursor();
                        self.status = if selection {
                            String::from("Formatted selection")
                        } else {
                            String::from("Formatted document")
                        };
                    }
                    None => self.status = String::from("Format failed: document not open"),
                }
            }
            _ if shfmt_missing => self.status = String::from(SHFMT_MISSING),
            _ => {
                self.status = if selection {
                    String::from("Selection already formatted")
                } else {
                    String::from("Document already formatted")
                }
            }
        }
        // A format-on-save write is deferred until now so the disk file gets the
        // formatted text in one go.
        self.complete_pending_save();
    }

    /// The range a Quick Fix asks the server about, as UTF-16 `(line, col)`
    /// start and end, with the diagnostics on its lines as context: the
    /// selection when there is one (VS Code sends it, so selection-scoped
    /// refactors like Extract Function are offered), else the caret point.
    #[allow(clippy::type_complexity)]
    pub(super) fn code_action_request(
        &self,
    ) -> ((u32, u32), (u32, u32), Vec<crate::lsp::manager::Diagnostic>) {
        let ((sr, sc), (er, ec)) = match self.editor.selection.filter(|s| s.has_area()) {
            Some(sel) => sel.normalised(),
            None => {
                let caret = (self.editor.cursor_row, self.editor.cursor_col);
                (caret, caret)
            }
        };
        let start = self.editor.pos_to_utf16(sr, sc);
        let end = self.editor.pos_to_utf16(er, ec);
        // Rows are plain line numbers, unaffected by the UTF-16 columns.
        let diagnostics = self.editor.diagnostics_in_line_range(start.0, end.0);
        (start, end, diagnostics)
    }

    /// VS Code "Quick Fix" (`Cmd+.`): ask the language server for the code
    /// actions available at the cursor (with the diagnostics on that line as
    /// context) and present them in a picker. No-op on read-only preview tabs.
    /// The reply is handled in [`Self::drain_lsp_code_actions`].
    pub(super) fn start_code_action(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        // A merge conflict at the cursor takes the Quick Fix over LSP code
        // actions, like VS Code's Accept Current / Incoming / Both lenses:
        // resolving markers is what the user wants there, and it needs no
        // server round-trip.
        let cursor_row = self.editor.cursor_row;
        if crate::merge::conflict_containing(self.editor.conflicts(), cursor_row).is_some() {
            self.open_merge_conflict_picker();
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file open");
            return;
        };
        let ((sr, sc), (er, ec), diagnostics) = self.code_action_request();
        let Some(lsp) = self.lsp.as_mut() else {
            self.status = String::from("No language server for this file");
            return;
        };
        let id = lsp.request_code_action(path, sr, sc, er, ec, diagnostics);
        self.code_action_request_id = Some(id);
        self.code_action_pending_resolve = false;
        self.code_action_baseline = self.edit_seqs_in_every_group();
        self.status = String::from("Finding quick fixes");
    }

    pub fn drain_lsp_code_actions(&mut self) -> bool {
        let mut arrived = false;
        let mut items: Option<Vec<crate::lsp::manager::CodeActionItem>> = None;
        let mut unsupported = false;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(result) = lsp.drain_code_action() {
                if Some(result.request_id) != self.code_action_request_id {
                    continue;
                }
                arrived = true;
                items = result.items;
                unsupported = result.unsupported;
            }
        }
        if !arrived {
            return false;
        }
        self.code_action_request_id = None;
        let was_resolve = std::mem::take(&mut self.code_action_pending_resolve);
        if unsupported {
            // No code-action server, but a seated navigator can still take
            // the diagnostic under the caret (#374).
            if self.navigator_fix_candidate().is_some() {
                self.open_code_action_picker(Vec::new());
            } else {
                self.status = String::from("No quick fixes: no code-action server for this file");
            }
            return true;
        }
        // A resolve reply carries the one action the user already chose, now with
        // its edit filled in: apply it directly instead of re-opening a menu.
        if was_resolve {
            match items.and_then(|mut v| v.pop()) {
                Some(item) => self.apply_resolved_code_action(item),
                None => self.status = String::from("Quick fix could not be resolved"),
            }
            return true;
        }
        match items {
            Some(items) if items.len() == 1 && items[0].needs_resolve => {
                // A single resolve-only action (e.g. a deferred auto-import):
                // resolve it straight away rather than popping a one-row menu.
                self.resolve_and_apply_code_action(items.into_iter().next().unwrap());
            }
            Some(items) if !items.is_empty() => self.open_code_action_picker(items),
            // Nothing from the server: the navigator entry alone, when a
            // diagnostic sits under the caret (#374).
            _ if self.navigator_fix_candidate().is_some() => {
                self.open_code_action_picker(Vec::new());
            }
            _ => self.status = String::from("No quick fixes available here"),
        }
        true
    }

    /// Present the available code actions in the shared list picker, tagging
    /// each row with its index into `pending_code_actions`. When a diagnostic
    /// sits under the caret, one more row follows the server's: Fix with
    /// Navigator (#374), at index `items.len()`, offered whether or not a
    /// navigator is seated (the action says so if none is).
    pub(super) fn open_code_action_picker(
        &mut self,
        items: Vec<crate::lsp::manager::CodeActionItem>,
    ) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let mut rows: Vec<ListRow> = items
            .iter()
            .enumerate()
            .map(|(i, item)| ListRow {
                id: i.to_string(),
                label: if item.is_preferred {
                    format!("{} (preferred)", item.title)
                } else {
                    item.title.clone()
                },
            })
            .collect();
        if self.navigator_fix_candidate().is_some() {
            rows.push(ListRow {
                id: String::from("navigator"),
                label: String::from("Fix with Navigator"),
            });
        }
        self.pending_code_actions = items;
        self.open_list_picker(
            ListPicker::new(ListPurpose::CodeAction, "Quick Fix", rows),
            "No quick fixes available here",
        );
    }

    /// Apply the chosen code action: edits first (as one undo step, like rename),
    /// then any command. A resolve-only action is sent for resolution first; its
    /// resolved edit/command arrives back through [`Self::drain_lsp_code_actions`].
    pub(super) fn apply_code_action(&mut self, item: crate::lsp::manager::CodeActionItem) {
        if item.needs_resolve {
            self.resolve_and_apply_code_action(item);
            return;
        }
        self.apply_resolved_code_action(item);
    }

    /// Apply an action that already carries its edit / command (the leaf of both
    /// the direct and the post-resolve paths). Never re-resolves, so a server
    /// that returns a still-edit-less action after resolve can't loop.
    pub(super) fn apply_resolved_code_action(&mut self, item: crate::lsp::manager::CodeActionItem) {
        let mut applied = false;
        if let Some(file) = self.stale_edit_target(&self.code_action_baseline, &item.edits) {
            self.status = format!(
                "Quick fix cancelled: {file} changed while the server was working; try it again"
            );
            return;
        }
        if !item.edits.is_empty() {
            match self.apply_rename_edits(&item.edits) {
                Ok((files, occ)) => {
                    applied = true;
                    self.status = format!(
                        "{}: changed {occ} edit(s) across {files} file(s)",
                        item.title
                    );
                }
                Err(e) => {
                    self.status = format!("Quick fix failed: {e}");
                    return;
                }
            }
        }
        if let Some(cmd) = item.command
            && let (Some(path), Some(lsp)) = (self.editor.path.clone(), self.lsp.as_mut())
        {
            lsp.execute_command(path, cmd.command, cmd.arguments);
            // A command-only action (gopls's "Browse documentation") does
            // its work in the server, and anything it has to show arrives
            // as a message; claiming a fix was applied was wrong (#1599).
            if !applied {
                self.status = format!("Ran {}", item.title);
            }
            applied = true;
        }
        if !applied {
            self.status = format!("Quick fix had no effect: {}", item.title);
        }
    }

    /// Send a resolve-only action for `codeAction/resolve`; the resolved item
    /// comes back as a single-item reply that the drain applies.
    pub(super) fn resolve_and_apply_code_action(
        &mut self,
        item: crate::lsp::manager::CodeActionItem,
    ) {
        let Some(action) = item.resolve_action else {
            self.status = format!("Quick fix cannot be resolved: {}", item.title);
            return;
        };
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_code_action_resolve(path, item.server, action);
        self.code_action_request_id = Some(id);
        self.code_action_pending_resolve = true;
        self.code_action_baseline = self.edit_seqs_in_every_group();
        self.status = String::from("Resolving quick fix");
    }

    /// Render the secondary side bar (VS Code's auxiliary bar): the active
    /// file's Outline, on the edge opposite the primary side bar. Reuses the
    /// single `OutlinePanel`, forced expanded for this dedicated panel; its
    /// collapse state is saved/restored so the Explorer's own OUTLINE section
    /// (which may render the same widget afterward) keeps its toggle.
    pub(super) fn render_secondary_side_bar(
        &mut self,
        frame: &mut ratatui::Frame,
        area: Option<Rect>,
    ) {
        let Some(area) = area.filter(|a| a.width > 0 && a.height > 0) else {
            self.secondary_outline.forget_area();
            return;
        };
        // A left border + bg fill so the panel reads as a distinct column,
        // like the primary side bar's seam against the editor.
        let block = match self.theme.surfaces() {
            Some(s) => ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::LEFT)
                .border_set(crate::widgets::surface::HAIRLINE)
                .border_style(Style::default().fg(s.hairline()))
                .style(Style::default().bg(s.chrome())),
            None => ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::LEFT)
                .border_style(Style::default().fg(self.theme.ui(Color::Rgb(0x2b, 0x2b, 0x2b))))
                .style(Style::default().bg(self.theme.editor_bg())),
        };
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.outline.theme = self.theme;
        self.outline.focus_gradient = self.theme.gradient();
        self.outline.hover_pointer = self.pointer_cell;
        // The bar never takes focus, so its Outline draws unfocused. The flag
        // is the Explorer's OUTLINE section's too, drawn after this, so it
        // goes back to what focus says (#2068).
        let explorer_focused = std::mem::replace(&mut self.outline.focused, false);
        self.with_secondary_outline(|outline| frame.render_widget(outline, inner));
        self.outline.focused = explorer_focused;
    }

    /// Run `f` on the Outline as the secondary side bar shows it: that bar's
    /// own scroll and hit rects swapped in (#1326), and never folded, since
    /// the Outline is all the bar holds.
    pub(super) fn with_secondary_outline<R>(
        &mut self,
        f: impl FnOnce(&mut crate::widgets::outline::OutlinePanel) -> R,
    ) -> R {
        let mut view = std::mem::take(&mut self.secondary_outline);
        self.outline.swap_view(&mut view);
        let collapsed = std::mem::replace(&mut self.outline.collapsed, false);
        let out = f(&mut self.outline);
        self.outline.collapsed = collapsed;
        self.outline.swap_view(&mut view);
        self.secondary_outline = view;
        out
    }

    /// A left click inside the secondary side bar's Outline: the scrollbar
    /// lane scrolls (and arms a drag), a symbol row jumps the editor there
    /// like the Explorer's OUTLINE does, and anything else is swallowed.
    pub(super) fn click_secondary_outline(&mut self, x: u16, y: u16) {
        let (on_bar, target) = self.with_secondary_outline(|o| {
            if rect_contains(o.last_scrollbar, x, y) {
                o.scroll_to_bar_y(y);
                (true, None)
            } else {
                (false, o.row_at(y).and_then(|idx| o.jump_target(idx)))
            }
        });
        self.secondary_outline_scrollbar_drag = on_bar;
        if let Some((path, line, col)) = target
            && !self.jump_in_scrub_view(line)
        {
            self.go_to_definition(path, line, col);
        }
    }

    /// Open "Go to Symbol in Editor" (Cmd/Ctrl+Shift+O). Snapshots the active
    /// file's symbols from the outline (tree-sitter is populated synchronously
    /// each tick; LSP refines it). A leading `:` in the query switches to Go to
    /// Line. ponytail: snapshot at open — symbols are present for any synced
    /// file; if a slow LSP reply ever needs to land into an open picker, feed
    /// it live from `drain_lsp_document_symbols`.
    pub(super) fn open_go_to_symbol(&mut self) {
        if self.go_to_symbol.is_some() {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Go to Symbol: no file open");
            return;
        };
        let symbols = self.outline.symbols().to_vec();
        let count = symbols.len();
        self.go_to_symbol = Some(crate::widgets::symbol_picker::SymbolPicker::new(
            path, symbols,
        ));
        self.overlays.command_palette_clear.request();
        self.status = format!("Go to Symbol: {count} symbols — Esc to close");
    }

    pub(super) fn close_go_to_symbol(&mut self) {
        if self.go_to_symbol.take().is_some() {
            self.overlays.command_palette_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    pub(super) fn handle_go_to_symbol_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.go_to_symbol.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.close_go_to_symbol(),
            (KeyCode::Enter, _) => {
                if picker.is_line_mode() {
                    // Resolve the target before closing (close drops the picker).
                    let target = picker.line_target().map(|n| (n as u32 - 1, 0));
                    let path = picker.path.clone();
                    self.close_go_to_symbol();
                    if let Some((line, col)) = target {
                        self.go_to_definition(path, line, col);
                    }
                } else {
                    self.jump_to_go_to_symbol_selection();
                }
            }
            (KeyCode::Up, _) => picker.select_prev(),
            (KeyCode::Down, _) => picker.select_next(),
            (KeyCode::PageUp, _) => {
                for _ in 0..10 {
                    picker.select_prev();
                }
            }
            (KeyCode::PageDown, _) => {
                for _ in 0..10 {
                    picker.select_next();
                }
            }
            (KeyCode::Left, _) => picker.move_cursor_left(),
            (KeyCode::Right, _) => picker.move_cursor_right(),
            (KeyCode::Home, _) => picker.move_cursor_home(),
            (KeyCode::End, _) => picker.move_cursor_end(),
            (KeyCode::Backspace, _) => picker.pop_char(),
            (KeyCode::Delete, _) => picker.delete_char(),
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                picker.push_char(c);
            }
            _ => {}
        }
    }

    /// Jump to the highlighted symbol and dismiss the picker: Enter's action
    /// outside line mode, shared with a mouse click on a result row (a click
    /// names a concrete row, so it always means the symbol, never a `:N`
    /// line query).
    pub(super) fn jump_to_go_to_symbol_selection(&mut self) {
        let Some(picker) = self.go_to_symbol.as_ref() else {
            return;
        };
        // Resolve the target before closing (close drops the picker).
        let target = picker.selected_target();
        let path = picker.path.clone();
        self.close_go_to_symbol();
        if let Some((line, col)) = target {
            self.go_to_definition(path, line, col);
        }
    }

    pub(super) fn handle_go_to_symbol_mouse(&mut self, m: MouseEvent) {
        let Some(picker) = self.go_to_symbol.as_mut() else {
            return;
        };
        let inside = rect_contains(picker.last_rect, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    picker.select_next();
                }
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    picker.select_prev();
                }
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
                if !inside =>
            {
                self.close_go_to_symbol();
            }
            // Click a row to jump to it, the way VS Code's quick-pick behaves.
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = picker.row_index_at(m.row) {
                    picker.selected = idx;
                    self.jump_to_go_to_symbol_selection();
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_go_to_symbol(&mut self, frame: &mut ratatui::Frame) {
        let theme = self.theme;
        let center = self.quick_input_position == QuickInputPosition::Center;
        let Some(picker) = self.go_to_symbol.as_mut() else {
            return;
        };
        let area = frame.area();
        crate::widgets::symbol_picker::render_symbol_picker(
            picker,
            area,
            frame.buffer_mut(),
            theme,
            center,
        );
    }

    /// Open "Go to Symbol in Workspace" seeded with `initial` (the text
    /// after Quick Open's `#`). The query goes to every running language
    /// server; results stream in via [`Self::drain_lsp_workspace_symbols`].
    pub(super) fn open_workspace_symbols(&mut self, initial: &str) {
        if self.workspace_symbols.is_some() {
            return;
        }
        self.workspace_symbols = Some(
            crate::widgets::workspace_symbols::WorkspaceSymbolPicker::new(
                self.tree.root.clone(),
                initial,
            ),
        );
        self.overlays.command_palette_clear.request();
        self.workspace_symbols_requery();
        self.status = String::from("Go to Symbol in Workspace: type to search — Esc to close");
    }

    pub(super) fn close_workspace_symbols(&mut self) {
        if self.workspace_symbols.take().is_some() {
            self.ws_symbols_request_id = None;
            self.ws_symbols_pending = None;
            self.overlays.command_palette_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    /// Note a query change and arm the debounce; the actual LSP request
    /// fires from [`Self::workspace_symbols_dispatch_due`] once the query
    /// has been still for the window. Firing per keystroke serialized a
    /// round trip per character behind the per-client lock, starving every
    /// other LSP feature on that server for the whole burst.
    pub(super) fn workspace_symbols_requery(&mut self) {
        let Some(picker) = self.workspace_symbols.as_mut() else {
            return;
        };
        let query = picker.query.trim().to_string();
        if query.is_empty() {
            picker.set_results(Vec::new());
            // A stale notice from the previous reply reads as an error;
            // an empty query should show the type-to-search hint.
            picker.unsupported = false;
            self.ws_symbols_request_id = None;
            self.ws_symbols_pending = None;
            return;
        }
        picker.loading = true;
        // Invalidate any in-flight request: its reply answers a query the
        // user has typed past and would render stale results until the
        // debounced request lands.
        self.ws_symbols_request_id = None;
        self.ws_symbols_pending = Some(std::time::Instant::now());
    }

    /// Debounced dispatch for [`Self::workspace_symbols_requery`]: called
    /// each tick from the drain loop, sends the newest query once it has
    /// been still for the window. Only the newest reply applies via
    /// `ws_symbols_request_id`.
    pub(super) fn workspace_symbols_dispatch_due(&mut self) {
        const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);
        let Some(armed) = self.ws_symbols_pending else {
            return;
        };
        if armed.elapsed() < DEBOUNCE {
            return;
        }
        self.ws_symbols_pending = None;
        let Some(picker) = self.workspace_symbols.as_mut() else {
            return;
        };
        let query = picker.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        let Some(lsp) = self.lsp.as_mut() else {
            if let Some(p) = self.workspace_symbols.as_mut() {
                p.loading = false;
                p.unsupported = true;
            }
            return;
        };
        self.ws_symbols_request_id = Some(lsp.request_workspace_symbols(query));
    }

    /// Install a `workspace/symbol` reply into the open picker. Only the
    /// newest request's reply applies; anything else is a stale answer to a
    /// query the user has already typed past. Returns true when applied.
    pub fn apply_workspace_symbols(
        &mut self,
        request_id: u64,
        symbols: Vec<crate::lsp::manager::WorkspaceSymbolItem>,
        unsupported: bool,
    ) -> bool {
        if self.ws_symbols_request_id != Some(request_id) {
            return false;
        }
        self.ws_symbols_request_id = None;
        let Some(picker) = self.workspace_symbols.as_mut() else {
            return false;
        };
        picker.unsupported = unsupported;
        picker.set_results(symbols);
        true
    }

    pub fn drain_lsp_workspace_symbols(&mut self) -> bool {
        let mut results = Vec::new();
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(r) = lsp.drain_workspace_symbols() {
                results.push(r);
            }
        }
        let mut changed = false;
        for r in results {
            changed |= self.apply_workspace_symbols(r.request_id, r.symbols, r.unsupported);
        }
        changed
    }

    pub(super) fn handle_workspace_symbols_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.workspace_symbols.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.close_workspace_symbols(),
            // Alt+Enter: go, then view that symbol in a tab of its own (#369).
            (KeyCode::Enter, m) if m.contains(KeyModifiers::ALT) => {
                let target = picker.selected_item().map(|i| i.path.clone());
                self.jump_to_workspace_symbol_selection();
                // The server's path may be spelled differently from the
                // tab's (a symlink, `..`): a landed jump is the same file.
                let landed = match (target, self.editor.path.as_ref()) {
                    (Some(t), Some(p)) => {
                        &t == p
                            || std::fs::canonicalize(&t)
                                .is_ok_and(|t| std::fs::canonicalize(p).is_ok_and(|p| p == t))
                    }
                    _ => false,
                };
                if landed {
                    self.open_symbol_tab();
                }
            }
            (KeyCode::Enter, _) => self.jump_to_workspace_symbol_selection(),
            (KeyCode::Up, _) => picker.select_prev(),
            (KeyCode::Down, _) => picker.select_next(),
            (KeyCode::PageUp, _) => {
                for _ in 0..10 {
                    picker.select_prev();
                }
            }
            (KeyCode::PageDown, _) => {
                for _ in 0..10 {
                    picker.select_next();
                }
            }
            (KeyCode::Left, _) => picker.move_cursor_left(),
            (KeyCode::Right, _) => picker.move_cursor_right(),
            (KeyCode::Home, _) => picker.move_cursor_home(),
            (KeyCode::End, _) => picker.move_cursor_end(),
            (KeyCode::Backspace, _) => {
                picker.pop_char();
                self.workspace_symbols_requery();
            }
            (KeyCode::Delete, _) => {
                picker.delete_char();
                self.workspace_symbols_requery();
            }
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                picker.push_char(c);
                self.workspace_symbols_requery();
            }
            _ => {}
        }
    }

    /// Jump to the highlighted workspace symbol and dismiss the picker:
    /// Enter's action, shared with a mouse click on a result row.
    pub(super) fn jump_to_workspace_symbol_selection(&mut self) {
        let Some(picker) = self.workspace_symbols.as_ref() else {
            return;
        };
        let target = picker
            .selected_item()
            .map(|i| (i.path.clone(), i.line, i.character));
        self.close_workspace_symbols();
        if let Some((path, line, character)) = target {
            self.go_to_definition(path, line, character);
        }
    }

    pub(super) fn handle_workspace_symbols_mouse(&mut self, m: MouseEvent) {
        let Some(picker) = self.workspace_symbols.as_mut() else {
            return;
        };
        let inside = rect_contains(picker.last_rect, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    picker.select_next();
                }
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    picker.select_prev();
                }
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
                if !inside =>
            {
                self.close_workspace_symbols();
            }
            // Click a row to jump to it, the way VS Code's quick-pick behaves.
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = picker.row_index_at(m.row) {
                    picker.selected = idx;
                    self.jump_to_workspace_symbol_selection();
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_workspace_symbols(&mut self, frame: &mut ratatui::Frame) {
        let theme = self.theme;
        let center = self.quick_input_position == QuickInputPosition::Center;
        let Some(picker) = self.workspace_symbols.as_mut() else {
            return;
        };
        let area = frame.area();
        crate::widgets::workspace_symbols::render_workspace_symbols(
            picker,
            area,
            frame.buffer_mut(),
            theme,
            center,
        );
    }

    /// Ask for the active file's code lenses once its edits have settled
    /// (#608). Lenses cost a server real work (rust-analyzer resolves each
    /// one), so this waits out [`CODE_LENS_SETTLE`] rather than riding every
    /// edit the way inlay hints do.
    pub fn tick_code_lens(&mut self) {
        if !self.code_lens_enabled {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(&seq) = self.lsp_last_seen.get(&path) else {
            return;
        };
        if self.code_lens_requested.get(&path) == Some(&seq)
            || self
                .editor
                .last_edit_at
                .is_some_and(|t| t.elapsed() < CODE_LENS_SETTLE)
        {
            return;
        }
        if let Some(lsp) = self.lsp.as_ref() {
            lsp.request_code_lens(path.clone(), seq);
            self.code_lens_requested.insert(path, seq);
        }
    }

    /// Land code-lens replies on every tab showing the file (#608). A reply
    /// for an older seq is dropped; the tab keeps its lenses, each shown only
    /// while its own line is unchanged.
    pub fn drain_lsp_code_lens(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_code_lens() {
            updates.push(u);
        }
        let mut changed = false;
        for u in updates {
            if !self.code_lens_enabled || self.lsp_last_seen.get(&u.path) != Some(&u.seq) {
                continue;
            }
            self.for_each_tab_of(&u.path, |tab| {
                tab.code_lenses = u
                    .lenses
                    .iter()
                    .filter_map(|l| {
                        let line = l.line as usize;
                        Some(crate::code_lens::EditorLens {
                            line,
                            line_text: tab.lines.get(line)?.clone(),
                            title: l.title.clone(),
                            command: l.command.clone(),
                            arguments: l.arguments.clone(),
                            server_side: l.server_side,
                        })
                    })
                    .collect();
            });
            changed = true;
        }
        changed
    }

    /// Run the active editor's code lens at `idx` (#608).
    pub(crate) fn run_code_lens(&mut self, idx: usize) {
        let Some(lens) = self.editor.code_lenses.get(idx).cloned() else {
            return;
        };
        match crate::code_lens::action_for(&lens) {
            crate::code_lens::LensAction::ShowLocations(targets) => {
                if targets.len() == 1 {
                    let (path, line, col) = targets.into_iter().next().expect("len 1");
                    self.go_to_definition(path, line, col);
                } else {
                    self.open_location_picker(targets, "references");
                }
            }
            crate::code_lens::LensAction::RunInTerminal { label, command } => {
                self.start_project_task(crate::tasks::Task {
                    label,
                    command,
                    source: String::from("code lens"),
                    is_build: false,
                    is_default: false,
                    problem_matcher: None,
                    is_background: false,
                    vscode: None,
                });
            }
            crate::code_lens::LensAction::DebugAt(line) => {
                self.editor.cursor_row = line.min(self.editor.lines.len().saturating_sub(1));
                self.editor.cursor_col = 0;
                self.debug_test_at_cursor();
            }
            crate::code_lens::LensAction::ServerCommand { command, arguments } => {
                if let (Some(lsp), Some(path)) = (self.lsp.as_mut(), self.editor.path.clone()) {
                    lsp.execute_command(path, command, arguments);
                    self.status = format!("Ran \u{201c}{}\u{201d}", lens.title);
                }
            }
            crate::code_lens::LensAction::Unsupported(why) => {
                self.status = format!("Code lens: {why}");
            }
        }
    }

    /// The caret line's code lenses from the keyboard (Cmd+K Shift+E): one
    /// runs at once, several open a menu to pick from (#608).
    pub(super) fn run_code_lens_at_cursor(&mut self) {
        let lenses = self.editor.lenses_on_line(self.editor.cursor_row);
        match lenses.as_slice() {
            [] => self.status = String::from("No code lens on this line"),
            [one] => self.run_code_lens(*one),
            many => {
                let items: Vec<(String, MenuAction)> = many
                    .iter()
                    .map(|&i| {
                        (
                            self.editor.code_lenses[i].title.clone(),
                            MenuAction::RunCodeLens(i),
                        )
                    })
                    .collect();
                let origin = self.editor.cursor_screen_pos().unwrap_or((
                    self.editor.last_full_area.x + 1,
                    self.editor.last_full_area.y + 1,
                ));
                let root = self.tree.root.clone();
                self.context_menu = Some(ContextMenu::flat(origin, items, root));
            }
        }
    }

    /// Editor: Toggle CodeLens (#608).
    pub(super) fn toggle_code_lens(&mut self) {
        self.code_lens_enabled = !self.code_lens_enabled;
        self.code_lens_requested.clear();
        if !self.code_lens_enabled {
            for tab in self.editor.editors.iter_mut() {
                tab.code_lenses.clear();
            }
            for group in self.editor_layout.inactive_groups_mut() {
                for tab in group.editors.iter_mut() {
                    tab.code_lenses.clear();
                }
            }
        }
        self.status = format!(
            "CodeLens: {}",
            if self.code_lens_enabled { "on" } else { "off" }
        );
    }
}

/// The bundled shell language server, the one that needs shfmt and
/// shellcheck (#1511).
pub(super) const BASH_SERVER: &str = "bash-language-server";

/// What Format Document says for a shell script when shfmt is missing (#1511).
pub(super) const SHFMT_MISSING: &str =
    "Not formatted: bash-language-server formats shell scripts with shfmt, which is not on PATH";

/// The one-time notice when a shell script opens and shellcheck is missing
/// (#1511).
pub(super) const SHELLCHECK_MISSING: &str = "Shell scripts are not linted: bash-language-server lints with shellcheck, which is not on PATH";

/// What the restart commands say while an update build has the language
/// servers stopped (#1621): the build's end starts them again.
pub(super) const LSP_PAUSED_FOR_BUILD: &str =
    "Language servers are stopped while croft builds its update; they restart when it finishes";

/// The outline of `lines` as the file at `path` from its own syntax tree,
/// empty for a language croft has no grammar for.
pub(super) fn syntax_outline_for(
    path: &std::path::Path,
    lines: &[String],
) -> Vec<crate::lsp::manager::OutlineSymbol> {
    crate::outline_syntax::symbols_for_lines(path, lines)
}

/// The largest buffer whose syntax outline is parsed on the UI thread
/// (#1150). A from-scratch parse costs about a quarter of a millisecond per
/// kilobyte, so this keeps it within a frame; larger buffers go to
/// [`App::start_outline_parse`].
pub(super) const OUTLINE_INLINE_MAX_BYTES: usize = 64 * 1024;

/// Whether `lines` is small enough to parse for the outline inline.
pub(super) fn outline_parses_inline(lines: &[String]) -> bool {
    lines
        .iter()
        .try_fold(0usize, |bytes, l| {
            let bytes = bytes + l.len() + 1;
            (bytes <= OUTLINE_INLINE_MAX_BYTES).then_some(bytes)
        })
        .is_some()
}

/// Characters that open or advance a call, triggering parameter hints.
pub(super) fn is_signature_help_trigger(c: char) -> bool {
    c == '(' || c == ','
}

/// Build a completion-popup item for a user snippet: the row shows the prefix,
/// filters on it, and carries the snippet body as `is_snippet` insertion text
/// the accept path expands through the editor's tab-stop engine.
pub(super) fn snippet_completion_item(
    snip: &crate::snippets::Snippet,
) -> crate::lsp::CompletionItem {
    crate::lsp::CompletionItem {
        label: snip.prefix.clone(),
        detail: Some(String::from("snippet")),
        insert_text: Some(snip.body.clone()),
        filter_text: Some(snip.prefix.clone()),
        kind: Some(lsp_types::CompletionItemKind::SNIPPET),
        is_snippet: true,
        ..Default::default()
    }
}

/// Divider drawn between the diagnostic block and the language hover content,
/// mirroring the horizontal rule VS Code renders between hover sections.
pub(super) const HOVER_DIVIDER: &str = "────────";

/// What Show or Focus Hover says when the caret has nothing to describe
/// (#1518): a keyboard command that does nothing must say why.
pub(super) const NO_HOVER_AT_CARET: &str = "No hover information at the caret";

/// Render the diagnostics under the hovered point into a single labelled text
/// block, errors first (so the most severe message leads), each line prefixed
/// with its severity. `None` when there are no diagnostics. The severity word
/// answers the user's "what type of error is it" directly, since a TUI can't
/// show VS Code's coloured marker icon.
pub(super) fn format_diagnostics(
    diags: &[(crate::lsp::manager::DiagnosticSeverity, String)],
) -> Option<String> {
    use crate::lsp::manager::DiagnosticSeverity;
    if diags.is_empty() {
        return None;
    }
    let rank = |s: &DiagnosticSeverity| match s {
        DiagnosticSeverity::Error => 0,
        DiagnosticSeverity::Warning => 1,
        DiagnosticSeverity::Information => 2,
        DiagnosticSeverity::Hint => 3,
    };
    let label = |s: &DiagnosticSeverity| match s {
        DiagnosticSeverity::Error => "Error",
        DiagnosticSeverity::Warning => "Warning",
        DiagnosticSeverity::Information => "Info",
        DiagnosticSeverity::Hint => "Hint",
    };
    let mut ordered: Vec<&(DiagnosticSeverity, String)> = diags.iter().collect();
    ordered.sort_by_key(|(s, _)| rank(s));
    Some(
        ordered
            .iter()
            .map(|(s, m)| format!("{}: {}", label(s), m))
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

/// Stack the diagnostic block above the language hover content, divided by a
/// rule, matching VS Code (PR microsoft/vscode#166560 put marker hovers on
/// top). Either side may be absent; `None` when both are.
pub(super) fn compose_hover(diagnostic: Option<&str>, hover: Option<&str>) -> Option<String> {
    match (diagnostic, hover) {
        (Some(d), Some(h)) => Some(format!("{d}\n{HOVER_DIVIDER}\n{h}")),
        (Some(d), None) => Some(d.to_string()),
        (None, Some(h)) => Some(h.to_string()),
        (None, None) => None,
    }
}

/// The tree-sitter grammar for `path`, by extension: what symbol tabs
/// (#369) parse to re-find a renamed or displaced symbol.
pub(super) fn syntax_kind_of(path: &Path) -> Option<crate::highlight::LangKind> {
    crate::highlight::lang_for_path(path)
}
