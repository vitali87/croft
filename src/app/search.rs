//! The Search view and Replace All, Search Editor tabs, the find/replace bar (also over
//! diffs and logs), Quick Open, and Switch Header/Source.

use super::*;

/// A Search Editor tab's label (#615): not a file on disk, so it names the
/// query rather than a path the save path could clobber.
pub(super) fn search_editor_label(query: &str) -> PathBuf {
    let q: String = query.chars().take(40).collect();
    PathBuf::from(if q.is_empty() {
        String::from("Search Editor")
    } else {
        format!("Search: {q}")
    })
}

/// Whether a tab is a Search Editor by its label, not just by its text: a
/// real file that happens to open with `# Query: ` must keep its Enter key,
/// and must never have a rerun overwrite it.
pub(super) fn is_search_editor_tab(path: Option<&Path>) -> bool {
    path.is_some_and(|p| {
        let p = p.to_string_lossy();
        p == "Search Editor" || p.starts_with("Search: ")
    })
}

/// A query as the status line quotes it, shortened.
pub(super) fn header_query_for_status(query: &str) -> String {
    let q: String = query.chars().take(40).collect();
    format!("\u{201c}{q}\u{201d}")
}

impl App {
    /// Push the current search query string and toggle state onto the
    /// worker channel and keep the editor's match highlight synced to the
    /// same term so the active file lights up matches as the user types.
    /// Called whenever the search input or one of the toggles changes.
    /// Clears the live result list so streamed `Hits` events accumulate
    /// from zero rather than appending to leftover hits from the prior query.
    pub(super) fn submit_search_query(&mut self) {
        self.search.complete = false;
        self.search.hits.clear();
        self.search.selected = 0;
        self.search.scroll = 0;
        // Snapshot unsaved editor buffers so the worker searches their live
        // content (and skips the stale disk copy). This is what makes an
        // unsaved edit — e.g. a Rename Symbol — findable before save, matching
        // VS Code / Zed. Sent before the query so the next scan sees it.
        let dirty: Vec<(PathBuf, String)> = self
            .editor
            .iter_tabs()
            .filter(|t| t.dirty)
            .filter_map(|t| t.path.clone().map(|p| (p, t.lines.join("\n"))))
            .collect();
        let _ = self
            .search_query_tx
            .send(SearchRequest::SetDirtyBuffers(dirty));
        // Push the current files-to-include / files-to-exclude globs so the
        // next scan honours them (VS Code's Search filter inputs).
        let _ = self.search_query_tx.send(SearchRequest::SetFilter(
            self.search.include.clone(),
            self.search.effective_exclude(),
        ));
        let _ = self.search_query_tx.send(SearchRequest::Query(
            self.search.query.clone(),
            self.search.opts,
        ));
        let term = if crate::widgets::search::as_typed(&self.search.query).is_empty() {
            None
        } else {
            Some(self.search.query.clone())
        };
        self.editor.set_search_highlight(term, self.search.opts);
    }

    /// Apply any pending search events from the background worker. Drops
    /// stale events whose query no longer matches the input field (the
    /// user has typed past it). `Hits` events append to the live result
    /// list as files yield matches; `Done` is informational. Returns true
    /// iff hits were updated, so the main loop knows to redraw.
    pub fn drain_search_results(&mut self) -> bool {
        use crate::widgets::search::SearchEvent;
        let mut applied = false;
        // Every batch waiting is merged in one pass (#1340).
        let mut arrived = Vec::new();
        while let Ok(event) = self.search_results_rx.try_recv() {
            match event {
                SearchEvent::Hits(q, opts, batch) => {
                    if q == self.search.query && opts.same_matching(&self.search.opts) {
                        arrived.extend(batch);
                        applied = true;
                    }
                }
                SearchEvent::Done(q, opts) => {
                    if q == self.search.query && opts.same_matching(&self.search.opts) {
                        self.search.complete = true;
                        applied = true;
                    }
                }
            }
        }
        self.search.add_hits(arrived);
        applied
    }

    /// The Replace All confirmation modal (#123): counts up front, commits
    /// on Enter/Y, cancels on Esc/N — the discard-all popup's shape.
    pub(super) fn render_replace_all_confirm(&self, frame: &mut ratatui::Frame) {
        let Some((occurrences, files, skipped)) = self.pending_replace_all else {
            return;
        };
        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(50, 96).min(area.width);
        let prompt = format!(
            "Replace {occurrences} occurrence(s) across {files} file(s) with \"{}\"?",
            self.search.replace
        );
        // The prompt interpolates arbitrary user text: derive the height
        // from its wrapped row count so the Enter/Esc hint can never fall
        // below the border (#124 review).
        let inner_w = width.saturating_sub(2);
        let prompt_rows = wrap_cells_variable_width(&prompt, inner_w, inner_w).len() as u16;
        let skipped_rows = u16::from(skipped > 0);
        let height: u16 = 6 + prompt_rows + skipped_rows;
        let rect = Rect {
            x: (area.width.saturating_sub(width)) / 2 + area.x,
            y: (area.height.saturating_sub(height)) / 2 + area.y,
            width,
            height,
        };
        let rect = rect.intersection(area);
        let warn = self.theme.ui(Color::Rgb(0xe7, 0xa7, 0x3c));
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(warn))
            .style(Style::default().bg(self.theme.ui(Color::Rgb(0x1e, 0x1e, 0x1e))))
            .title(ratatui::text::Span::styled(
                " REPLACE ALL? ",
                Style::default()
                    .fg(Color::Black)
                    .bg(warn)
                    .add_modifier(Modifier::BOLD),
            ));
        frame.render_widget(ratatui::widgets::Clear, rect);
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let mut lines = vec![
            Line::from(prompt),
            Line::from(""),
            Line::from("Files are rewritten on disk."),
        ];
        if skipped > 0 {
            lines.push(Line::from(format!(
                "{skipped} open file(s) with unsaved changes will be skipped."
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Enter / Y replaces \u{00b7} Esc cancels",
            Style::default().fg(self.theme.ui(Color::Rgb(0x9a, 0xa4, 0xb8))),
        )));
        frame.render_widget(
            Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
            inner,
        );
        crate::widgets::approval_popup::paint_warning_card(
            frame.buffer_mut(),
            rect,
            self.theme,
            warn,
        );
    }

