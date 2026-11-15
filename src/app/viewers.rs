//! Read-only viewers: the spreadsheet, CSV and SQLite grid, PDF, archives, hex, rendered
//! logs, and the Markdown preview with its scroll sync.

use super::*;

impl App {
    /// Cmd+F on a hex tab (#172): a single-line input popup, per croft's
    /// inputs-in-popups rule. Hex byte pairs or literal ASCII.
    pub(super) fn open_hex_find_prompt(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        if self.editor.hex.is_none() {
            return;
        }
        // Which reading a hex-shaped query gets depends on the column the
        // search starts from (#1644), so the title says it.
        let text_side = self.editor.hex.as_ref().is_some_and(|v| v.ascii_focus);
        self.open_input_prompt(
            InputPrompt::new(
                InputPurpose::HexFind,
                if text_side {
                    "Find in Hex (text)"
                } else {
                    "Find in Hex"
                },
                if text_side {
                    "text"
                } else {
                    "hex bytes (de ad be ef) or text"
                },
            )
            .with_hint(if text_side {
                "Enter finds this text · Esc cancels"
            } else {
                "\"1234\" finds hex-looking text · Enter · Esc cancels"
            }),
        );
    }

    /// Markdown: Toggle Preview — flips the active tab between source and the
    /// rendered view; explains itself on a non-Markdown tab.
    pub(super) fn toggle_markdown_preview(&mut self) {
        if self.editor.toggle_markdown_preview() {
            self.status = if self.editor.markdown_preview.is_some() {
                String::from("Markdown Preview")
            } else {
                String::from("Markdown source")
            };
        } else {
            self.status = String::from("Markdown Preview needs a Markdown file");
        }
    }

