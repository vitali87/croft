//! The OUTLINE: keeping it in step with the active editor, the syntax-tree outline (parsed
//! off the UI thread for a large buffer) until the server's `documentSymbol` answer lands,
//! the secondary side bar that shows it, and the grammar symbol tabs parse with.

use super::*;

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
    pub(in crate::app) fn start_outline_parse(&mut self) {
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
    pub(in crate::app) fn drain_outline_parse(&mut self) -> bool {
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
    pub(in crate::app) fn apply_outline_symbols(
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
    pub(in crate::app) fn compute_syntax_outline(
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
}

impl App {
    /// Render the secondary side bar (VS Code's auxiliary bar): the active
    /// file's Outline, on the edge opposite the primary side bar. Reuses the
    /// single `OutlinePanel`, forced expanded for this dedicated panel; its
    /// collapse state is saved/restored so the Explorer's own OUTLINE section
    /// (which may render the same widget afterward) keeps its toggle.
    pub(in crate::app) fn render_secondary_side_bar(
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
    pub(in crate::app) fn with_secondary_outline<R>(
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
    pub(in crate::app) fn click_secondary_outline(&mut self, x: u16, y: u16) {
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
}

/// The outline of `lines` as the file at `path` from its own syntax tree,
/// empty for a language croft has no grammar for.
pub(in crate::app) fn syntax_outline_for(
    path: &std::path::Path,
    lines: &[String],
) -> Vec<crate::lsp::manager::OutlineSymbol> {
    crate::outline_syntax::symbols_for_lines(path, lines)
}

/// Whether `lines` is small enough to parse for the outline inline.
pub(in crate::app) fn outline_parses_inline(lines: &[String]) -> bool {
    lines
        .iter()
        .try_fold(0usize, |bytes, l| {
            let bytes = bytes + l.len() + 1;
            (bytes <= OUTLINE_INLINE_MAX_BYTES).then_some(bytes)
        })
        .is_some()
}

/// The tree-sitter grammar for `path`, by extension: what symbol tabs
/// (#369) parse to re-find a renamed or displaced symbol.
pub(in crate::app) fn syntax_kind_of(path: &Path) -> Option<crate::highlight::LangKind> {
    crate::highlight::lang_for_path(path)
}