    pub(super) fn handle_search_key(&mut self, key: KeyEvent) {
        use crate::widgets::search::SearchField;
        // Reset the blink so the caret is solidly visible right after any key,
        // matching the commit box; the panel is focused whenever this fires.
        self.poke_cursor();
        if is_search_paste_key(key) {
            let text = self.clipboard_text();
            self.paste_clipboard_into_search(text.as_deref());
            return;
        }
        if is_editor_select_all_key(key) {
            self.search.select_all_active();
            return;
        }
        if is_editor_copy_key(key) {
            let text = self.search.active_selection_text();
            if !text.is_empty() {
                self.deliver_copied_text(&text);
            }
            return;
        }
        if is_editor_cut_key(key) {
            let text = self.search.active_selection_text();
            if !text.is_empty() {
                let n = text.chars().count();
                let landing = self.copy_where_pasted(&text);
                self.search.backspace();
                self.refresh_search_if_filtering();
                self.status = match landing {
                    CopyLanding::Clipboard => format!("Cut {n} chars to clipboard"),
                    _ => format!("Cut {n} chars{}", landing.note()),
                };
            }
            return;
        }
        // VS Code's Alt+C / Alt+W / Alt+R flip the Aa / ab / .* toggles from
        // any Search input, and Alt+D opens or closes the include / exclude
        // rows (#860).
        if let Some(toggle) = search_toggle_for_key(key) {
            self.toggle_search_opt(toggle);
            return;
        }
        if is_search_details_key(key) {
            self.search.toggle_details();
            return;
        }
        // Tab cycles through the visible inputs (Query → Replace → include →
        // exclude → Query), mirroring VS Code. Shift+Tab is left to fall
        // through to default since BackTab arrives as its own key code.
        if key.code == KeyCode::Tab && key.modifiers.is_empty() {
            self.search.focus_next_field();
            return;
        }
        match key.code {
            KeyCode::Esc => {
                if self.search.active_text().is_empty() {
                    if self.search.field == SearchField::Query {
                        self.set_sidebar_view(SidebarView::Explorer);
                    } else {
                        self.search.focus_field(SearchField::Query);
                    }
                } else {
                    let was_query = self.search.field == SearchField::Query;
                    self.search.active_text_mut_clear();
                    self.search.clear_active_selection();
                    if was_query {
                        self.search.hits.clear();
                    }
                    self.refresh_search_if_filtering();
                }
            }
            KeyCode::Enter => {
                // On the Replace field, Enter runs Replace All across the
                // current results; elsewhere it opens the selected hit.
                if self.search.field == SearchField::Replace {
                    self.run_search_replace_all();
                } else if let Some(hit) = self.search.selected_hit().cloned() {
                    self.open_search_hit(&hit);
                }
            }
            // The caret keys (#1729), as in every input row: Left/Right/
            // Home/End move inside the focused field, Delete removes ahead,
            // and Ctrl/Cmd/Alt makes Left/Right/Backspace/Delete walk by a word.
            KeyCode::Left => self.search.move_caret(|t, c| {
                crate::widgets::line_edit::caret_left(t, c, key_walks_by_word(key))
            }),
            KeyCode::Right => self.search.move_caret(|t, c| {
                crate::widgets::line_edit::caret_right(t, c, key_walks_by_word(key))
            }),
            KeyCode::Home => self.search.move_caret(|_, _| 0),
            KeyCode::End => self
                .search
                .move_caret(|t, _| crate::widgets::line_edit::char_len(t)),
            KeyCode::Delete if self.search.delete_forward(key_walks_by_word(key)) => {
                self.refresh_search_if_filtering();
            }
            KeyCode::Backspace if self.search.backspace_by(key_walks_by_word(key)) => {
                self.refresh_search_if_filtering();
            }
            KeyCode::Up => self.search.move_up(),
            KeyCode::Down => self.search.move_down(),
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER)
                {
                    self.search.type_char(c);
                    self.refresh_search_if_filtering();
                } else if key.modifiers.contains(KeyModifiers::SUPER) {
                    self.status = format!("Search: unhandled Cmd+{c}");
                } else if key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.status = format!("Search: unhandled Ctrl+{c}");
                }
            }
            _ => {}
        }
    }

    /// Flip one of the Search side bar's mode toggles, from a click or a key,
    /// and re-run the query under it. The status line names the new state:
    /// the toggle's glyph is two cells in a panel that may be off-screen.
    pub(super) fn toggle_search_opt(&mut self, toggle: crate::widgets::search::SearchToggle) {
        use crate::widgets::search::SearchToggle;
        let on = self.search.toggle_opt(toggle);
        let name = match toggle {
            SearchToggle::CaseSensitive => "Match Case",
            SearchToggle::WholeWord => "Match Whole Word",
            SearchToggle::UseRegex => "Use Regular Expression",
            SearchToggle::PreserveCase => "Preserve Case",
        };
        self.status = format!("Search: {name} {}", if on { "on" } else { "off" });
        // Preserve Case shapes replacements only: the hits stay as they are.
        if toggle != SearchToggle::PreserveCase {
            self.submit_search_query();
        }
    }

    /// Search: Replace in Files (#860, `Cmd+Shift+H`): the Search side bar
    /// with its Replace row expanded and focused, the way the chevron opens
    /// it.
    pub(super) fn open_replace_in_files(&mut self) {
        // As for the Search jump: a live find bar would keep painting over
        // the editor and shadow the side bar's input.
        self.close_editor_find();
        self.set_sidebar_view(SidebarView::Search);
        self.search.open_replace();
    }

    /// Re-run the search after a field edit, but only when the edited field
    /// actually affects the result set. Editing the Replace text changes
    /// nothing about which files match, so it must not clear / re-scan.
    pub(super) fn refresh_search_if_filtering(&mut self) {
        if self.search.field != crate::widgets::search::SearchField::Replace {
            self.submit_search_query();
        }
    }

    /// Replace every match in the current results with the Replace text, then
    /// re-run the query so the (now stale) hit list refreshes.
    pub(super) fn run_search_replace_all(&mut self) {
        if self.is_collab_guest() {
            // Replace All writes hit files on disk directly; in a collab
            // session only the owner writes (their croft offers the same
            // action, and open shared files converge through the reload
            // diff).
            self.status = String::from("Replace All is owner-only in a shared session");
            return;
        }
        if crate::widgets::search::as_typed(&self.search.query).is_empty() {
            self.status = String::from("Replace All: enter a search term first");
            return;
        }
        if !self.search.complete {
            // Hits stream in batches; replacing before Done would silently
            // miss files found after confirmation (#124 review).
            self.status =
                String::from("Replace All: search still running — retry when it finishes");
            return;
        }
        // Count first, write only after the modal confirms: a workspace-wide
        // disk rewrite is exactly what the confirm-destructive rule is for
        // (#123 — Enter used to commit instantly). Dirty-open files are
        // excluded from the count too: the confirmed action skips them, and
        // counting them made the modal overpromise.
        let dirty = self.dirty_open_paths();
        let (occurrences, files) = self.search.count_replacements(&dirty);
        let skipped = self
            .search
            .hits
            .iter()
            .map(|h| &h.path)
            .filter(|p| dirty.contains(*p))
            .collect::<std::collections::HashSet<_>>()
            .len();
        if occurrences == 0 && skipped == 0 {
            self.status = String::from("Replace All: nothing to replace");
            return;
        }
        self.pending_replace_all = Some((occurrences, files, skipped));
    }

    /// Every path open with unsaved edits, across the active group and all
    /// inactive split leaves.
    pub(super) fn dirty_open_paths(&self) -> std::collections::HashSet<PathBuf> {
        self.editor
            .editors
            .iter()
            .chain(
                self.editor_layout
                    .inactive_leaf_tabs()
                    .into_iter()
                    .flat_map(|tabs| tabs.editors.iter()),
            )
            .filter(|e| e.dirty)
            .filter_map(|e| e.path.clone())
            .collect()
    }

    /// The confirmed Replace All: writes every hit file EXCEPT ones open
    /// with unsaved edits — a disk write under a dirty buffer forces that
    /// tab into conflict divergence, so those are skipped and named.
    pub(super) fn confirm_pending_replace_all(&mut self) {
        self.pending_replace_all = None;
        let dirty = self.dirty_open_paths();
        let (n, skipped, failed) = self.search.replace_all_skipping(&dirty);
        let list = |paths: &[PathBuf]| {
            let names: Vec<String> = paths.iter().take(2).map(|p| self.status_path(p)).collect();
            let more = paths.len().saturating_sub(names.len());
            if more > 0 {
                format!("{} and {more} more", names.join(", "))
            } else {
                names.join(", ")
            }
        };
        let mut status = format!("Replace All: replaced {n} occurrence(s)");
        if !skipped.is_empty() {
            status.push_str(&format!(
                "; skipped {} (unsaved changes — save first)",
                list(&skipped)
            ));
        }
        if !failed.is_empty() {
            status.push_str(&format!(
                "; could not rewrite {} (not UTF-8, or unwritable)",
                list(&failed)
            ));
        }
        self.status = status;
        self.submit_search_query();
    }

    pub(super) fn cancel_pending_replace_all(&mut self) {
        self.pending_replace_all = None;
        self.status = String::from("Replace All cancelled");
    }

    pub(super) fn open_search_hit(&mut self, hit: &crate::widgets::search::SearchHit) {
        match self.editor.open_preview(&hit.path) {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                let row = hit
                    .line_no
                    .saturating_sub(1)
                    .min(self.editor.lines.len().saturating_sub(1));
                // Drop the cursor at the first match column on this line so
                // long lines (HTML with base64 blobs, minified JS) auto-scroll
                // horizontally and the user lands on the match instead of the
                // line's leftmost cell - which on a 200k-char line means
                // never seeing the match without manual scrolling.
                let needle = crate::widgets::search::as_typed(&self.search.query);
                let opts = self.search.opts;
                let line = self.editor.lines.get(row).cloned().unwrap_or_default();
                // Locate the first match on the line: its start column and
                // length (in chars). The length lets us mark this occurrence
                // as the *active* match so the renderer paints it the stronger
                // orange while sibling matches stay yellow - mirroring the
                // Cmd+F path's `active_search_match` behaviour and VS Code's
                // "current match" highlight.
                let first_match = if needle.is_empty() {
                    None
                } else {
                    let mut col = 0usize;
                    let mut found = None;
                    for (chunk, is_match) in
                        crate::widgets::search::split_for_highlight(&line, needle, opts)
                    {
                        if is_match {
                            found = Some((col, chunk.chars().count()));
                            break;
                        }
                        col = col.saturating_add(chunk.chars().count());
                    }
                    found
                };
                let match_col = first_match.map(|(c, _)| c).unwrap_or(0);
                self.editor.active_search_match = first_match.map(|(c, len)| (row, c, len));
                self.editor.cursor_col = match_col;
                // Centre the hit's line, as Go to Line does, instead of
                // leaving it wherever the preview's scroll happened to be.
                self.editor.goto_line_centered(row);
                self.editor.ensure_cursor_col_visible();
                self.status = format!(
                    "Opened {} at line {}",
                    self.status_path(&hit.path),
                    hit.line_no
                );
                self.focus_pane(Pane::Editor);
            }
            Err(e) => {
                self.status = format!("Open failed: {e}");
            }
        }
    }

    /// Seed the Search sidebar from the last `grep`/`rg` command run in the
    /// focused terminal, then run it. The Search panel already has multi-file
    /// replace-all, so this turns a terminal search into an editable,
    /// replace-across-every-match buffer (the `:cdo` loop) with one command.
    pub(super) fn search_from_last_terminal_command(&mut self) {
        let input = {
            let term = self.terminal();
            let decs = term.command_decorations();
            match decs.last() {
                Some(d) => term.command_input_text(d),
                None => {
                    self.status = String::from("No terminal command yet to search from");
                    return;
                }
            }
        };
        if !self.seed_search_from_command(input.trim()) {
            self.status =
                String::from("Last terminal command wasn't a grep/rg search — nothing to seed");
        }
    }

    /// Populate and run the Search panel from a parsed terminal search
    /// command line, scoped to the paths it searched from the focused pane's
    /// directory. Returns false when `input` isn't a recognisable search.
    pub(super) fn seed_search_from_command(&mut self, input: &str) -> bool {
        let cwd = self
            .terminals
            .get(self.active_terminal)
            .and_then(|t| t.pid())
            .and_then(cwd_of_pid)
            .filter(|p| p.is_dir());
        let Some(cwd) = cwd else {
            // Guessing the workspace root would widen `rg foo` run from a
            // subdirectory to every file (#1201).
            if crate::quickfix::parse_search_command(input).is_none() {
                return false;
            }
            self.status = String::from(
                "Search not seeded: can't tell which directory the terminal searched from",
            );
            return true;
        };
        self.seed_search_from_command_in(input, &cwd)
    }

    /// [`Self::seed_search_from_command`] for a command run in `cwd`. A
    /// search whose scope can't be reproduced exactly is refused with the
    /// reason in the status bar and leaves the panel alone (still true:
    /// the input was a search), so Replace All never widens past it (#1201).
    pub(super) fn seed_search_from_command_in(&mut self, input: &str, cwd: &Path) -> bool {
        let Some(sc) = crate::quickfix::parse_search_command(input) else {
            return false;
        };
        let (include, exclude) =
            match crate::quickfix::scope_filters(&sc, cwd, self.workspace_root()) {
                Ok(filters) => filters,
                Err(why) => {
                    self.status = format!("Search not seeded: {why}");
                    return true;
                }
            };
        self.search.seed(sc.pattern.clone(), include, exclude);
        self.search.opts.case_sensitive = sc.case_sensitive;
        self.search.opts.whole_word = sc.whole_word;
        self.search.opts.use_regex = sc.use_regex;
        self.set_sidebar_view(SidebarView::Search);
        self.submit_search_query();
        self.status = format!("Search seeded from terminal: {}", sc.pattern);
        true
    }

    pub fn consume_file_finder_image_clear(&mut self) -> bool {
        self.overlays.file_finder_clear.consume()
    }

    /// Drain the background-build receiver into `file_finder_index`
    /// without blocking. Called from `open_file_finder` and from the
    /// main loop tick. Accepts REPLACEMENT results too: when the FS
    /// watcher or a workspace re-root has triggered a rebuild, the
    /// fresh index swaps in here and is pushed into the open finder
    /// (if any) so an in-progress Cmd+P session immediately re-ranks
    /// against the new file set. If a follow-up rebuild was queued
    /// while this one was in flight (the `file_finder_index_dirty`
    /// flag), it is re-kicked here so a long-running build on a
    /// multi-GB monorepo can never miss a later FS event.
    pub(super) fn poll_file_finder_index(&mut self) {
        let Some(rx) = self.file_finder_index_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(entries) => {
                let arc = std::sync::Arc::new(entries);
                self.file_finder_index = Some(arc.clone());
                self.file_finder_index_rx = None;
                let visible = self.without_project_excludes(arc);
                if let Some(finder) = self.file_finder.as_mut() {
                    finder.replace_entries(visible);
                }
                if self.file_finder_index_dirty {
                    self.kick_file_finder_index_rebuild();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.file_finder_index_rx = None;
            }
        }
    }

    /// Spawn a background walker that rebuilds the Cmd+P file index
    /// against the current `workspace_root`. If a rebuild is already
    /// in flight, sets the dirty flag instead; `poll_file_finder_index`
    /// will re-kick on completion. Called from `change_workspace_root`
    /// so Cmd+P reflects the new project, and from `drain_fs_events`
    /// so creates, deletes, and git-checkout-driven swaps land in the
    /// index without a croft restart.
    pub(super) fn kick_file_finder_index_rebuild(&mut self) {
        if self.file_finder_index_rx.is_some() {
            self.file_finder_index_dirty = true;
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel::<Vec<crate::widgets::file_finder::FileEntry>>();
        let roots: Vec<PathBuf> = self.roots.iter().map(Path::to_path_buf).collect();
        std::thread::spawn(move || {
            let entries = crate::widgets::file_finder::build_multi_file_index(&roots);
            let _ = tx.send(entries);
        });
        self.file_finder_index_rx = Some(rx);
        self.file_finder_index_dirty = false;
    }

    /// C/C++: Switch Header/Source (#1360): open the header of the source
    /// in the editor, or the source of the header, found beside it, across
    /// an `include/` ↔ `src/` split, or anywhere in the workspace.
    pub(super) fn switch_header_source(&mut self) {
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("No file in the active tab");
            return;
        };
        if !crate::header_source::is_c_family(&path) {
            self.status =
                String::from("Switch Header/Source works on C, C++ and Objective-C files");
            return;
        }
        self.poll_file_finder_index();
        // The root that owns the file, so a file in a secondary root of a
        // multi-root workspace finds its counterpart in that root.
        let root = self
            .roots
            .owning_root(&path)
            .unwrap_or_else(|| self.roots.primary())
            .to_path_buf();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let index_paths = |app: &Self| -> Vec<PathBuf> {
            app.file_finder_index
                .as_ref()
                .map(|entries| entries.iter().map(|e| e.path.clone()).collect())
                .unwrap_or_default()
        };
        let is_file = |p: &Path| p.is_file();
        let mut found =
            crate::header_source::counterpart(&path, &root, &index_paths(self), &is_file);
        // The background index has not landed yet: walk the workspace
        // synchronously this once, as Go to File does, so the
        // anywhere-in-the-workspace fallback is not answered from nothing.
        if found.is_none() && self.file_finder_index.is_none() {
            let roots: Vec<PathBuf> = self.roots.iter().map(Path::to_path_buf).collect();
            let entries = crate::widgets::file_finder::build_multi_file_index(&roots);
            self.file_finder_index = Some(std::sync::Arc::new(entries));
            self.file_finder_index_rx = None;
            found = crate::header_source::counterpart(&path, &root, &index_paths(self), &is_file);
        }
        let Some(found) = found else {
            self.status = format!("No header/source counterpart for {name}");
            return;
        };
        // A tab-aware open: the counterpart gets its own tab (or its
        // existing one), so unsaved edits in the current buffer survive.
        match self.editor.open_pinned(&found) {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                self.focus_pane(Pane::Editor);
                let shown = found.strip_prefix(&root).unwrap_or(&found);
                self.status = format!("{name} \u{2194} {}", shown.display());
            }
            Err(e) => self.status = format!("Could not open {}: {e}", found.display()),
        }
    }

    pub(super) fn open_file_finder(&mut self) {
        if self.file_finder.is_some() {
            return;
        }
        // Drain the background-built index if it has landed.
        self.poll_file_finder_index();
        // Fallback: the worker thread has not finished yet AND no
        // index has ever been cached. Walk synchronously this once
        // rather than open an empty finder; this only fires when
        // Cmd+P is pressed within the first few hundred ms of launch.
        if self.file_finder_index.is_none() {
            let roots: Vec<PathBuf> = self.roots.iter().map(Path::to_path_buf).collect();
            let entries = crate::widgets::file_finder::build_multi_file_index(&roots);
            self.file_finder_index = Some(std::sync::Arc::new(entries));
            self.file_finder_index_rx = None;
        }
        let entries = self
            .file_finder_index
            .clone()
            .unwrap_or_else(|| std::sync::Arc::new(Vec::new()));
        let entries = self.without_project_excludes(entries);
        let count = entries.len();
        self.file_finder = Some(crate::widgets::file_finder::FileFinder::new(entries));
        self.overlays.file_finder_clear.request();
        self.status = format!("Go to File: {count} files indexed — Esc to close");
    }

    pub(super) fn close_file_finder(&mut self) {
        if self.file_finder.take().is_some() {
            self.overlays.file_finder_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    pub(super) fn handle_file_finder_key(&mut self, key: KeyEvent) {
        // Cmd+V / Ctrl+V: paste the system clipboard into the query as if
        // each character had been typed. Hosts that route Cmd+V through
        // bracketed paste (iTerm2 default) deliver an `Event::Paste(s)`
        // instead, which `handle_paste` forwards to the finder — but if
        // the user re-bound Cmd+V to a CSI-u Cmd+V sequence or is running
        // in a terminal that disables bracketed paste, the chord arrives
        // here as a key event, and reading the clipboard ourselves keeps
        // the "open finder, paste a filename" gesture working everywhere.
        if is_clipboard_paste_key(key) {
            if let Some(text) = self.clipboard_text()
                && let Some(finder) = self.file_finder.as_mut()
            {
                for c in text.chars() {
                    if !c.is_control() {
                        finder.push_char(c);
                    }
                }
            }
            self.follow_quick_open_prefix();
            return;
        }
        let Some(finder) = self.file_finder.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                self.close_file_finder();
            }
            (KeyCode::Enter, _) => self.open_file_finder_selection(),
            (KeyCode::Up, _) => finder.select_prev(),
            (KeyCode::Down, _) => finder.select_next(),
            (KeyCode::PageUp, _) => {
                for _ in 0..10 {
                    finder.select_prev();
                }
            }
            (KeyCode::PageDown, _) => {
                for _ in 0..10 {
                    finder.select_next();
                }
            }
            (KeyCode::Left, _) => finder.move_cursor_left(),
            (KeyCode::Right, _) => finder.move_cursor_right(),
            (KeyCode::Home, _) => finder.move_cursor_home(),
            (KeyCode::End, _) => finder.move_cursor_end(),
            (KeyCode::Backspace, _) => finder.pop_char(),
            (KeyCode::Delete, _) => finder.delete_char(),
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                finder.push_char(c);
            }
            _ => {}
        }
        self.follow_quick_open_prefix();
    }

    /// VS Code's shared quick input: the first character of Quick Open's
    /// query picks the mode, and the text after it carries over as the new
    /// query. `#` is Go to Symbol in Workspace, `>` the Command Palette
    /// (#1306), `@` Go to Symbol in Editor, and `:` Go to Line, which is
    /// Go to Symbol's own `:N` mode. Anything else stays a file search,
    /// `name:12` included.
    pub(super) fn follow_quick_open_prefix(&mut self) {
        let Some(query) = self.file_finder.as_ref().map(|f| f.query.clone()) else {
            return;
        };
        let mut chars = query.chars();
        let (Some(prefix), rest) = (chars.next(), chars.as_str()) else {
            return;
        };
        if !matches!(prefix, '#' | '>' | '@' | ':') {
            return;
        }
        self.close_file_finder();
        match prefix {
            '#' => self.open_workspace_symbols(rest),
            '>' => {
                self.open_command_palette();
                if let Some(palette) = self.command_palette.as_mut() {
                    palette.set_query(rest);
                }
            }
            _ => {
                self.open_go_to_symbol();
                if let Some(picker) = self.go_to_symbol.as_mut() {
                    let carried = if prefix == ':' { &query } else { rest };
                    for c in carried.chars() {
                        picker.push_char(c);
                    }
                }
            }
        }
    }

    /// Open the highlighted result and dismiss the finder: Enter's action,
    /// shared with a mouse click on a result row.
    pub(super) fn open_file_finder_selection(&mut self) {
        let Some(finder) = self.file_finder.as_ref() else {
            return;
        };
        let path = finder.selected_entry().map(|e| e.path.clone());
        let hint = finder.line_hint();
        if let Some(path) = path {
            self.close_file_finder();
            // `alpha:236-239` (#472): land on the line, centred, and select
            // the range so it reads as highlighted; `alpha:236` lands with
            // nothing selected, since one line is a place, not a span.
            let opened = match hint {
                Some(hint) => {
                    let row = hint.line.saturating_sub(1);
                    let col = hint.col.map_or(0, |c| c.saturating_sub(1));
                    self.open_at(&path, row, col)
                }
                None => self.editor.open_preview(&path),
            };
            match opened {
                Ok(()) => {
                    self.sync_open_file_poll_mtime();
                    // A range an earlier hinted pick installed is cleared if
                    // it is still the selection in its tab, so neither
                    // `alpha:12` nor a bare `alpha` keeps showing it; a
                    // selection the user made or extended since is theirs
                    // and stays. Stale secondary carets never survive a
                    // pick, as they never survive a click: the next
                    // keystroke would edit at every one of them.
                    if let Some((prev, sel)) = self.quick_open_range.take()
                        && let Some(idx) = self.editor.find_tab_with_path(&prev)
                        && self.editor.editors[idx].selection == Some(sel)
                    {
                        self.editor.editors[idx].selection = None;
                    }
                    if hint.is_some() {
                        self.editor.clear_selection();
                    }
                    self.editor.collapse_carets();
                    if let Some(end) = hint.and_then(|h| h.end) {
                        let start = self.editor.cursor_row;
                        let last = self.editor.lines.len().saturating_sub(1);
                        let end_row = end.saturating_sub(1).clamp(start, last);
                        let end_col = self
                            .editor
                            .lines
                            .get(end_row)
                            .map_or(0, |l| l.chars().count());
                        let sel = crate::widgets::editor::EditorSelection {
                            anchor: (end_row, end_col),
                            head: (start, 0),
                        };
                        self.editor.selection = Some(sel);
                        self.quick_open_range = Some((path.to_path_buf(), sel));
                        // The landing line is the head, and the cursor sits at
                        // the head as every other selection-installing path
                        // leaves it: Shift+motion extends from the cursor, and
                        // the paint-time clamp keeps the cursor on screen, so
                        // a range taller than the viewport still shows the
                        // line the user asked for, centred by `open_at`, with
                        // the selection running down from it.
                        self.editor.cursor_row = start;
                        self.editor.cursor_col = 0;
                    }
                    // VS Code's `explorer.autoReveal` analogue:
                    // expand every parent dir of the picked file
                    // and park the cursor on its row so the user
                    // sees where in the workspace the file lives.
                    self.tree.reveal_path(&path);
                    self.focus_pane(Pane::Editor);
                    // The status names the lines landed on: a number past the
                    // file (or one that saturated in the parser) was clamped
                    // above, and the clamped truth is the useful signal.
                    let last_line = self.editor.lines.len().max(1);
                    self.status = match hint {
                        Some(hint) => match hint.end {
                            Some(end) => format!(
                                "Opened {}:{}-{}",
                                self.status_path(&path),
                                hint.line.min(last_line),
                                end.min(last_line)
                            ),
                            None => format!(
                                "Opened {}:{}",
                                self.status_path(&path),
                                hint.line.min(last_line)
                            ),
                        },
                        None => format!("Opened {}", self.status_path(&path)),
                    };
                }
                Err(e) => {
                    self.status = format!("Open failed: {e}");
                }
            }
        }
    }

    pub(super) fn handle_file_finder_mouse(&mut self, m: MouseEvent) {
        let Some(finder) = self.file_finder.as_mut() else {
            return;
        };
        let inside = rect_contains(finder.last_rect, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    finder.select_next();
                }
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    finder.select_prev();
                }
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
                if !inside =>
            {
                self.close_file_finder();
            }
            // Click a row to open it, the way VS Code's quick-pick behaves.
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = finder.row_index_at(m.row) {
                    finder.selected = idx;
                    self.open_file_finder_selection();
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_file_finder(&mut self, frame: &mut ratatui::Frame) {
        let theme = self.theme;
        let center = self.quick_input_position == QuickInputPosition::Center;
        // Inline images are baked pixels the scrim cannot dim, so the
        // backdrop stays lit where the terminal draws them.
        let backdrop = !self.inline_images_enabled();
        let Some(finder) = self.file_finder.as_mut() else {
            return;
        };
        let area = frame.area();
        crate::widgets::file_finder::render_file_finder(
            finder,
            area,
            frame.buffer_mut(),
            theme,
            center,
            backdrop,
        );
    }

    pub(super) fn open_editor_find(&mut self) {
        // A hex tab's find is the byte-query popup, not the text bar
        // (#172): route there whether the chord or the palette got here.
        if self.editor.hex.is_some() {
            self.open_hex_find_prompt();
            return;
        }
        if self.editor_find.is_some() {
            self.reselect_editor_find();
            return;
        }
        let opts = self.search.opts;
        if self.editor.diff.is_some() {
            let initial = self
                .editor
                .diff
                .as_ref()
                .and_then(|d| d.selection_text())
                .filter(|s| !s.contains('\n'))
                .unwrap_or_default();
            self.editor_find = Some(crate::widgets::editor_find::EditorFind::new(
                String::new(),
                opts,
            ));
            if !initial.is_empty() {
                self.diff_find_set_query(initial);
            }
            self.status =
                String::from("Find: type to search, Enter next, Shift+Enter prev, Esc close");
            return;
        }
        let initial = self.editor_find_seed();
        let mut state = crate::widgets::editor_find::EditorFind::new(initial.clone(), opts);
        // Selected, so what the user types next replaces the seed (#1278).
        state.query_selected = !initial.is_empty();
        if !initial.is_empty() {
            state.set_match_count(
                crate::widgets::editor_find::count_matches(&self.editor.lines, &initial, opts),
                false,
            );
            self.editor
                .set_search_highlight(Some(initial.clone()), opts);
        }
        self.editor_find = Some(state);
        self.status = String::from("Find: type to search, Enter next, Shift+Enter prev, Esc close");
    }

    /// `Cmd+Opt+F` (VS Code `editor.action.startFindReplaceAction`): open
    /// the find bar expanded with the replace row, or toggle the row on an
    /// already-open bar. Read-only tabs (diff / sheet / image previews)
    /// get the plain find bar — there is nothing to replace into.
    pub(super) fn open_editor_replace(&mut self) {
        if !self.editor_is_text() {
            self.open_editor_find();
            return;
        }
        // The cursor starts in Replace only when the query is already the
        // user's choice, as in VS Code: the bar was open (they typed or took
        // the find text there) or the text came from a one-line selection.
        // A word merely under the caret is a guess: starting in Replace put
        // what they typed to find into Replace, and Replace All then made
        // the opposite edit (#1592).
        let selection = self.editor.selection_text();
        let chosen =
            self.editor_find.is_some() || (!selection.is_empty() && !selection.contains('\n'));
        if self.editor_find.is_none() {
            self.open_editor_find();
        }
        let Some(state) = self.editor_find.as_mut() else {
            return;
        };
        state.replace_visible = !state.replace_visible;
        state.focus = if state.replace_visible && !state.query.is_empty() && chosen {
            crate::widgets::editor_find::FindField::Replace
        } else {
            crate::widgets::editor_find::FindField::Query
        };
        if state.replace_visible {
            self.status = String::from(
                "Replace: Tab switches field, Enter replaces, Cmd+Opt+Enter replaces all, Esc close",
            );
        }
    }

    pub(super) fn close_editor_find(&mut self) {
        if self.editor_find.take().is_some() {
            // Every tab's: a range taken in one tab and left there when the
            // bar closes from another would clip that tab's next find.
            for tab in self.editor.editors.iter_mut() {
                tab.find_range = None;
            }
            for group in self.editor_layout.inactive_groups_mut() {
                for tab in group.editors.iter_mut() {
                    tab.find_range = None;
                }
            }
            self.editor.set_search_highlight(None, self.search.opts);
            if let Some(diff) = self.editor.diff.as_mut() {
                diff.find.needle = None;
                diff.find.active = None;
            }
            self.status.clear();
        }
    }

    /// Viewport geometry (visible rows, visible text columns) of the diff
    /// body, derived from the last rendered inner rect the same way
    /// `render_diff` lays the columns out. Used to keep a jumped-to match
    /// on screen.
    pub(super) fn diff_find_viewport(&self) -> (usize, usize) {
        let inner = self.editor.last_inner;
        let viewport_rows = (inner.height as usize).saturating_sub(2);
        let half = inner.width / 2;
        let text_cols = match self.editor.diff.as_ref() {
            Some(diff) => {
                let r_gutter = (diff.right_lines.len() + 1).to_string().len() as u16 + 1;
                inner
                    .width
                    .saturating_sub(half + 1)
                    .saturating_sub(r_gutter + 2) as usize
            }
            None => 0,
        };
        (viewport_rows, text_cols)
    }

    /// Recompute the diff's matches for the current query, select the
    /// match at `target` (wrapping), scroll it into view, and sync the
    /// find bar's count / index. Drives every diff find action.
    pub(super) fn diff_find_apply(&mut self, target: usize) {
        let (needle, opts) = match self.editor_find.as_ref() {
            Some(s) => (s.query.clone(), s.opts),
            None => return,
        };
        let (viewport_rows, text_cols) = self.diff_find_viewport();
        let Some(diff) = self.editor.diff.as_mut() else {
            return;
        };
        diff.find.opts = opts;
        if needle.is_empty() {
            diff.find.needle = None;
            diff.find.active = None;
            if let Some(s) = self.editor_find.as_mut() {
                s.set_match_count(0, false);
                s.match_index = None;
            }
            return;
        }
        diff.find.needle = Some(needle.clone());
        let matches = diff.find_matches(&needle, opts);
        let count = matches.len();
        if count == 0 {
            diff.find.active = None;
            if let Some(s) = self.editor_find.as_mut() {
                s.set_match_count(0, false);
                s.match_index = None;
            }
            return;
        }
        let pos = target % count;
        let m = matches[pos];
        diff.find.active = Some(m);
        diff.scroll_to_match(m, viewport_rows, text_cols);
        if let Some(s) = self.editor_find.as_mut() {
            s.set_match_count(count, false);
            s.match_index = Some(pos + 1);
        }
    }

    pub(super) fn diff_find_set_query(&mut self, new_query: String) {
        if let Some(s) = self.editor_find.as_mut() {
            s.query = new_query;
        }
        self.diff_find_apply(0);
    }

    pub(super) fn diff_find_current_pos(&self) -> usize {
        self.editor_find
            .as_ref()
            .and_then(|s| s.match_index)
            .map(|i| i.saturating_sub(1))
            .unwrap_or(0)
    }

    pub(super) fn editor_find_set_query(&mut self, new_query: String) {
        // A whole-query replacement puts the caret after it; the caret
        // editors below then move it where the edit left it.
        // Assigned, not `set_caret`: that clamps to the query still in place,
        // so a longer replacement would leave the caret inside it. Reads
        // clamp through `caret()` once the new query is in.
        if let Some(s) = self.editor_find.as_mut() {
            s.caret = new_query.chars().count();
        }
        let unchanged = self
            .editor_find
            .as_ref()
            .map(|s| s.query == new_query)
            .unwrap_or(true);
        if unchanged {
            return;
        }
        if self.editor.diff.is_some() {
            self.diff_find_set_query(new_query);
            return;
        }
        if self.editor.log.is_some() {
            self.log_find_set_query(new_query);
            return;
        }
        let range = self.editor_find_range();
        let (scope, _, window) = editor_find_lines(&self.editor, range);
        let Some(state) = self.editor_find.as_mut() else {
            return;
        };
        state.query = new_query;
        let opts = state.opts;
        state.set_match_count(
            crate::widgets::editor_find::count_matches_in(scope, &state.query, opts, window),
            false,
        );
        if state.query.is_empty() {
            state.match_index = None;
            self.editor.active_search_match = None;
            self.editor.set_search_highlight(None, opts);
            return;
        }
        let query = state.query.clone();
        self.editor.set_search_highlight(Some(query.clone()), opts);
        let from_row = self.editor.cursor_row;
        let from_col = self.editor.cursor_col;
        if let Some(m) = self.editor_find_step(true, &query, opts, from_row, from_col, false) {
            self.jump_editor_to_match(m);
        }
        self.refresh_editor_find_index();
    }

    pub(super) fn editor_find_jump_next(&mut self) {
        let Some(state) = self.editor_find.as_ref() else {
            return;
        };
        if state.query.is_empty() {
            return;
        }
        if self.editor.diff.is_some() {
            self.diff_find_apply(self.diff_find_current_pos() + 1);
            return;
        }
        if self.editor.log.is_some() {
            self.log_find_step(true);
            return;
        }
        let opts = state.opts;
        let needle = state.query.clone();
        if let Some(m) = self.editor_find_step(
            true,
            &needle,
            opts,
            self.editor.cursor_row,
            self.editor.cursor_col,
            true,
        ) {
            self.jump_editor_to_match(m);
            self.refresh_editor_find_index();
        }
    }

    pub(super) fn editor_find_jump_prev(&mut self) {
        let Some(state) = self.editor_find.as_ref() else {
            return;
        };
        if state.query.is_empty() {
            return;
        }
        if self.editor.diff.is_some() {
            let count = state.match_count;
            if count == 0 {
                return;
            }
            self.diff_find_apply(self.diff_find_current_pos() + count - 1);
            return;
        }
        if self.editor.log.is_some() {
            self.log_find_step(false);
            return;
        }
        let opts = state.opts;
        let needle = state.query.clone();
        if let Some(m) = self.editor_find_step(
            false,
            &needle,
            opts,
            self.editor.cursor_row,
            self.editor.cursor_col,
            true,
        ) {
            self.jump_editor_to_match(m);
            self.refresh_editor_find_index();
        }
    }

    /// Find over a rendered ANSI log (#257).
    ///
    /// A log tab's `lines` is a one-line stub, because the file stays on disk
    /// and only a window of it is ever parsed. Running the ordinary find path
    /// over that stub answers "No results" for a file full of matches, so the
    /// search goes through the view's own stripped text instead, the same fork
    /// the diff view already takes.
    pub(super) fn log_find_set_query(&mut self, new_query: String) {
        let Some(state) = self.editor_find.as_mut() else {
            return;
        };
        state.query = new_query;
        let opts = state.opts;
        let needle = state.query.clone();
        if needle.is_empty() {
            state.set_match_count(0, false);
            state.match_index = None;
            self.editor.active_search_match = None;
            self.editor.set_search_highlight(None, opts);
            return;
        }
        let Some(log) = self.editor.log.as_ref() else {
            return;
        };
        let (count, truncated) = log.count_matches(&needle, opts);
        // Search from the top of the viewport, so typing finds what is on
        // screen first rather than jumping to the head of the file.
        let hit = log.find_next(&needle, opts, self.editor.scroll, 0, false);
        if let Some(state) = self.editor_find.as_mut() {
            state.set_match_count(count, truncated);
            // "N of M" would have to count the matches BEFORE this one, which
            // for a budgeted scan is a number we may not have; the bar falls
            // back to "M matches" rather than guessing an index.
            state.match_index = None;
        }
        self.editor.set_search_highlight(Some(needle), opts);
        // A CHANGED query invalidates the old position outright, so the
        // active match is cleared before the new step is applied. The early
        // return for an out-of-reach STEP deliberately keeps the match the
        // user was on; doing the same here left the previous query's
        // highlight painted under a new query that could not reach a match
        // of its own.
        self.editor.active_search_match = None;
        self.scroll_log_to_match(hit);
    }

    /// Enter / Shift+Enter over a rendered log.
    pub(super) fn log_find_step(&mut self, forward: bool) {
        let Some(state) = self.editor_find.as_ref() else {
            return;
        };
        let opts = state.opts;
        let needle = state.query.clone();
        let Some(log) = self.editor.log.as_ref() else {
            return;
        };
        // With no active match, walk from the viewport rather than the file
        // head, so the first step lands near what the user is looking at.
        let (row, col) = match self.editor.active_search_match {
            Some((r, c, _)) => (r, c),
            None => (self.editor.scroll, 0),
        };
        let hit = if forward {
            log.find_next(&needle, opts, row, col, true)
        } else {
            log.find_prev(&needle, opts, row, col)
        };
        self.scroll_log_to_match(hit);
    }

    /// Park the viewport on the step's match and mark it active. A log has
    /// no caret, so the match position is carried by `active_search_match`
    /// alone and the row is centred rather than merely revealed.
    ///
    /// A step that ran out of budget is NOT the same as one that found
    /// nothing: it leaves the count marked partial, so the bar keeps saying
    /// how far the search actually reached instead of claiming "No results"
    /// for a file it never finished reading.
    pub(super) fn scroll_log_to_match(&mut self, step: crate::log_view::Step) {
        if step.out_of_reach() {
            // Mark the count partial and STOP. Falling through to clear the
            // active match left the bar reading "1+ matches" with nothing
            // highlighted and no way back to the match the user was on: a
            // step that could not reach further must leave them where they
            // are, not throw away the position it did have.
            if let Some(state) = self.editor_find.as_mut() {
                let count = state.match_count;
                state.set_match_count(count, true);
            }
            return;
        }
        let Some(m) = step.found() else {
            self.editor.active_search_match = None;
            return;
        };
        self.editor.active_search_match = Some((m.row, m.col_chars, m.len_chars));
        // The match is the reader's place now; a reload still settling its
        // own place must not scroll it away on the next index batch.
        self.editor.cancel_log_reload();
        // The log body starts one row below the header.
        let rows = (self.editor.last_inner.height as usize).saturating_sub(1);
        if rows == 0 {
            self.editor.scroll = m.row;
            return;
        }
        if m.row < self.editor.scroll || m.row >= self.editor.scroll + rows {
            self.editor.scroll = m.row.saturating_sub(rows / 2);
        }
    }

    pub(super) fn jump_editor_to_match(&mut self, m: crate::widgets::editor_find::MatchPos) {
        self.editor.cursor_row = m.row;
        self.editor.cursor_col = m.col_chars;
        self.editor.active_search_match = Some((m.row, m.col_chars, m.len_chars));
        let viewport = self.editor.last_inner.height as usize;
        if viewport > 0 {
            if self.editor.cursor_row < self.editor.scroll {
                self.editor.scroll = self.editor.cursor_row;
            } else if self.editor.cursor_row >= self.editor.scroll + viewport {
                self.editor.scroll = self.editor.cursor_row + 1 - viewport;
            }
        }
        self.editor.ensure_cursor_col_visible();
    }

    /// The next (`forward`) or previous find-bar match from `(row, col)`,
    /// searching and wrapping within a symbol tab's symbol only (#369).
    pub(super) fn editor_find_step(
        &self,
        forward: bool,
        needle: &str,
        opts: crate::widgets::search::SearchOpts,
        row: usize,
        col: usize,
        skip_current: bool,
    ) -> Option<crate::widgets::editor_find::MatchPos> {
        let range = self.editor_find_range();
        let (scope, first, window) = editor_find_lines(&self.editor, range);
        let (row, col) = editor_find_scope_pos(scope, first, window, (row, col))?;
        let step = if forward {
            crate::widgets::editor_find::find_next_match_in
        } else {
            crate::widgets::editor_find::find_prev_match_in
        };
        let mut m = step(scope, needle, opts, window, row, col, skip_current)?;
        m.row += first;
        Some(m)
    }

    pub(super) fn refresh_editor_find_index(&mut self) {
        let range = self.editor_find_range();
        let (scope, first, window) = editor_find_lines(&self.editor, range);
        let caret = (self.editor.cursor_row, self.editor.cursor_col);
        // A caret outside the selection is on none of its matches.
        let outside = range.is_some_and(|(a, b)| caret < a || caret > b);
        let at = editor_find_scope_pos(scope, first, window, caret);
        let Some(state) = self.editor_find.as_mut() else {
            return;
        };
        let opts = state.opts;
        state.match_index = at.filter(|_| !outside).and_then(|(row, col)| {
            crate::widgets::editor_find::match_index_at_in(
                scope,
                &state.query,
                opts,
                window,
                row,
                col,
            )
        });
    }

    /// The range Find in Selection limits the open find bar to: the active
    /// tab's own (#1577), so another tab never searches by its coordinates.
    pub(super) fn editor_find_range(&self) -> Option<crate::widgets::editor_find::FindRange> {
        self.editor_find.as_ref()?;
        self.editor.find_range
    }

    /// Find in Selection (#1577, `Alt+L` in the find bar, VS Code's `≡`):
    /// limit the matches, the count and Replace All to the selection (the
    /// caret's line with none), or lift the limit again. From outside the
    /// find bar it opens it first.
    pub(super) fn toggle_editor_find_in_selection(&mut self) {
        if !self.editor_is_text() || self.editor.diff.is_some() {
            self.status = String::from("Find in Selection works on text tabs");
            return;
        }
        let range = self
            .editor
            .selection
            .as_ref()
            .filter(|s| s.anchor != s.head)
            .map(|s| s.normalised())
            .unwrap_or_else(|| {
                let row = self.editor.cursor_row;
                (
                    (row, 0),
                    (
                        row,
                        self.editor.lines.get(row).map_or(0, |l| l.chars().count()),
                    ),
                )
            });
        if self.editor_find.is_none() {
            self.open_editor_find();
        }
        let Some(state) = self.editor_find.as_mut() else {
            return;
        };
        // The range belongs to the tab it was taken in (#1577): another
        // tab under the same find bar searches whole until it gets its own.
        let on = self.editor.find_range.is_none();
        self.editor.find_range = on.then_some(range);
        state.in_selection = self.editor.find_range;
        self.status = String::from(if on {
            "Find in Selection: on"
        } else {
            "Find in Selection: off"
        });
        self.editor_find_recount();
    }

    /// Recount the find bar's matches and re-place the active one, after
    /// its scope changed under an unchanged query.
    pub(super) fn editor_find_recount(&mut self) {
        let range = self.editor_find_range();
        let (scope, _, window) = editor_find_lines(&self.editor, range);
        let Some(state) = self.editor_find.as_mut() else {
            return;
        };
        let (needle, opts) = (state.query.clone(), state.opts);
        state.set_match_count(
            crate::widgets::editor_find::count_matches_in(scope, &needle, opts, window),
            false,
        );
        self.editor.active_search_match = None;
        if needle.is_empty() {
            return;
        }
        let (row, col) = (self.editor.cursor_row, self.editor.cursor_col);
        if let Some(m) = self.editor_find_step(true, &needle, opts, row, col, false) {
            self.jump_editor_to_match(m);
        }
        self.refresh_editor_find_index();
    }

    /// True when the replace row is shown and owns the keyboard.
    pub(super) fn editor_find_replace_focused(&self) -> bool {
        self.editor_find
            .as_ref()
            .map(|s| {
                s.replace_visible && s.focus == crate::widgets::editor_find::FindField::Replace
            })
            .unwrap_or(false)
    }

    /// Edit the focused find field around the caret (#1729): the query row
    /// passes its caret through `edit`, the replace row — which has none —
    /// always edits at its end.
    pub(super) fn editor_find_edit_at_caret(
        &mut self,
        edit: impl FnOnce(&str, usize) -> (String, usize),
    ) {
        if self.editor_find_replace_focused() {
            if let Some(s) = self.editor_find.as_mut() {
                let (text, caret) = edit(&s.replace, s.replace_caret());
                s.replace = text;
                s.set_replace_caret(caret);
            }
            return;
        }
        let Some((text, caret)) = self.editor_find.as_ref().map(|s| {
            let (text, caret) = edit(&s.query, s.caret());
            (text, caret)
        }) else {
            return;
        };
        self.editor_find_set_query(text);
        if let Some(s) = self.editor_find.as_mut() {
            s.set_caret(caret);
        }
    }

    /// Move the focused find bar's caret without touching the text. The
    /// selection a seed put on the query is dropped, as any caret key does.
    pub(super) fn editor_find_move_caret(&mut self, step: impl FnOnce(&str, usize) -> usize) {
        if self.editor_find_replace_focused() {
            if let Some(s) = self.editor_find.as_mut() {
                let at = step(&s.replace, s.replace_caret());
                s.set_replace_caret(at);
            }
            return;
        }
        if let Some(s) = self.editor_find.as_mut() {
            let at = step(&s.query, s.caret());
            s.set_caret(at);
            s.query_selected = false;
        }
    }

    /// What Ctrl+F puts in the find bar: a one-line selection, else the
    /// whole word under the caret or just left of it (#1278; the half of
    /// the word before the caret made typing build `loadOrsave`).
    pub(super) fn editor_find_seed(&self) -> String {
        let selection = self.editor.selection_text();
        if !selection.is_empty() && !selection.contains('\n') {
            return selection;
        }
        let (row, col) = (self.editor.cursor_row, self.editor.cursor_col);
        self.editor
            .word_string_at(row, col)
            .or_else(|| {
                col.checked_sub(1)
                    .and_then(|c| self.editor.word_string_at(row, c))
            })
            .unwrap_or_default()
    }

    /// Ctrl+F on an open find bar, as in VS Code: take a one-line selection
    /// if there is one, focus the query and select it, so typing replaces it.
    pub(super) fn reselect_editor_find(&mut self) {
        let selection = self.editor.selection_text();
        if self.editor.diff.is_none() && !selection.is_empty() && !selection.contains('\n') {
            self.editor_find_set_query(selection);
        }
        if let Some(s) = self.editor_find.as_mut() {
            s.focus = crate::widgets::editor_find::FindField::Query;
            s.query_selected = !s.query.is_empty();
        }
    }

    pub(super) fn handle_editor_find_key(&mut self, key: KeyEvent) {
        if is_editor_replace_key(key) {
            self.open_editor_replace();
            return;
        }
        if is_editor_find_key(key) {
            self.reselect_editor_find();
            return;
        }
        // Find in Selection (#1577): VS Code's `Alt+L`. A bare Alt only, so
        // a macOS keyboard typing `¬` with Option keeps it.
        // Ahead of the selected query below, which `Alt+L` must not clear.
        if key.modifiers == KeyModifiers::ALT && matches!(key.code, KeyCode::Char('l' | 'L')) {
            self.toggle_editor_find_in_selection();
            return;
        }
        // A selected query (#1278) is replaced by what is typed or pasted
        // and cleared by Backspace / Delete; any other editing or caret key
        // drops the selection and then acts as usual.
        let selected = !self.editor_find_replace_focused()
            && self.editor_find.as_ref().is_some_and(|s| s.query_selected);
        if selected {
            let plain_char = matches!(key.code, KeyCode::Char(_))
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
            let paste = matches!(key.code, KeyCode::Char('v'))
                && key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
            let keeps = matches!(
                key.code,
                KeyCode::Enter | KeyCode::F(3) | KeyCode::Tab | KeyCode::BackTab | KeyCode::Esc
            ) || (matches!(key.code, KeyCode::Char('a'))
                && key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER));
            if let Some(s) = self.editor_find.as_mut()
                && !keeps
            {
                s.query_selected = false;
            }
            if plain_char || paste || matches!(key.code, KeyCode::Backspace | KeyCode::Delete) {
                self.editor_find_set_query(String::new());
                if matches!(key.code, KeyCode::Backspace | KeyCode::Delete) {
                    return;
                }
            }
        }
        let word_delete = key.code == KeyCode::Backspace
            && key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if word_delete {
            // The word before the caret, as in a text field; a lone
            // punctuation mark goes on its own.
            self.editor_find_edit_at_caret(|text, caret| {
                crate::widgets::editor_find::delete_before(text, caret, true)
            });
            return;
        }
        if matches!(key.code, KeyCode::Char('a'))
            && key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
        {
            if let Some(s) = self.editor_find.as_mut()
                && s.focus == crate::widgets::editor_find::FindField::Query
            {
                s.query_selected = !s.query.is_empty();
            }
            return;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let cmd_like = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
        let by_word = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT);
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.close_editor_find(),
            // `Cmd+Opt+Enter` (Linux: `Ctrl+Alt+Enter`) = Replace All,
            // VS Code's binding, from either field.
            (KeyCode::Enter, m) if m.contains(KeyModifiers::ALT) && cmd_like => {
                self.editor_find_replace_all();
            }
            (KeyCode::Enter, _) => {
                if self.editor_find_replace_focused() {
                    self.editor_find_replace_current();
                } else if shift {
                    self.editor_find_jump_prev();
                } else {
                    self.editor_find_jump_next();
                }
            }
            (KeyCode::Tab | KeyCode::BackTab, _) => {
                if let Some(s) = self.editor_find.as_mut()
                    && s.replace_visible
                {
                    use crate::widgets::editor_find::FindField;
                    s.focus = match s.focus {
                        FindField::Query => FindField::Replace,
                        FindField::Replace => FindField::Query,
                    };
                }
            }
            (KeyCode::F(3), _) => {
                if shift {
                    self.editor_find_jump_prev();
                } else {
                    self.editor_find_jump_next();
                }
            }
            // The caret keys (#1729): the query is a real input line, so
            // Left/Right/Home/End move inside it and Delete removes what
            // lies ahead. Ctrl/Cmd/Alt with Left/Right/Delete walks by a
            // word, as a text field does.
            (KeyCode::Left, _) => {
                self.editor_find_move_caret(|t, c| {
                    crate::widgets::editor_find::caret_left(t, c, by_word)
                });
            }
            (KeyCode::Right, _) => {
                self.editor_find_move_caret(|t, c| {
                    crate::widgets::editor_find::caret_right(t, c, by_word)
                });
            }
            (KeyCode::Home, _) => self.editor_find_move_caret(|_, _| 0),
            (KeyCode::End, _) => self.editor_find_move_caret(|t, _| t.chars().count()),
            (KeyCode::Delete, _) => {
                self.editor_find_edit_at_caret(|t, c| {
                    crate::widgets::editor_find::delete_at(t, c, by_word)
                });
            }
            (KeyCode::Backspace, _) => {
                self.editor_find_edit_at_caret(|t, c| {
                    crate::widgets::editor_find::delete_before(t, c, false)
                });
            }
            (KeyCode::Char('v'), m)
                if m.contains(KeyModifiers::CONTROL) || m.contains(KeyModifiers::SUPER) =>
            {
                if let Some(text) = self.clipboard_text() {
                    self.editor_find_edit_at_caret(|t, c| {
                        crate::widgets::editor_find::insert_at(t, c, &text)
                    });
                }
            }
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                let typed = c.to_string();
                self.editor_find_edit_at_caret(|t, caret| {
                    crate::widgets::editor_find::insert_at(t, caret, &typed)
                });
            }
            _ => {}
        }
    }

    /// Replace the active find match with the replace field's text
    /// (expanding `$1` captures in regex mode) and advance to the next
    /// match — the find bar's Replace (Enter in the replace row). With no
    /// active match, Enter just finds the next one, like VS Code.
    pub(super) fn editor_find_replace_current(&mut self) {
        if !self.editor_is_text() {
            return;
        }
        let Some(state) = self.editor_find.as_ref() else {
            return;
        };
        if state.query.is_empty() {
            return;
        }
        let (needle, replacement, opts) = (state.query.clone(), state.replace.clone(), state.opts);
        let Some((row, col, len)) = self.editor.active_search_match else {
            self.editor_find_jump_next();
            return;
        };
        let Some(line) = self.editor.lines.get(row) else {
            return;
        };
        let Some(text) = crate::widgets::editor_find::expand_replacement(
            line,
            col,
            len,
            &needle,
            &replacement,
            opts,
        ) else {
            // Stale match position (buffer changed underneath): resync
            // instead of splicing blindly.
            self.editor_find_jump_next();
            return;
        };
        self.editor.replace_find_match(row, col, len, &text);
        // The selected range ends further on (or sooner) by what the
        // replacement added (#1577).
        if let Some(s) = self.editor_find.as_mut()
            && let Some((start, end)) = self.editor.find_range.as_mut()
        {
            // As `replace_find_match` put it in: a `\r` is a line break too.
            let text = crate::widgets::editor::normalize_newlines(&text);
            let breaks = text.matches('\n').count();
            if row == end.0 {
                let after = end.1.saturating_sub(col + len);
                end.1 = match text.rfind('\n') {
                    Some(i) => text[i + 1..].chars().count() + after,
                    None => col + text.chars().count() + after,
                };
            }
            if row <= end.0 {
                end.0 += breaks;
            }
            debug_assert!(*start <= *end);
            s.in_selection = self.editor.find_range;
        }
        self.editor.active_search_match = None;
        self.editor.set_search_highlight(Some(needle.clone()), opts);
        let range = self.editor_find_range();
        let (scope, _, window) = editor_find_lines(&self.editor, range);
        if let Some(s) = self.editor_find.as_mut() {
            s.set_match_count(
                crate::widgets::editor_find::count_matches_in(scope, &needle, opts, window),
                false,
            );
        }
        if let Some(m) = self.editor_find_step(
            true,
            &needle,
            opts,
            self.editor.cursor_row,
            self.editor.cursor_col,
            false,
        ) {
            self.jump_editor_to_match(m);
        }
        self.refresh_editor_find_index();
    }

    /// Replace every match in the buffer as one undo step — the find
    /// bar's Replace All (`Cmd+Opt+Enter`).
    pub(super) fn editor_find_replace_all(&mut self) {
        if !self.editor_is_text() {
            return;
        }
        let Some(state) = self.editor_find.as_ref() else {
            return;
        };
        if !state.replace_visible || state.query.is_empty() {
            return;
        }
        let (needle, replacement, opts) = (state.query.clone(), state.replace.clone(), state.opts);
        // A symbol tab replaces within its symbol only (#369), and Find in
        // Selection within the selected range (#1577).
        let range = self.editor_find_range();
        let (scope, first, window) = editor_find_lines(&self.editor, range);
        let scope_end = first + scope.len();
        let Some((replaced, n)) = crate::widgets::editor_find::replace_all_in_lines_in(
            scope,
            &needle,
            &replacement,
            opts,
            window,
        ) else {
            return;
        };
        if n == 0 {
            self.status = String::from("Replace All: no matches");
            return;
        }
        // Text after the range's end is untouched, so the end keeps its
        // distance from the end of its (now last replaced) line.
        let new_range = window.map(|(start, (_, c1))| {
            let tail = scope
                .last()
                .map_or(0, |l| l.chars().count().saturating_sub(c1));
            let last = replaced.last().map_or(0, |l| l.chars().count());
            (
                (first + start.0, start.1),
                (
                    first + replaced.len().saturating_sub(1),
                    last.saturating_sub(tail),
                ),
            )
        });
        let mut new_lines = self.editor.lines[..first].to_vec();
        new_lines.extend(replaced);
        new_lines.extend_from_slice(&self.editor.lines[scope_end..]);
        self.editor.replace_all_lines(new_lines);
        self.editor.active_search_match = None;
        if range.is_some() {
            self.editor.find_range = new_range;
            if let Some(s) = self.editor_find.as_mut() {
                s.in_selection = new_range;
            }
        }
        let (scope, _, window) = editor_find_lines(&self.editor, new_range);
        if let Some(s) = self.editor_find.as_mut() {
            // Recount against the new buffer: `a` -> `aa` doubles the
            // matches, and a hard zero would read "No results" over a body
            // still painted full of highlights.
            s.set_match_count(
                crate::widgets::editor_find::count_matches_in(scope, &needle, opts, window),
                false,
            );
            s.match_index = None;
        }
        self.editor.set_search_highlight(Some(needle), opts);
        self.status = if n == 1 {
            String::from("Replaced 1 occurrence")
        } else {
            format!("Replaced {n} occurrences")
        };
    }

    pub(super) fn render_editor_find(&mut self, frame: &mut ratatui::Frame) {
        let theme = self.theme;
        let Some(state) = self.editor_find.as_mut() else {
            return;
        };
        // The chip shows the active tab's range, not one left in another.
        state.in_selection = self.editor.find_range;
        let area = self.editor.last_full_area;
        if area.width == 0 || area.height == 0 {
            return;
        }
        crate::widgets::editor_find::render_editor_find(state, area, frame.buffer_mut(), theme);
    }

    /// Search: Open Results in Editor (#615, Cmd+K Shift+F): the sidebar's
    /// query and results as a Search Editor tab.
    pub(super) fn open_search_editor(&mut self) {
        let header = crate::search_editor::Header {
            query: self.search.query.clone(),
            opts: self.search.opts,
            include: self.search.include.clone(),
            exclude: self.search.exclude.clone(),
        };
        let text = crate::search_editor::render(&header, &self.search.hits, &self.tree.root);
        let label = search_editor_label(&header.query);
        match self.editor.open_text_buffer(&label, &text) {
            Ok(()) => {
                self.focus_pane(Pane::Editor);
                self.status = if header.query.is_empty() {
                    String::from(
                        "Search Editor: type a query on the first line, then Cmd+K Shift+R",
                    )
                } else {
                    format!("Search Editor: {} result(s)", self.search.hits.len())
                };
            }
            Err(e) => self.status = format!("Search Editor: {e}"),
        }
    }

    /// Search Editor: Rerun (#615, Cmd+K Shift+R): search again for the query
    /// the active Search Editor's header spells, off the UI thread.
    pub(super) fn rerun_search_editor(&mut self) {
        let header = is_search_editor_tab(self.editor.path.as_deref())
            .then(|| crate::search_editor::parse_header(&self.editor.lines))
            .flatten();
        let Some(header) = header else {
            self.status = String::from("Not a Search Editor");
            return;
        };
        let Some(label) = self.editor.path.clone() else {
            return;
        };
        let root = self.tree.root.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let query = header.query.clone();
        let header_query = query.clone();
        let opts = header.opts;
        let filter = crate::widgets::search::PathFilter::new(&header.include, &header.exclude);
        std::thread::spawn(move || {
            let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let sink = hits.clone();
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            crate::widgets::search::search_workspace_streaming_filtered(
                &root,
                &query,
                opts,
                &cancel,
                &std::collections::HashSet::new(),
                &filter,
                move |batch| {
                    if let Ok(mut all) = sink.lock() {
                        all.extend(batch);
                    }
                },
            );
            let all = hits.lock().map(|h| h.clone()).unwrap_or_default();
            let _ = tx.send(all);
        });
        self.search_editor_job = Some((label, header, rx));
        self.status = format!(
            "Searching for {}\u{2026}",
            header_query_for_status(&header_query)
        );
    }

    /// Land a finished Search Editor rerun in its tab (#615).
    pub fn drain_search_editor(&mut self) -> bool {
        let Some((_, _, rx)) = self.search_editor_job.as_ref() else {
            return false;
        };
        let Ok(hits) = rx.try_recv() else {
            return false;
        };
        let (label, header, _) = self.search_editor_job.take().expect("checked");
        let text = crate::search_editor::render(&header, &hits, &self.tree.root);
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let mut landed = false;
        // A whole-buffer replacement like Replace All (#2074): one undo step,
        // the old selection and extra carets dropped (left on rows the new
        // results no longer have, they crashed the next frame), and the
        // tab's layout and highlights rebuilt for the new text.
        self.for_each_tab_of(&label, |tab| {
            tab.replace_all_lines(lines.clone());
            tab.cursor_col = 0;
            landed = true;
        });
        if landed {
            self.status = format!("Search Editor: {} result(s)", hits.len());
        }
        landed
    }

    /// Open the match under the caret when the active tab is a Search Editor
    /// and the caret is on a result row. Returns whether it did.
    pub(super) fn open_search_editor_result(&mut self) -> bool {
        if !is_search_editor_tab(self.editor.path.as_deref())
            || !crate::search_editor::is_search_editor(&self.editor.lines)
        {
            return false;
        }
        let Some((path, line)) = crate::search_editor::location_at(
            &self.editor.lines,
            self.editor.cursor_row,
            &self.tree.root,
        ) else {
            return false;
        };
        self.go_to_definition(path, line.saturating_sub(1) as u32, 0);
        true
    }

    /// Flip the Search panel's project exclusions on or off (#1345), VS
    /// Code's "Use Exclude Settings and Ignore Files", and search again.
    pub(super) fn toggle_project_excludes(&mut self) {
        self.search.use_exclude_settings = !self.search.use_exclude_settings;
        self.status = String::from(if self.search.use_exclude_settings {
            "Search: project exclusions on"
        } else {
            "Search: project exclusions off, searching every folder"
        });
        if !crate::widgets::search::as_typed(&self.search.query).is_empty() {
            self.submit_search_query();
        }
    }

    /// The Go to File index without the files the project excludes
    /// (#1345): VS Code's file search honours both exclude settings.
    pub(super) fn without_project_excludes(
        &self,
        entries: std::sync::Arc<Vec<crate::widgets::file_finder::FileEntry>>,
    ) -> std::sync::Arc<Vec<crate::widgets::file_finder::FileEntry>> {
        if self.search.project_exclude.is_empty() {
            return entries;
        }
        let filter = crate::widgets::search::PathFilter::excluding(&self.search.project_exclude);
        let kept = entries
            .iter()
            .filter(|e| {
                let root = self
                    .roots
                    .iter()
                    .find(|r| e.path.starts_with(r))
                    .map_or_else(|| self.roots.primary().to_path_buf(), |r| r.to_path_buf());
                !filter.excludes(&root, &e.path)
            })
            .cloned()
            .collect();
        std::sync::Arc::new(kept)
    }
}