    /// Markdown: Open Preview to the Side (#1605, VS Code's
    /// `markdown.showPreviewToSide`): the active Markdown file rendered in
    /// the next editor group, or in a new group to the right when there is
    /// none, showing what is typed here as it is typed
    /// ([`App::sync_markdown_side_previews`]). Focus stays on the source,
    /// which a tab showing the preview turns back into.
    pub(super) fn open_markdown_preview_to_side(&mut self) {
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Open Preview to the Side: save the file first");
            return;
        };
        if self.editor.language() != Some(crate::highlight::LangKind::Markdown)
            || self.editor.has_non_text_view()
        {
            self.status = String::from("Markdown Preview needs a Markdown file");
            return;
        }
        if self.editor.markdown_preview.is_some() {
            self.editor.toggle_markdown_preview();
        }
        let source = self.editor_layout.active_dfs_index();
        let opened = if source + 1 < self.editor_layout.leaf_count() {
            let here = std::mem::take(&mut self.editor);
            self.editor = self.editor_layout.refocus_to_dfs(here, source + 1);
            self.editor.open_preview(&path)
        } else {
            let mut group = EditorTabs::default();
            let opened = group.open_preview(&path);
            if opened.is_ok() {
                group.copy_view_position_from(&self.editor);
                let here = std::mem::replace(&mut self.editor, group);
                self.editor_layout
                    .split_active(here, editor_layout::SplitDir::Horizontal, true);
            }
            opened
        };
        if opened.is_ok() && self.editor.markdown_preview.is_none() {
            self.editor.toggle_markdown_preview();
        }
        if self.editor_layout.active_dfs_index() != source {
            let side = std::mem::take(&mut self.editor);
            self.editor = self.editor_layout.refocus_to_dfs(side, source);
        }
        // The side pane comes up at the source's place and with its text.
        self.md_scroll_synced = None;
        self.sync_markdown_side_previews();
        self.focus_pane(Pane::Editor);
        self.status = match opened {
            Ok(()) => format!("Previewing {} to the side", self.status_path(&path)),
            Err(e) => format!("Open Preview to the Side: {e}"),
        };
    }

    /// Start a mouse selection over a rendered ANSI log (#257).
    ///
    /// Mirrors `begin_preview_selection`: a log tab's `lines` is a one-line
    /// stub, so the editor's own selection coordinates cannot describe it and
    /// the view carries its own, in absolute (line, char column).
    pub(super) fn begin_log_selection(&mut self, col: u16, row: u16) -> bool {
        let scroll = self.editor.scroll;
        let Some(log) = self.editor.log.as_mut() else {
            return false;
        };
        if !rect_contains(log.last_body, col, row) {
            if log.has_selection() {
                log.selection = None;
                log.dragging = false;
            }
            return false;
        }
        let cell = log_cell_at(log, scroll, col, row);
        log.selection = Some((cell, cell));
        log.dragging = true;
        self.focus_pane(Pane::Editor);
        true
    }

    /// Ctrl+click a `path:line` printed in a RENDERED log (#257).
    ///
    /// Scanned against the escape-free text, not the raw bytes: the
    /// reference's column in the file is not its column on screen once SGR
    /// sequences are stripped, and a path can sit inside an escape entirely.
    /// `log_cell_at` already maps a screen cell to that text's column.
    ///
    /// Dispatched ahead of `begin_log_selection`, which runs before the
    /// editor's own Ctrl handling and would otherwise swallow the gesture
    /// into a selection drag. Relative paths resolve against the workspace
    /// root - a log has no shell cwd to consult, unlike a terminal pane.
    pub(super) fn log_file_click(&mut self, col: u16, row: u16) -> bool {
        let scroll = self.editor.scroll;
        let Some(log) = self.editor.log.as_ref() else {
            return false;
        };
        if !rect_contains(log.last_body, col, row) {
            return false;
        }
        let (line, column) = log_cell_at(log, scroll, col, row);
        let Some(text) = log.visible_text(line).map(str::to_string) else {
            return false;
        };
        let Some(fr) = crate::file_ref::file_ref_at(&text, column) else {
            return false;
        };
        let raw = std::path::PathBuf::from(&fr.path);
        let abs = if let Some(rest) = fr.path.strip_prefix("~/") {
            match std::env::var_os("HOME") {
                Some(h) => std::path::PathBuf::from(h).join(rest),
                None => return false,
            }
        } else if raw.is_absolute() {
            raw
        } else {
            self.tree.root.join(raw)
        };
        // Only when it resolves to a real file, which is also what filters
        // lookalikes like `host.com:443`.
        if !abs.is_file() {
            return false;
        }
        self.open_resolved_file_ref(&abs, fr.line, fr.column, fr.end_line)
    }

    /// Extend a live log drag and finish it on release.
    pub(super) fn update_log_selection(&mut self, m: MouseEvent) -> bool {
        let scroll = self.editor.scroll;
        let copy_on_select = self.copy_on_select;
        let Some(log) = self.editor.log.as_mut() else {
            return false;
        };
        if !log.dragging {
            return false;
        }
        match m.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                let body = log.last_body;
                // Clamp to the body: dragging past an edge extends to it
                // rather than dropping the gesture.
                let col = m
                    .column
                    .clamp(body.x, body.x + body.width.saturating_sub(1));
                let row = m.row.clamp(body.y, body.y + body.height.saturating_sub(1));
                let head = log_cell_at(log, scroll, col, row);
                if let Some((anchor, _)) = log.selection {
                    log.selection = Some((anchor, head));
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                log.dragging = false;
                if copy_on_select {
                    self.copy_log_selection();
                }
                true
            }
            _ => false,
        }
    }

    /// Put the log selection on the clipboard, saying so when the copy was
    /// clamped rather than leaving the user with silently less than they
    /// selected.
    pub(super) fn copy_log_selection(&mut self) -> bool {
        let Some(log) = self.editor.log.as_ref() else {
            return false;
        };
        if !log.has_selection() {
            return false;
        }
        let (text, clamped) = log.selection_text();
        if text.is_empty() {
            return false;
        }
        let bytes = text.len();
        copy_to_clipboard(&text);
        // Say what was actually copied. `clamped` is true for the byte CAP
        // and for a sweep that ran out of budget on escape-heavy text, and in
        // the second case the copy can be far short of the cap: naming the
        // cap then tells the user they got four megabytes when they got a
        // fraction of one.
        self.status = if clamped {
            format!("Copied {bytes} bytes; the selection was larger than one copy allows")
        } else {
            format!("Copied {bytes} bytes")
        };
        true
    }

    /// Start a drag-selection in the rendered Markdown/document preview
    /// when `(col, row)` lands inside it. Returns true when the press was
    /// consumed, so the normal editor click path is skipped: the source
    /// buffer is not visible, and a caret there would serve nobody.
    pub(super) fn begin_preview_selection(&mut self, col: u16, row: u16) -> bool {
        if let Some(idx) = self.preview_runnable_at(col, row) {
            self.run_markdown_block(idx);
            return true;
        }
        let Some(md) = self.editor.markdown_preview.as_mut() else {
            return false;
        };
        if !rect_contains(md.last_area, col, row) {
            // A press elsewhere drops the selection, like the editor's.
            if md.has_selection() {
                md.selection = None;
                md.dragging = false;
            }
            return false;
        }
        let cell = (row - md.last_area.y, col - md.last_area.x);
        md.selection = Some((cell, cell));
        md.dragging = true;
        self.focus_pane(Pane::Editor);
        true
    }

    /// The runnable fence whose play glyph sits under screen `(col, row)`
    /// (#353): the glyph cell itself, the first column of the block's first
    /// visual row - one column, so a drag-selection starting beside it is
    /// still a selection.
    pub(super) fn preview_runnable_at(&self, col: u16, row: u16) -> Option<usize> {
        let md = self.editor.markdown_preview.as_ref()?;
        if !rect_contains(md.last_area, col, row) || col != md.last_area.x {
            return None;
        }
        let visual = (row - md.last_area.y) as usize + md.scroll as usize;
        md.run_rows.iter().position(|r| *r == visual)
    }

    /// Follow link `idx` of the open preview (#931), as VS Code's preview
    /// does: `#heading` scrolls to it, `http(s)` opens in the browser, and a
    /// relative path opens in the editor (`#L3` at that line; a Markdown
    /// file rendered, at its `#heading`). Any other scheme is refused.
    pub(super) fn follow_preview_link(&mut self, idx: usize) {
        let Some(dest) = (self.editor.markdown_preview.as_ref())
            .and_then(|md| Some(md.links.links.get(idx)?.dest.trim().to_string()))
        else {
            return;
        };
        if let Some(frag) = dest.strip_prefix('#') {
            let md = self.editor.markdown_preview.as_mut();
            self.status = if md.is_some_and(|md| md.scroll_to_heading(frag)) {
                format!("Went to {dest}")
            } else {
                format!("No heading {dest} in this document")
            };
            return;
        }
        let lower = dest.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            return self.open_url_for_user(&dest);
        }
        if dest.contains(':') {
            self.status = format!("Refused to open non-web link: {dest}");
            return;
        }
        let (path, frag) = dest
            .split_once('#')
            .map_or((dest.as_str(), None), |(p, f)| (p, Some(f)));
        // A query is no part of the file's name: GitHub READMEs link to
        // `guide.md?plain=1` for its source, as images add `?raw=true`.
        let path = path.split_once('?').map_or(path, |(p, _)| p);
        let path = crate::shell_integration::percent_decode(path.as_bytes());
        let path = String::from_utf8_lossy(&path);
        // `/docs/x.md` is from the workspace root, as GitHub reads it.
        let doc_dir = self.editor.path.as_ref().and_then(|p| p.parent());
        let base = doc_dir
            .filter(|_| !path.starts_with('/'))
            .map_or_else(|| self.active_workspace_root(), Path::to_path_buf);
        let abs = base.join(path.trim_start_matches('/'));
        let lines = frag.and_then(|f| {
            let mut n = f
                .split('-')
                .map(|n| n.strip_prefix('L')?.parse::<u32>().ok());
            Some((n.next()??, n.next().flatten()))
        });
        let (line, end) = lines.unwrap_or((1, None));
        if !self.open_resolved_file_ref(&abs, line, None, end) {
            self.status = format!("No file at {dest}");
            return;
        }
        if lines.is_none()
            && (self.editor.markdown_preview.is_some() || self.editor.toggle_markdown_preview())
            && let Some(frag) = frag
            && let Some(md) = self.editor.markdown_preview.as_mut()
        {
            md.scroll_to_heading(frag);
        }
    }

    /// Extend a live preview drag and finish it on release. Returns true
    /// when the event was consumed.
    pub(super) fn update_preview_selection(&mut self, m: MouseEvent) -> bool {
        let Some(md) = self.editor.markdown_preview.as_mut() else {
            return false;
        };
        if !md.dragging {
            return false;
        }
        match m.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                // Clamp to the view: dragging past an edge extends to it
                // rather than dropping the gesture.
                let area = md.last_area;
                let row = m.row.clamp(area.y, area.y + area.height.saturating_sub(1)) - area.y;
                let col = m
                    .column
                    .clamp(area.x, area.x + area.width.saturating_sub(1))
                    - area.x;
                if let Some((anchor, _)) = md.selection {
                    md.selection = Some((anchor, (row, col)));
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                md.dragging = false;
                // A click or Ctrl+click (#1848) that did not drag follows
                // the link under it (#931); a drag from a link selects.
                if let Some(i) = md.link_at(m.column, m.row).filter(|_| !md.has_selection()) {
                    self.follow_preview_link(i);
                    return true;
                }
                // Mirror the editor's copy-on-select setting so a finished
                // drag lands on the clipboard when the user asked for that.
                if self.copy_on_select {
                    let text = self
                        .editor
                        .markdown_preview
                        .as_ref()
                        .map(|md| md.selection_text())
                        .unwrap_or_default();
                    if !text.is_empty() {
                        copy_to_clipboard(&text);
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Fold in the background index of an open rendered log (#394).
    ///
    /// Returns whether a repaint is due: the header's line count and the
    /// scroll range moved. When the pass finishes, an open find bar has its
    /// count re-run, since the count it shows was taken against a partial
    /// index and flagged as such; a step the user makes next reaches the
    /// tail on its own.
    pub fn poll_log_index(&mut self) -> bool {
        let Some(log) = self.editor.log.as_mut() else {
            return false;
        };
        if !log.indexing() {
            return false;
        }
        let changed = log.poll_index();
        let indexing = log.indexing();
        // A reload's kept place may lie past what was indexed so far.
        if changed {
            self.editor.settle_log_reload();
        }
        if indexing {
            return changed;
        }
        let Some(log) = self.editor.log.as_ref() else {
            return true;
        };
        if let Some(state) = self.editor_find.as_ref()
            && !state.query.is_empty()
        {
            let (count, truncated) = log.count_matches(&state.query, state.opts);
            if let Some(state) = self.editor_find.as_mut() {
                state.set_match_count(count, truncated);
            }
        }
        true
    }

    /// Scroll sync between split panes (#619): when the focused pane shows a
    /// Markdown file and another pane shows the same file the other way
    /// (source beside preview), the other one follows. Returns true when it
    /// moved anything.
    pub fn sync_markdown_scroll(&mut self) -> bool {
        let active = &*self.editor;
        let Some(path) = active.path.clone() else {
            self.md_scroll_synced = None;
            return false;
        };
        // What the focused pane says the position is, in source lines.
        let (leads_preview, line) = match active.markdown_preview.as_ref() {
            Some(md) => match md.source_line_at_top() {
                Some(line) => (true, line),
                None => return false,
            },
            None => (false, active.scroll),
        };
        let key = (path.clone(), leads_preview, line);
        if self.md_scroll_synced.as_ref() == Some(&key) {
            return false;
        }
        let mut moved = false;
        for group in self.editor_layout.inactive_groups_mut() {
            let idx = group.active_index();
            let Some(tab) = group.editors.get_mut(idx) else {
                continue;
            };
            if tab.path.as_deref() != Some(path.as_path()) {
                continue;
            }
            match (leads_preview, tab.markdown_preview.as_mut()) {
                (false, Some(md)) if md.source_map.len() > 1 => {
                    md.scroll_to_source = Some(line);
                    moved = true;
                }
                (true, None) => {
                    tab.scroll = line.min(tab.lines.len().saturating_sub(1));
                    moved = true;
                }
                _ => {}
            }
        }
        self.md_scroll_synced = Some(key);
        moved
    }

    /// Open Preview to the Side (#1605): a split's Markdown preview renders
    /// the text being typed in the source pane beside it. Each pane's tab is
    /// a buffer of its own, and nobody types into the preview's, so it showed
    /// the file as last loaded. The panes pair as in
    /// [`App::sync_markdown_scroll`], the focused pane's tab the preferred
    /// source; a preview left with no source beside it shows its own buffer
    /// again. The preview's buffer is never touched, so it stays clean.
    /// Returns true when a preview was rebuilt.
    pub fn sync_markdown_side_previews(&mut self) -> bool {
        let mut tabs: Vec<&mut crate::widgets::editor::Editor> = std::iter::once(&mut self.editor)
            .chain(self.editor_layout.inactive_groups_mut())
            .filter_map(|group| {
                let idx = group.active_index();
                group.editors.get_mut(idx)
            })
            .collect();
        let mut changed = false;
        for p in 0..tabs.len() {
            let follows = match tabs[p].markdown_preview.as_ref() {
                Some(md) if !md.notebook && md.doc_path.is_none() => md.follows,
                _ => continue,
            };
            let source = (0..tabs.len()).find(|&s| {
                s != p
                    && tabs[s].path.is_some()
                    && tabs[s].path == tabs[p].path
                    && tabs[s].markdown_preview.is_none()
                    && !tabs[s].has_non_text_view()
            });
            let wanted = source.map(|s| (tabs[s].edit_seq, tabs[s].undo_step_id));
            let built = tabs[p]
                .markdown_preview
                .as_ref()
                .is_some_and(|md| md.built_seq == tabs[p].edit_seq);
            // A preview of its own buffer is rebuilt by its render, as ever.
            if wanted == follows && (built || wanted.is_none()) {
                continue;
            }
            match source {
                Some(s) => {
                    let text = tabs[s].lines.join("\n");
                    tabs[p].rebuild_markdown_preview_from(&text, wanted);
                }
                None => tabs[p].rebuild_markdown_preview(),
            }
            changed = true;
        }
        changed
    }

    /// Palette "Log: Toggle Highlighting (tailspin)" and the Settings row
    /// (#466): flip tailspin colouring for every open rendered log, steer
    /// the default for logs opened later, and persist the preference.
    pub fn toggle_log_highlight(&mut self) {
        self.set_log_highlight(!self.log_highlight);
        self.status = if self.log_highlight {
            String::from("Log highlighting (tailspin): on")
        } else {
            String::from("Log highlighting (tailspin): off")
        };
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_disable_log_highlight(
                !self.log_highlight,
            ));
        }
    }

    /// Apply a log-highlight setting everywhere it lives (#466): the app's
    /// field, the log view's opening default for logs opened later, and
    /// every log view and plain `.log` text tab already open in any split
    /// group. Shared by the palette / Settings toggle and by a settings
    /// remerge (a workspace layer setting `disable_log_highlight`), so the
    /// two cannot drift.
    pub(super) fn set_log_highlight(&mut self, on: bool) {
        self.log_highlight = on;
        seed_log_highlight_default(on);
        for group in
            std::iter::once(&mut self.editor).chain(self.editor_layout.inactive_groups_mut())
        {
            for ed in &mut group.editors {
                if let Some(log) = ed.log.as_mut() {
                    log.set_highlight(on);
                }
                // A plain `.log` open as text wears the same colours (#1537).
                ed.refresh_log_highlight();
            }
        }
    }

    /// Step the focused PDF preview by `delta` pages, wrapping at the
    /// document ends. The overlay re-bakes on its own (the page is part of
    /// its re-emit key). Deliberate gestures only - arrows, menu; the wheel
    /// goes through [`Self::wheel_pdf_page`].
    pub(super) fn step_pdf_page(&mut self, delta: i32) {
        self.editor.change_pdf_page(delta);
    }

    /// Wheel paging for the PDF preview. Differs from the key path twice
    /// over: it clamps at the document ends (no scroll wheel wraps a PDF),
    /// and a same-direction repeat inside the cooldown is dropped - every
    /// page step is a synchronous rasteriser run on the UI thread, so a
    /// momentum flick's burst of wheel events must coalesce instead of
    /// freezing the TUI for one render per event. A reversal is deliberate
    /// and always passes.
    pub(super) fn wheel_pdf_page(&mut self, delta: i32) {
        let doc = self.editor.path.clone();
        if pdf_wheel_cooling(self.last_pdf_wheel.as_ref(), delta, &doc) {
            return;
        }
        let Some(cur) = self.editor.pdf_page() else {
            return;
        };
        let target = if delta >= 0 {
            cur.saturating_add(delta as u32)
        } else {
            cur.saturating_sub(delta.unsigned_abs()).max(1)
        };
        self.editor.set_pdf_page(target);
        // Stamped AFTER the rasterise: the flick's remaining events queued
        // while the render ran, so a stamp taken at entry had already aged
        // by the render's duration when they were processed — a render
        // slower than the cooldown let every queued event through, one
        // blocking render each.
        self.last_pdf_wheel = Some((std::time::Instant::now(), delta.signum(), doc));
    }

    pub(super) fn jump_pdf_page(&mut self, page: u32) {
        self.editor.set_pdf_page(page);
    }

    /// Resolve the PDF link under a click at screen cell (`col`, `row`) on
    /// the focused PDF preview, extracting the current page's link regions
    /// on first use (and caching them on the tab until the page changes).
    pub(super) fn pdf_link_target_at(
        &mut self,
        col: u16,
        row: u16,
    ) -> Option<crate::pdf::LinkTarget> {
        let side = self.focused_image_side();
        let (cw_px, ch_px) = self.cell_pixel?;
        let frac = {
            let image = self.editor.image.as_ref()?;
            let pdf = image.pdf.as_ref()?;
            let layout = self.overlays.editor[side].layout()?;
            // A stale overlay (different tab, or a page flip the next frame
            // hasn't baked yet) must not hit-test the wrong page's geometry.
            if layout.path != pdf.source_path || layout.page != pdf.current_page {
                return None;
            }
            crate::iterm2_inline::cell_source_fraction(
                (
                    col.checked_sub(layout.cell_x)?,
                    row.checked_sub(layout.cell_y)?,
                ),
                (cw_px, ch_px),
                (layout.cell_w, layout.cell_h),
                (image.pixel_w, image.pixel_h),
            )?
        };
        let pdf = self.editor.image.as_mut()?.pdf.as_mut()?;
        if pdf.links.is_none() {
            match crate::pdf::page_links(&pdf.source_path, pdf.current_page) {
                Ok(links) => pdf.links = Some(links),
                // Not cached: a mid-write extraction failure (pdflatex
                // rebuilding the file) heals on the next click.
                Err(e) => {
                    self.status = format!("PDF links unavailable: {e}");
                    return None;
                }
            }
        }
        Some(pdf.links.as_ref()?.link_at(frac)?.target.clone())
    }

    pub(super) fn focused_image_side(&self) -> usize {
        // There are physically two overlay slots (left/right column), and
        // only the two-column split keys them by group: every other layout -
        // unsplit, or a grid of three or more groups - bakes the active
        // group into slot 0 (see the render path). Indexing by DFS position
        // here panicked on a PDF click in the third group of a grid.
        if self.editor_layout.leaf_count() == 2 {
            self.editor_layout.active_dfs_index()
        } else {
            0
        }
    }

    /// Sheet: Sort by Column (#578): sort the open CSV/TSV sheet's rows by
    /// the cursor's column, reversing when it is already sorted that way.
    /// A reorder is an edit, as in a spreadsheet: it marks the sheet
    /// unsaved. xlsx is left out, since its edits are written back by
    /// row, and so are the read-only kinds.
    pub(super) fn sort_sheet_by_column(&mut self) {
        let Some(sheet) = self.editor.sheet.as_mut() else {
            self.status = String::from("Open a CSV or TSV file to sort it");
            return;
        };
        if !matches!(
            sheet.kind,
            crate::sheet::SheetKind::Csv | crate::sheet::SheetKind::Tsv
        ) {
            self.status = String::from("Sorting is for CSV and TSV sheets");
            return;
        }
        let current = sheet.current_sheet;
        let Some(data) = sheet.sheets.get_mut(current) else {
            return;
        };
        let col = data.cur_col;
        let name = data
            .headers
            .get(col)
            .filter(|h| !h.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("column {}", col + 1));
        let ascending = data.sort_by_column(col);
        sheet.dirty = true;
        self.editor.dirty = true;
        self.status = format!(
            "Sorted by {name}, {}",
            if ascending { "ascending" } else { "descending" }
        );
    }

    /// Keyboard navigation for spreadsheet preview tabs. All gestures are
    /// scroll-only: arrows pan one row/column, PageUp/PageDown jump by a
    /// full viewport, Home/End jump to the first/last row, Tab/Shift+Tab
    /// switch worksheets. Anything else is swallowed so a stray keystroke
    /// can't insert characters into a buffer the user can't see.
    pub(super) fn handle_sheet_key(&mut self, key: KeyEvent) {
        let visible = sheet_visible_rows(self.editor.last_inner);
        let Some(sheet) = self.editor.sheet.as_mut() else {
            return;
        };
        // Cell editing covers CSV/TSV and (via the umya write path,
        // #178) xlsx; the legacy binary kinds stay read-only.
        let editable = matches!(
            sheet.kind,
            crate::sheet::SheetKind::Csv
                | crate::sheet::SheetKind::Tsv
                | crate::sheet::SheetKind::Xlsx
        );
        let total_sheets = sheet.sheets.len();
        let current = sheet.current_sheet;
        let plain = !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::SUPER)
            && !key.modifiers.contains(KeyModifiers::ALT);
        // In-grid cell editor (#177): it owns every key while open.
        if sheet.editing.is_some() {
            let (commit, next) = {
                let edit = sheet.editing.as_mut().expect("checked");
                match key.code {
                    KeyCode::Esc => {
                        sheet.editing = None;
                        return;
                    }
                    KeyCode::Enter => (true, (1i64, 0i64)),
                    KeyCode::Tab => (true, (0, 1)),
                    KeyCode::BackTab => (true, (0, -1)),
                    KeyCode::Backspace => {
                        if edit.cursor > 0 {
                            let i = floor_char(&edit.value, edit.cursor - 1);
                            edit.value.remove(i);
                            edit.cursor = i;
                        }
                        (false, (0, 0))
                    }
                    KeyCode::Delete => {
                        if edit.cursor < edit.value.len() {
                            edit.value.remove(edit.cursor);
                        }
                        (false, (0, 0))
                    }
                    KeyCode::Left => {
                        if edit.cursor > 0 {
                            edit.cursor = floor_char(&edit.value, edit.cursor - 1);
                        }
                        (false, (0, 0))
                    }
                    KeyCode::Right => {
                        if edit.cursor < edit.value.len() {
                            edit.cursor = ceil_char(&edit.value, edit.cursor + 1);
                        }
                        (false, (0, 0))
                    }
                    KeyCode::Home => {
                        edit.cursor = 0;
                        (false, (0, 0))
                    }
                    KeyCode::End => {
                        edit.cursor = edit.value.len();
                        (false, (0, 0))
                    }
                    KeyCode::Char(c) if plain => {
                        edit.value.insert(edit.cursor, c);
                        edit.cursor += c.len_utf8();
                        (false, (0, 0))
                    }
                    _ => (false, (0, 0)),
                }
            };
            if commit {
                if !editable {
                    // Belt-and-braces (#201 review): editing should never
                    // start on a read-only kind, but a commit must not
                    // mutate one either.
                    sheet.editing = None;
                    self.status = String::from("This view is read-only");
                    return;
                }
                let (r, c) = sheet.commit_edit().expect("checked");
                self.editor.dirty = true;
                let data = &mut self.editor.sheet.as_mut().expect("checked").sheets[current];
                data.cur_row = r
                    .saturating_add_signed(next.0 as isize)
                    .min(data.row_count().saturating_sub(1));
                data.cur_col = c
                    .saturating_add_signed(next.1 as isize)
                    .min(data.col_count().saturating_sub(1));
                sheet_follow_cursor(
                    self.editor.sheet.as_mut().expect("checked"),
                    current,
                    visible,
                );
            }
            return;
        }
        let data = match sheet.sheets.get_mut(current) {
            Some(d) => d,
            None => return,
        };
        let row_count = data.rows.len();
        let col_count = data.col_widths.len();
        match key.code {
            KeyCode::Down => {
                data.cur_row = (data.cur_row + 1).min(row_count.saturating_sub(1));
            }
            KeyCode::Up => data.cur_row = data.cur_row.saturating_sub(1),
            KeyCode::Right => {
                data.cur_col = (data.cur_col + 1).min(col_count.saturating_sub(1));
            }
            KeyCode::Left => data.cur_col = data.cur_col.saturating_sub(1),
            KeyCode::PageDown => {
                // SQLite (#201 review): at the bottom of the loaded
                // batch, fetch the NEXT page instead of stopping.
                if sheet.kind == crate::sheet::SheetKind::Sqlite
                    && data.cur_row + visible.max(1) >= row_count
                {
                    self.sqlite_page_step(1);
                    return;
                }
                data.cur_row = (data.cur_row + visible).min(row_count.saturating_sub(1));
            }
            KeyCode::PageUp => {
                if sheet.kind == crate::sheet::SheetKind::Sqlite
                    && data.cur_row == 0
                    && data.scroll_row == 0
                {
                    self.sqlite_page_step(-1);
                    return;
                }
                data.cur_row = data.cur_row.saturating_sub(visible);
            }
            KeyCode::Home | KeyCode::End
                if sheet.kind == crate::sheet::SheetKind::Sqlite
                    && (key.modifiers.contains(KeyModifiers::CONTROL)
                        || key.modifiers.contains(KeyModifiers::SUPER)) =>
            {
                // The table's first or last row, not the loaded batch's
                // (#1222).
                self.sqlite_jump(key.code == KeyCode::End, visible);
                return;
            }
            KeyCode::Home => {
                data.cur_col = 0;
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::SUPER)
                {
                    data.cur_row = 0;
                }
            }
            KeyCode::End => {
                data.cur_col = col_count.saturating_sub(1);
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::SUPER)
                {
                    data.cur_row = row_count.saturating_sub(1);
                }
            }
            KeyCode::Enter | KeyCode::F(2) => {
                if editable {
                    if row_count > 0 {
                        let value = data.cell(data.cur_row, data.cur_col).to_string();
                        let cursor = value.len();
                        sheet.editing = Some(crate::sheet::CellEdit { value, cursor });
                    }
                } else {
                    self.status = String::from("This workbook format is read-only (xls/ods/xlsb)");
                }
                return;
            }
            KeyCode::Delete if !editable => {
                self.status = String::from("This view is read-only");
                return;
            }
            KeyCode::Delete if editable && row_count > 0 => {
                let (r, c) = (data.cur_row, data.cur_col);
                data.set_cell(r, c, String::new());
                if !sheet.cell_edits.contains(&(current, r, c)) {
                    sheet.cell_edits.push((current, r, c));
                }
                sheet.dirty = true;
                self.editor.dirty = true;
                return;
            }
            KeyCode::Tab if total_sheets > 1 => {
                sheet.current_sheet = (current + 1) % total_sheets;
                return;
            }
            KeyCode::BackTab if total_sheets > 1 => {
                sheet.current_sheet = (current + total_sheets - 1) % total_sheets;
                return;
            }
            // Any printable character starts the edit, not just ASCII:
            // `Ö`, `東` or an emoji typed first must not be dropped (#963).
            KeyCode::Char(c) if plain && !c.is_control() => {
                if editable {
                    if row_count > 0 {
                        // Spreadsheet convention: typing REPLACES the cell.
                        sheet.editing = Some(crate::sheet::CellEdit {
                            value: String::from(c),
                            cursor: c.len_utf8(),
                        });
                    }
                } else {
                    self.status = String::from("This workbook format is read-only (xls/ods/xlsb)");
                }
                return;
            }
            _ => return,
        }
        sheet_follow_cursor(sheet, current, visible);
    }

    /// Rendered log keys (#1189): the arrows move a line, PageUp/PageDown a
    /// screen, Home/End (with or without Ctrl) go to the top and the tail.
    /// The tab's text side is a one-line stub, so every other key is
    /// swallowed: reaching the editor, it pinned the view to line 1 and
    /// typing marked the read-only tab modified. Copy, Esc and the
    /// tab-number jumps are left to the editor (false), which handles them
    /// for a log.
    pub(super) fn handle_log_key(&mut self, key: KeyEvent) -> bool {
        if is_editor_copy_key(key) || jump_to_tab_index(key).is_some() || key.code == KeyCode::Esc {
            return false;
        }
        let page = self.editor.visible_rows().saturating_sub(2).max(1);
        match key.code {
            KeyCode::Down => self.editor.scroll_down(1),
            KeyCode::Up => self.editor.scroll_up(1),
            KeyCode::PageDown => self.editor.scroll_down(page),
            KeyCode::PageUp => self.editor.scroll_up(page),
            KeyCode::End => self.editor.scroll_down(usize::MAX / 2),
            KeyCode::Home => self.editor.scroll_up(usize::MAX / 2),
            _ => {}
        }
        true
    }

    /// Archive browser keys (#179): selection movement, Enter extracts
    /// the member to scratch and opens it through the normal dispatch,
    /// E prompts for an extraction folder.
    pub(super) fn handle_archive_key(&mut self, key: KeyEvent) {
        let Some(view) = self.editor.archive.as_mut() else {
            return;
        };
        let rows = (view.rows_visible as usize).max(1);
        let last = view.entries.len().saturating_sub(1);
        match key.code {
            KeyCode::Down => view.selected = (view.selected + 1).min(last),
            KeyCode::Up => view.selected = view.selected.saturating_sub(1),
            KeyCode::PageDown => view.selected = (view.selected + rows).min(last),
            KeyCode::PageUp => view.selected = view.selected.saturating_sub(rows),
            KeyCode::Home => view.selected = 0,
            KeyCode::End => view.selected = last,
            KeyCode::Enter => self.open_selected_archive_member(),
            KeyCode::Char('e') | KeyCode::Char('E') => self.open_archive_extract_prompt(),
            _ => {}
        }
    }

    /// Extract the selected member into the session scratch dir and open
    /// the extracted FILE through the standard dispatch, so text, images,
    /// PDFs, sheets, and binaries all render with their real viewers.
    pub(super) fn open_selected_archive_member(&mut self) {
        let Some((path, kind, member, dir)) = self.editor.archive.as_ref().and_then(|v| {
            let e = v.entries.get(v.selected)?;
            Some((self.editor.path.clone()?, v.kind, e.path.clone(), e.dir))
        }) else {
            return;
        };
        if dir {
            self.status = String::from("Directories only group members; open a file");
            return;
        }
        // Session-unique (#198 review): a shared path let another croft
        // instance overwrite a member between extraction and open.
        let dest = std::env::temp_dir().join(format!("croft-archive-{}", std::process::id()));
        match crate::archive::extract_member(&path, kind, &member, &dest) {
            Ok(extracted) => {
                if let Err(e) = self.open_at(&extracted, 0, 0) {
                    self.status = format!("Member open failed: {e}");
                } else {
                    self.status = format!("Opened {member} (extracted copy)");
                }
            }
            Err(e) => self.status = format!("Member open failed: {e}"),
        }
    }

    pub(super) fn open_archive_extract_prompt(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let Some((member, dir)) = self
            .editor
            .archive
            .as_ref()
            .and_then(|v| v.entries.get(v.selected))
            .map(|e| (e.path.clone(), e.dir))
        else {
            return;
        };
        if dir {
            // extract_member extracts ONE member, not a subtree (#198
            // review): the prompt would fail or write an empty folder.
            self.status = String::from("Directories only group members; extract a file");
            return;
        }
        let suggestion = self.workspace_root().display().to_string();
        self.open_input_prompt(
            InputPrompt::new(
                InputPurpose::ArchiveExtract { member },
                String::from("Extract Member To"),
                "destination folder",
            )
            .with_value(&suggestion),
        );
    }

    /// Hex-tab navigation (#172): cursor movement over bytes, Shift
    /// extending the selection, F3 repeating the last find. The viewport
    /// row count comes from the last painted frame's layout (frame
    /// truth), so PageUp/PageDown match what the user sees.
    pub(super) fn handle_hex_key(&mut self, key: KeyEvent) {
        // Cmd/Ctrl+F never reaches here — `is_editor_find_key` routes to
        // `open_editor_find` earlier in the dispatch, which forwards hex
        // tabs to the byte-query popup.
        let Some(view) = self.editor.hex.as_mut() else {
            return;
        };
        let rows = (view.layout.data_rows as usize).max(1);
        let bpr = view.bytes_per_row as i64;
        let select = key.modifiers.contains(KeyModifiers::SHIFT);
        let jump = key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::SUPER);
        // Editing (#173): undo/redo chords, then plain typing. The
        // chord check comes first so Cmd+Z is never read as the letter.
        if jump && matches!(key.code, KeyCode::Char('z') | KeyCode::Char('Z')) {
            let did = if select {
                view.redo_edit()
            } else {
                view.undo_edit()
            };
            if did {
                self.editor.dirty = self.editor.hex.as_ref().is_some_and(|v| v.has_edits());
            } else {
                self.status = String::from(if select {
                    "Nothing to redo"
                } else {
                    "Nothing to undo"
                });
            }
            return;
        }
        if let KeyCode::Char(c) = key.code
            && !jump
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            if view.read_only {
                self.status = String::from("Read-only file: no write permission");
                return;
            }
            let mut wrote = false;
            if view.ascii_focus {
                if (' '..='~').contains(&c) {
                    let off = view.cursor;
                    view.apply_edit(off, c as u8);
                    view.move_cursor(1, false, rows);
                    wrote = true;
                }
            } else if let Some(d) = c.to_digit(16) {
                if view.type_hex_digit(d as u8) {
                    view.move_cursor(1, false, rows);
                }
                wrote = true;
            }
            if wrote {
                self.editor.dirty = self.editor.hex.as_ref().is_some_and(|v| v.has_edits());
                return;
            }
            // Not a typable char for this pane: fall through to the
            // navigation matches below (F3 etc. are KeyCode variants,
            // never Char, so nothing else claims chars).
            return;
        }
        match key.code {
            KeyCode::Tab => {
                view.ascii_focus = !view.ascii_focus;
                view.pending_nibble = None;
                return;
            }
            KeyCode::Backspace => {
                view.pending_nibble = None;
                view.move_cursor(-1, false, rows);
                return;
            }
            KeyCode::Delete => {
                let off = view.cursor;
                view.revert_edit(off);
                self.editor.dirty = self.editor.hex.as_ref().is_some_and(|v| v.has_edits());
                return;
            }
            _ => {}
        }
        let Some(view) = self.editor.hex.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Left => view.move_cursor(-1, select, rows),
            KeyCode::Right => view.move_cursor(1, select, rows),
            KeyCode::Up => view.move_cursor(-bpr, select, rows),
            KeyCode::Down => view.move_cursor(bpr, select, rows),
            KeyCode::PageUp => view.move_cursor(-bpr * rows as i64, select, rows),
            KeyCode::PageDown => view.move_cursor(bpr * rows as i64, select, rows),
            KeyCode::Home if jump => view.set_cursor(0, select, rows),
            KeyCode::End if jump => view.set_cursor(u64::MAX, select, rows),
            KeyCode::Home => {
                let row_start = view.cursor - view.cursor % view.bytes_per_row;
                view.set_cursor(row_start, select, rows);
            }
            KeyCode::End => {
                let row_end =
                    view.cursor - view.cursor % view.bytes_per_row + (view.bytes_per_row - 1);
                view.set_cursor(row_end, select, rows);
            }
            KeyCode::Esc => {
                view.sel_anchor = None;
                view.pending_nibble = None;
            }
            KeyCode::F(3) => self.hex_find_next(),
            _ => {}
        }
    }

    /// Sheet row/column structure ops (#177), palette-driven, anchored
    /// on the selected cell. CSV/TSV only; the workbook kinds are still
    /// read-only (#178).
    pub(super) fn sheet_structure_op(&mut self, cmd: crate::widgets::command_palette::Command) {
        use crate::widgets::command_palette::Command as Cmd;
        let Some(view) = self.editor.sheet.as_mut() else {
            self.status = String::from("The active tab is not a spreadsheet");
            return;
        };
        if !matches!(
            view.kind,
            crate::sheet::SheetKind::Csv | crate::sheet::SheetKind::Tsv
        ) {
            self.status = String::from(
                "Row/column structure edits are CSV/TSV only; xlsx supports cell edits",
            );
            return;
        }
        view.editing = None;
        let current = view.current_sheet;
        let Some(data) = view.sheets.get_mut(current) else {
            return;
        };
        let (r, c) = (data.cur_row, data.cur_col);
        let changed = match cmd {
            Cmd::SheetInsertRowBelow => {
                let at = if data.row_count() == 0 { 0 } else { r + 1 };
                data.insert_row(at);
                data.cur_row = at;
                true
            }
            Cmd::SheetDeleteRow => {
                let did = data.delete_row(r);
                data.cur_row = r.min(data.row_count().saturating_sub(1));
                did
            }
            Cmd::SheetInsertColRight => {
                let at = if data.col_count() == 0 { 0 } else { c + 1 };
                data.insert_col(at);
                data.cur_col = at;
                true
            }
            Cmd::SheetDeleteCol => {
                let did = data.delete_col(c);
                data.cur_col = c.min(data.col_count().saturating_sub(1));
                did
            }
            _ => false,
        };
        // Removing the only row that reached the last column shrinks the
        // sheet's width too, so the caret column can be past the end (#1137).
        let last_col = data.col_count().saturating_sub(1);
        data.cur_col = data.cur_col.min(last_col);
        data.scroll_col = data.scroll_col.min(data.cur_col);
        if changed {
            view.dirty = true;
            let visible = sheet_visible_rows(self.editor.last_inner);
            if let Some(view) = self.editor.sheet.as_mut() {
                sheet_follow_cursor(view, current, visible);
            }
            self.editor.dirty = true;
        } else {
            self.status = String::from("Nothing to remove there");
        }
    }

    /// Step the active SQLite table to an adjacent page (#201 review),
    /// replacing the sheet's rows and keeping the cursor sane.
    pub(super) fn sqlite_page_step(&mut self, delta: i64) {
        let Some(view) = self.editor.sheet.as_ref() else {
            return;
        };
        let idx = view.current_sheet;
        let Some(&(_, page)) = view.sqlite_pages.get(idx) else {
            return;
        };
        let next = page.saturating_add_signed(delta as isize);
        if delta > 0 {
            // Do not run past the end: only step when a further page can
            // exist per the loaded batch being full.
            if view.sheets[idx].row_count() < crate::sqlite_view::ROW_CAP {
                self.status = String::from("Last page");
                return;
            }
        } else if page == 0 {
            return;
        }
        self.sqlite_load_page(next, delta < 0);
    }

    /// Ctrl+Home / Ctrl+End in a SQLite table (#1222): the table's first or
    /// last row, loading its page, not the loaded batch's.
    pub(super) fn sqlite_jump(&mut self, to_end: bool, visible: usize) {
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(view) = self.editor.sheet.as_ref() else {
            return;
        };
        let idx = view.current_sheet;
        let Some((table, page)) = view.sqlite_pages.get(idx).cloned() else {
            return;
        };
        let target = if to_end {
            match crate::sqlite_view::last_page(&path, &table) {
                Ok(last) => last,
                Err(e) => {
                    self.status = format!("Page fetch failed: {e}");
                    return;
                }
            }
        } else {
            0
        };
        if target != page {
            self.sqlite_load_page(target, to_end);
        }
        if let Some(view) = self.editor.sheet.as_mut() {
            let data = &mut view.sheets[idx];
            (data.cur_row, data.cur_col) = if to_end {
                (
                    data.row_count().saturating_sub(1),
                    data.col_count().saturating_sub(1),
                )
            } else {
                (0, 0)
            };
            sheet_follow_cursor(view, idx, visible);
        }
    }

    /// Replace the current SQLite sheet with page `next` of its table, the
    /// cursor on its last row when `at_end`.
    pub(super) fn sqlite_load_page(&mut self, next: usize, at_end: bool) {
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(view) = self.editor.sheet.as_mut() else {
            return;
        };
        let idx = view.current_sheet;
        let Some((table, _)) = view.sqlite_pages.get(idx).cloned() else {
            return;
        };
        match crate::sqlite_view::table_page(&path, &table, next) {
            Ok(page) if !page.rows.is_empty() || next == 0 => {
                let mut data = page.into_sheet(&table, next);
                if at_end {
                    data.cur_row = data.row_count().saturating_sub(1);
                }
                view.sheets[idx] = data;
                view.sqlite_pages[idx].1 = next;
            }
            Ok(_) => self.status = String::from("Last page"),
            Err(e) => self.status = format!("Page fetch failed: {e}"),
        }
    }

    /// F3 / "Hex: Find Next": repeat the stored query from just past the
    /// cursor. The query itself is set by the find prompt
    /// (`InputPurpose::HexFind`), whose submit searches cursor-INCLUSIVE
    /// via `hex_find_run` directly.
    pub(super) fn hex_find_next(&mut self) {
        let Some(view) = self.editor.hex.as_ref() else {
            return;
        };
        if view.last_find.is_none() {
            self.status = String::from("Hex: no find query yet (Cmd+F)");
            return;
        }
        self.hex_find_run(view.cursor.saturating_add(1));
    }

    /// Run the hex tab's stored find query from `from` (wrapping),
    /// selecting the match and reporting the outcome in the status line.
    pub(super) fn hex_find_run(&mut self, from: u64) {
        let Some(view) = self.editor.hex.as_mut() else {
            return;
        };
        let Some(query) = view.last_find.clone() else {
            return;
        };
        let needle = &query.needle;
        // Which reading the query got (#1644): "1234" as bytes 12 34 and as
        // text are different searches, and the line says which one ran.
        let what = query.describe();
        let rows = (view.layout.data_rows as usize).max(1);
        match view.find_forward(needle, from) {
            Ok(crate::hex::FindOutcome::Found(off)) => {
                view.set_cursor(off, false, rows);
                view.sel_anchor = Some(off + needle.len() as u64 - 1);
                self.status = format!("Found {what} at 0x{off:X}");
            }
            Ok(crate::hex::FindOutcome::NotFound) => {
                self.status = format!("Not found: {what}");
            }
            Ok(crate::hex::FindOutcome::Capped) => {
                self.status = format!(
                    "Not found in the first {} MB scanned: {what}",
                    crate::hex::FIND_SCAN_CAP / (1024 * 1024)
                );
            }
            Err(e) => self.status = format!("Hex find failed: {e}"),
        }
    }
}