/// The lines the find bar searches, the buffer row the first of them is,
/// and the part of them it searches: with Find in Selection the whole rows
/// the `range` spans and the range relative to them (#1577), so matches
/// are found on the real lines and kept when wholly inside; else a symbol
/// tab's symbol (#369) or the whole buffer.
pub(super) fn editor_find_lines(
    editor: &crate::widgets::editor::Editor,
    range: Option<crate::widgets::editor_find::FindRange>,
) -> (
    &[String],
    usize,
    Option<crate::widgets::editor_find::FindRange>,
) {
    if let Some(range) = range {
        let ((r0, c0), (r1, c1)) = crate::widgets::editor_find::clamp_range(&editor.lines, range);
        let lines = editor.lines.get(r0..=r1).unwrap_or(&[]);
        return (lines, r0, Some(((0, c0), (r1 - r0, c1))));
    }
    let (scope, first) = editor.find_scope();
    (scope, first, None)
}

/// Buffer position `(row, col)` as a position in the lines
/// [`editor_find_lines`] returned. Outside a selected `window` it goes to
/// the window's nearer edge, so the next match from a caret above the
/// selection is its first, and from below it wraps to it.
pub(super) fn editor_find_scope_pos(
    scope: &[String],
    first: usize,
    window: Option<crate::widgets::editor_find::FindRange>,
    (row, col): (usize, usize),
) -> Option<(usize, usize)> {
    let last = scope.len().checked_sub(1)?;
    let Some((start, end)) = window else {
        return Some((row.saturating_sub(first).min(last), col));
    };
    let Some(r) = row.checked_sub(first) else {
        return Some(start);
    };
    Some((r, col).clamp(start, end))
}

/// Human-readable name of a Search input field, for status-line messages.
pub(super) fn search_field_name(field: crate::widgets::search::SearchField) -> &'static str {
    use crate::widgets::search::SearchField;
    match field {
        SearchField::Query => "search",
        SearchField::Replace => "replace",
        SearchField::Include => "files-to-include",
        SearchField::Exclude => "files-to-exclude",
    }
}