/// Map a screen cell to a sheet grid cell (#177) using the frame-truth
/// layout the render wrote: row from the vertical band, column by
/// walking the visible column widths from `scroll_col`.
pub(super) fn sheet_cell_at(
    view: &crate::sheet::SheetView,
    col: u16,
    row: u16,
) -> Option<(usize, usize)> {
    let g = view.grid;
    if g.data_rows == 0 || row < g.data_top || row >= g.data_top + g.data_rows {
        return None;
    }
    let data = view.sheets.get(view.current_sheet)?;
    let r = data.scroll_row + (row - g.data_top) as usize;
    if r >= data.row_count() {
        return None;
    }
    if col < g.body_x {
        return None;
    }
    crate::sheet::visible_columns(&data.col_widths, data.scroll_col, g.body_w)
        .into_iter()
        .find(|&(_, x_off, w)| col >= g.body_x + x_off && col < g.body_x + x_off + w)
        .map(|(c, _, _)| (r, c))
}

/// Char-boundary helpers for the in-grid cell editor (#177): byte
/// cursor stepped to valid boundaries.
pub(super) fn floor_char(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub(super) fn ceil_char(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Keep the selected cell visible: rows directly, columns by advancing
/// `scroll_col` until the cumulative widths place `cur_col` inside the
/// last painted body width (frame truth from the render).
pub(super) fn sheet_follow_cursor(view: &mut crate::sheet::SheetView, idx: usize, visible: usize) {
    let body_w = view.grid.body_w.max(8) as usize;
    let Some(data) = view.sheets.get_mut(idx) else {
        return;
    };
    if data.cur_row < data.scroll_row {
        data.scroll_row = data.cur_row;
    } else if visible > 0 && data.cur_row >= data.scroll_row + visible {
        data.scroll_row = data.cur_row + 1 - visible;
    }
    if data.cur_col < data.scroll_col {
        data.scroll_col = data.cur_col;
    } else {
        // Advance until the cursor column's right edge fits. `get`, since a
        // caller that shrank the sheet may not have clamped the caret yet.
        while data.scroll_col < data.cur_col {
            let Some(span) = data.col_widths.get(data.scroll_col..=data.cur_col) else {
                break;
            };
            let used: usize = span.iter().map(|&w| w as usize + 1).sum();
            if used <= body_w {
                break;
            }
            data.scroll_col += 1;
        }
    }
}

pub(super) fn sheet_visible_rows(inner: Rect) -> usize {
    // Renderer reserves 1 row for the header line, 1 row for the column
    // labels, and 1 row for the bottom status. Anything left is data.
    inner.height.saturating_sub(3) as usize
}

/// The absolute (line, char column) a screen cell lands on in a rendered log
/// body. The body starts one row below the header and the view scrolls by
/// whole lines, so the mapping is a straight offset.
///
/// A cell is mapped back to a character through the same `CellMap` the
/// renderer painted with (#404): a double-width character is two cells and
/// one column, and a click on either half is a click on that character.
/// The two must move together; a mapping that measured width differently
/// from the painter would disagree with what is actually on screen.
pub(super) fn log_cell_at(
    log: &crate::log_view::LogView,
    scroll: usize,
    col: u16,
    row: u16,
) -> (usize, usize) {
    let body = log.last_body;
    let line = (scroll + row.saturating_sub(body.y) as usize).min(log.len().saturating_sub(1));
    let cell = col.saturating_sub(body.x);
    // Clamp the COLUMN to the line's own length, not just the line to the
    // file's. The renderer paints to the endpoint it is given while the copy
    // reads `min(len)`, so an unclamped column let a drag past the end of a
    // short line paint sixty cells and copy three characters. Clamping here
    // means both readers see one value rather than each clamping its own way.
    // `char_at_cell` already answers the character count for a cell past
    // the end, which is that clamp.
    //
    // A line outside the parsed window has no text to map through; the raw
    // cell stands as the column, which is what the old behaviour was
    // everywhere.
    let column = log
        .visible_text(line)
        .map(|t| crate::cell_map::CellMap::new(t).char_at_cell(cell))
        .unwrap_or(cell as usize);
    (line, column)
}

/// Whether a PDF wheel step of `delta` in `doc` falls inside the cooldown of
/// the last one (`last`: when, which direction, which document): a
/// same-direction repeat in the same document within 150 ms is dropped so a
/// momentum flick costs one rasterise, not one per event. A reversal or
/// another document always passes.
pub(super) fn pdf_wheel_cooling(
    last: Option<&(std::time::Instant, i32, Option<PathBuf>)>,
    delta: i32,
    doc: &Option<PathBuf>,
) -> bool {
    const COOLDOWN: std::time::Duration = std::time::Duration::from_millis(150);
    last.is_some_and(|(at, dir, last_doc)| {
        *dir == delta.signum() && last_doc == doc && at.elapsed() < COOLDOWN
    })
}

/// Push a log-highlight setting into the log view's process-wide opening
/// default (#466), from the toggle and from a settings remerge.
pub(crate) fn seed_log_highlight_default(on: bool) {
    crate::log_view::set_default_highlight(on);
}

/// The startup seed of that default from the saved preference (#466). In
/// the shipped binary it is the same write; under test it RECORDS the value
/// per thread instead, because the suite builds hundreds of apps on
/// parallel threads and each write would race the tests that read the
/// process-wide default. The record keeps the call site observable: a test
/// asserts what `App::new` decided to seed, not merely that a setter sets.
#[cfg(not(test))]
pub(super) fn seed_log_highlight_default_at_startup(on: bool) {
    crate::log_view::set_default_highlight(on);
}

#[cfg(test)]
thread_local! {
    /// What the most recent `App::new` on this thread would have seeded.
    pub(crate) static STARTUP_LOG_HIGHLIGHT_SEED: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn seed_log_highlight_default_at_startup(on: bool) {
    STARTUP_LOG_HIGHLIGHT_SEED.with(|cell| cell.set(Some(on)));
}

/// A grid tab with unsaved edits as a session tab (#1593): its file, with
/// the edits [`Editor::sheet_backup`] keeps. `None` for any other tab.
pub(super) fn sheet_tab_state(ed: &Editor) -> Option<crate::session_state::OpenTabState> {
    let path = ed.path.clone()?;
    let (unsaved_text, sheet_cells) = ed.sheet_backup()?;
    Some(crate::session_state::OpenTabState {
        path: Some(path),
        cursor_row: 0,
        cursor_col: 0,
        scroll: 0,
        scroll_col: 0,
        dirty: true,
        unsaved_text,
        sheet_cells,
    })
}
