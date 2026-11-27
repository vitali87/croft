//! All mouse handling (`handle_mouse_inner`), hover popups and tooltips, and closing
//! stale popups on a click.

use super::*;
use crate::widgets::source_control::KeyStop;

impl App {
    /// [`Self::ui_tooltip_at`], plus the hints whose text is what sits under
    /// the pointer rather than a control's name: a Search result row shows
    /// its file's full path (#2008).
    pub(super) fn ui_tooltip_text_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<std::borrow::Cow<'static, str>> {
        self.search_result_hint_at(col, row)
            .map(Into::into)
            .or_else(|| self.ui_tooltip_at(col, row).map(Into::into))
    }

    /// The full path of the Search result row under `(col, row)` (#2008):
    /// a row names only the file.
    pub(super) fn search_result_hint_at(&self, col: u16, row: u16) -> Option<String> {
        if !(self.show_tree
            && self.sidebar_view == SidebarView::Search
            && rect_contains(self.search.last_area, col, row))
        {
            return None;
        }
        let path = self.search.result_path_at(col, row)?;
        Some(path.display().to_string())
    }

    /// The hover-tooltip label for whatever chrome control sits under the
    /// pointer, or `None` over inert space. The single dispatch point for
    /// button hints: each surface contributes one branch that reuses that
    /// surface's own hit-test, so adding a new tooltipped control is a
    /// one-line change here plus a label in the widget.
    pub(super) fn ui_tooltip_at(&self, col: u16, row: u16) -> Option<&'static str> {
        // Layout-control icons at the top-right of the editor strip / welcome.
        let icons = self.layout_icon_areas;
        if rect_contains(icons.toggle_side_bar, col, row) {
            return Some("Toggle Primary Side Bar");
        }
        if rect_contains(icons.toggle_panel, col, row) {
            return Some("Toggle Panel");
        }
        if rect_contains(icons.customize, col, row) {
            return Some("Customize Layout");
        }
        if self.editor.tab_list_button_at(col, row) {
            return Some("Show All Open Tabs");
        }
        // Activity bar: always visible, so it is checked regardless of which
        // sidebar view is open.
        if let Some(icon) = self.activity_icon_at(col, row) {
            return Some(icon.label());
        }
        // Sidebar header actions, dispatched by the view that owns them so a
        // panel's stored geometry is never consulted once another view has
        // replaced it on screen.
        match self.sidebar_view {
            SidebarView::Search => {
                if let Some(label) = self.search.tooltip_at(col, row) {
                    return Some(label);
                }
            }
            SidebarView::RunDebug if self.run_debug.debug_active => {
                if let Some(control) = self.run_debug.debug_control_painted_at(col, row) {
                    return Some(control.tooltip());
                }
            }
            SidebarView::Explorer => {
                let t = &self.tree;
                if rect_contains(t.header_new_file_btn, col, row) {
                    return Some("New File");
                }
                if rect_contains(t.header_new_folder_btn, col, row) {
                    return Some("New Folder");
                }
                if rect_contains(t.header_refresh_btn, col, row) {
                    return Some("Refresh Explorer");
                }
                if rect_contains(t.header_collapse_btn, col, row) {
                    return Some("Collapse Folders in Explorer");
                }
                if rect_contains(t.header_views_btn, col, row) {
                    return Some("Views and More Actions");
                }
                if self.open_editors.hit_save_all(col, row) {
                    return Some("Save All");
                }
            }
            SidebarView::Remote => {
                let r = &self.remote;
                if rect_contains(r.header_add_btn, col, row) {
                    return Some("Open SSH Configuration File");
                }
                if rect_contains(r.header_refresh_btn, col, row) {
                    return Some("Refresh");
                }
            }
            SidebarView::SourceControl => {
                if rect_contains(self.source_control.header_refresh_btn, col, row) {
                    return Some("Refresh");
                }
            }
            SidebarView::RunDebug => {}
            SidebarView::Extensions => {
                if let Some(label) = self.extensions_tooltip_at(col, row) {
                    return Some(label);
                }
            }
            SidebarView::Testing | SidebarView::CodeQL => {}
        }
        None
    }

    /// Resolve the hover tooltip for whatever the pointer is over in the
    /// Extensions panel: the toggle switch (state-aware enable/disable), the
    /// `+Add` affordance on AVAILABLE rows, or a row body (which conveys the
    /// extension's class and how to remove it). Catalog-aware so an installed
    /// catalog entry advertises Delete-to-uninstall while a hand-dropped user
    /// manifest is correctly described as disable-only.
    pub(super) fn extensions_tooltip_at(&self, col: u16, row: u16) -> Option<&'static str> {
        // A tooltip fires ONLY over an actual control, never over the row's
        // name/blurb — so hovering the body shows nothing, and each control
        // describes just itself. The trash button takes precedence over the
        // switch since it sits left of it (and is checked first regardless).
        if self.extensions.click_refresh(col, row) {
            return Some("Refresh the extension index");
        }
        if self.extensions.click_clear(col, row) {
            return Some("Clear the filter");
        }
        if self.extensions.click_uninstall(col, row) {
            return Some("Uninstall this extension (returns to Available)");
        }
        if !self.extensions.click_action(col, row) {
            return None;
        }
        let idx = self.extensions.row_at(row)?;
        let item = self.extensions.item_at(idx)?;
        Some(if item.available {
            "Add to your installed extensions"
        } else if item.enabled {
            "Enabled \u{2014} click to disable"
        } else {
            "Disabled \u{2014} click to enable"
        })
    }

    /// Read the hint again when the Search list changed under a resting
    /// pointer (#2008): rows move as hits stream in or the list scrolls by
    /// wheel, key or scrollbar, and empty when the search is cleared, so a
    /// hint naming the file that was there would mislabel the row there now.
    /// A changed hint waits out its own dwell, as one the pointer moved to.
    /// An empty row a hit fills under the pointer gets its hint too, but a
    /// control's hint that a click dismissed stays dismissed. Returns whether
    /// a showing hint was taken down.
    pub(super) fn reread_ui_tooltip(&mut self) -> bool {
        let shown = (
            self.search.scroll,
            self.search.hits.len(),
            self.show_tree && self.sidebar_view == SidebarView::Search,
        );
        if std::mem::replace(&mut self.search_hint_view, shown) == shown {
            return false;
        }
        let Some((col, row)) = self.pointer_cell else {
            return false;
        };
        let result = self.search_result_hint_at(col, row);
        let on_result = result.is_some();
        let label = result
            .map(Into::into)
            .or_else(|| self.ui_tooltip_at(col, row).map(Into::into));
        if label == self.ui_tooltip_label || (self.ui_tooltip_label.is_none() && !on_result) {
            return false;
        }
        self.ui_hover.clear();
        if label.is_some() {
            self.ui_hover.on_move(std::time::Instant::now(), col, row);
        }
        self.ui_tooltip_label = label;
        self.ui_tooltip.take().is_some()
    }

    /// Fire the chrome button-hint tooltip once its dwell crosses
    /// `HOVER_DELAY`. Returns `true` when a tooltip was created so the frame
    /// loop redraws. Suppressed while a context menu is open so the two never
    /// overlap.
    pub(super) fn poll_ui_tooltip(&mut self) -> bool {
        let taken_down = self.reread_ui_tooltip();
        if !self.ui_hover.due(std::time::Instant::now(), HOVER_DELAY) {
            return taken_down;
        }
        self.ui_hover.mark_fired();
        if self.context_menu.is_some() {
            return false;
        }
        let Some(label) = self.ui_tooltip_label.as_deref() else {
            return false;
        };
        let Some((col, row)) = self.ui_hover.cell() else {
            return false;
        };
        self.ui_tooltip = Some(crate::widgets::hover_popup::HoverPopup::new_compact(
            label.to_string(),
            (col, row),
        ));
        true
    }

    pub(super) fn on_mouse_moved(&mut self, col: u16, row: u16, in_editor: bool) {
        // Crossing onto the hover keeps it, so the wheel can reach it.
        if self
            .hover_popup
            .as_ref()
            .is_some_and(|p| rect_contains(p.last_area, col, row))
        {
            self.hover.clear();
            return;
        }
        // Activity-bar hover: brighten the icon under the pointer. Re-emit the
        // swapped pre-baked image only when the hovered icon actually changes,
        // so a pointer drifting within one icon (or anywhere else) costs
        // nothing. Computed before the tab/editor early-returns below so the
        // hover clears the moment the pointer leaves the bar.
        let new_hover = self.activity_icon_at(col, row);
        if self.hovered_activity_icon != new_hover {
            self.hovered_activity_icon = new_hover;
            self.overlays.activity.mark_dirty();
        }
        // Layout toolbar hover: same affordance, re-emit the brighter image
        // variant only when the hovered icon changes.
        let li = self.layout_icon_areas;
        let new_layout_hover = if rect_contains(li.toggle_side_bar, col, row) {
            Some(0)
        } else if rect_contains(li.toggle_panel, col, row) {
            Some(1)
        } else if rect_contains(li.customize, col, row) {
            Some(2)
        } else {
            None
        };
        if self.hovered_layout_icon != new_layout_hover {
            self.hovered_layout_icon = new_layout_hover;
            self.overlays.activity.mark_dirty();
        }
        // Chrome button hints: dwell over a labelled control (activity-bar
        // icons, search-panel actions/toggles) to surface its name. Keyed on
        // the label so the dwell only restarts when the pointer crosses to a
        // different control, and is left running while it drifts within one.
        // Computed before the tab/editor early-returns so it tracks the
        // pointer everywhere, including over the sidebar.
        match self.ui_tooltip_text_at(col, row) {
            // Same label AND still on the same row: keep the dwell running so the
            // tooltip persists while the pointer drifts within one control. The
            // row check matters because adjacent controls can share a label (e.g.
            // every enabled extension toggle reads "Enabled — click to disable");
            // without it the anchor would freeze on the first such control and
            // the box would appear detached from the one actually under the
            // pointer.
            Some(label)
                if self.ui_tooltip_label.as_ref() == Some(&label)
                    && self.ui_hover.cell().map(|(_, r)| r) == Some(row) => {}
            Some(label) => {
                self.ui_hover.on_move(std::time::Instant::now(), col, row);
                self.ui_tooltip_label = Some(label);
                self.ui_tooltip = None;
            }
            None => {
                self.ui_hover.clear();
                self.ui_tooltip = None;
                self.ui_tooltip_label = None;
            }
        }
        // Editor tab strip: dwell over a tab to surface its full path. The
        // dwell only restarts when the pointer crosses into a *different*
        // tab, so the tooltip keeps showing while the pointer drifts a few
        // cells within the same tab (matches the LSP "same word" rule).
        if let Some(idx) = self.editor.tab_at(col, row) {
            self.hover.clear();
            self.hover_popup = None;
            self.hover_word = None;
            self.hover_diagnostic = None;
            self.hover_request_id = None;
            if self.tab_hover_idx != Some(idx) {
                self.tab_hover.on_move(std::time::Instant::now(), col, row);
                self.tab_hover_idx = Some(idx);
                self.tab_tooltip = None;
            }
            return;
        }
        self.tab_hover.clear();
        self.tab_tooltip = None;
        self.tab_hover_idx = None;
        if !in_editor {
            self.hover.clear();
            // A terminal-annotation note survives pointer drift while the
            // pointer stays on its own span (the editor's same-region rule);
            // leaving the span dismisses it. Matching the popup's text to
            // the span's note keeps a stale editor hover from hiding here.
            let over_own_note = self
                .hover_popup
                .as_ref()
                .zip(self.terminal().annotation_at(col, row))
                .is_some_and(|(popup, (_, note))| popup.lines.join("\n") == note);
            if !over_own_note {
                self.hover_popup = None;
            }
            self.hover_word = None;
            self.hover_diagnostic = None;
            self.hover_request_id = None;
            return;
        }
        self.hover.on_move(std::time::Instant::now(), col, row);
        // The blame card (#1556) belongs to the annotation of its commit,
        // not to a word: it stays while the pointer moves along that and
        // goes when the pointer leaves it.
        if let Some((hash, _)) = &self.blame_hover {
            if self.editor.blame_commit_at(col, row) == Some(hash.as_str()) {
                return;
            }
            self.blame_hover = None;
            self.hover_popup = None;
        }
        if self.hover_popup.is_some() {
            let current = self
                .editor
                .buffer_pos_at(col, row)
                .and_then(|(line, c)| self.editor.hover_region_at(line, c));
            if current != self.hover_word {
                self.hover_popup = None;
                self.hover_word = None;
                self.hover_diagnostic = None;
                self.hover_request_id = None;
            }
        }
    }

    pub(super) fn poll_hover(&mut self) {
        if !self.hover.due(std::time::Instant::now(), HOVER_DELAY) {
            return;
        }
        self.hover.mark_fired();
        if self.completion_popup.is_some() {
            return;
        }
        if self.editor.has_non_text_view() {
            return;
        }
        let Some((col, row)) = self.hover.cell() else {
            return;
        };
        self.hover_at_cell(col, row);
    }

    /// Resolve a hover at screen cell `(col, row)`: capture any diagnostic
    /// under the point synchronously and, for a real identifier, fire the
    /// async `textDocument/hover`. Split out of `poll_hover` so the dwell
    /// timer doesn't have to be driven to test the resolution logic.
    pub(super) fn hover_at_cell(&mut self, col: u16, row: u16) {
        if let Some(hash) = self.editor.blame_commit_at(col, row) {
            self.hover_blame_card(col, row, hash.to_string());
            return;
        }
        let Some((line, c)) = self.editor.buffer_pos_at(col, row) else {
            return;
        };
        self.hover_at(line, c, (col, row), false);
    }

    /// Show or Focus Hover (#1518): the hover the pointer's rest would open,
    /// for the caret, anchored at the caret's cell. Says so when there is
    /// nothing to show, where the pointer's hover stays silent.
    pub(super) fn show_hover_at_caret(&mut self) {
        self.hover_popup = None;
        self.hover_diagnostic = None;
        self.hover_request_id = None;
        // A pointer that came to rest just before the command would fire its
        // own hover next frame and take this one's place.
        self.hover.clear();
        let Some(anchor) = self.editor.cursor_screen_pos() else {
            return;
        };
        let (line, c) = self.caret_hover_point();
        self.hover_at(line, c, anchor, true);
        if self.hover_popup.is_none() && self.hover_request_id.is_none() {
            self.status = String::from(NO_HOVER_AT_CARET);
        }
    }

    /// The buffer point Show Hover describes: the caret, or the word ending
    /// at it, as in VS Code, so a hover after typing a name or a word-right
    /// move shows that word.
    pub(super) fn caret_hover_point(&self) -> (usize, usize) {
        let (line, col) = (self.editor.cursor_row, self.editor.cursor_col);
        if col > 0
            && self.editor.hover_region_at(line, col).is_none()
            && self.editor.word_at(line, col - 1).is_some()
        {
            return (line, col - 1);
        }
        (line, col)
    }

    /// Resolve a hover at buffer `(line, c)` with its popup at screen cell
    /// `anchor`, for the pointer or, with `at_caret`, the caret.
    pub(super) fn hover_at(&mut self, line: usize, c: usize, anchor: (u16, u16), at_caret: bool) {
        let (col, row) = anchor;
        // The region (word, or else a diagnostic span over punctuation) the
        // popup belongs to. Nothing of interest under the point → no hover.
        let Some(region) = self.editor.hover_region_at(line, c) else {
            return;
        };
        self.hover_anchor = Some((col, row));
        self.hover_word = Some(region);
        self.hover_at_caret = at_caret.then(|| self.status.clone());
        self.hover_doc = (self.editor.path.clone(), self.editor.edit_seq);
        // Debug hover-to-evaluate: while paused, hovering a variable shows its
        // current value (from the loaded locals/globals) instead of the LSP
        // type hover. Takes priority because the live value is what the user
        // wants mid-debug.
        if let Some(text) = self.debug_hover_value(line, c) {
            self.hover_request_id = None;
            self.hover_diagnostic = None;
            self.hover_popup = Some(crate::widgets::hover_popup::HoverPopup::new(
                text,
                (col, row),
            ));
            return;
        }
        // Server diagnostics are already in the editor, so capture the
        // message synchronously; the squiggle's message must show even if the
        // server has no type info (or no LSP is attached at all).
        self.hover_diagnostic =
            format_diagnostics(&self.editor.diagnostics_at(line, c, self.workspace_root()));
        // Only a real word is worth a `textDocument/hover` round-trip; over
        // punctuation there is no symbol to describe, so show the diagnostic
        // alone right away.
        let word = self.editor.word_at(line, c).is_some();
        // Converted before the match: `self.lsp.as_mut()` borrows self for the
        // whole scrutinee, so the editor is unreachable inside the arm.
        let (uline, uchar) = self.editor.pos_to_utf16(line, c);
        match (word, self.editor.path.clone(), self.lsp.as_mut()) {
            (true, Some(path), Some(lsp)) => {
                let id = lsp.request_hover(path, uline, uchar);
                self.hover_request_id = Some(id);
            }
            _ => {
                self.hover_request_id = None;
                self.hover_popup = compose_hover(self.hover_diagnostic.as_deref(), None)
                    .map(|text| crate::widgets::hover_popup::HoverPopup::new(text, (col, row)));
            }
        }
    }

    /// Fire the tab-strip tooltip once its dwell crosses `HOVER_DELAY`.
    /// Returns `true` when a tooltip was created so the frame loop knows
    /// to redraw. Suppressed while a tab context menu is open so the
    /// right-click menu never collides with the tooltip.
    pub(super) fn poll_tab_hover(&mut self) -> bool {
        if !self.tab_hover.due(std::time::Instant::now(), HOVER_DELAY) {
            return false;
        }
        self.tab_hover.mark_fired();
        if self.context_menu.is_some() {
            return false;
        }
        let Some(idx) = self.tab_hover_idx else {
            return false;
        };
        let Some((col, row)) = self.tab_hover.cell() else {
            return false;
        };
        let Some(path) = self.editor.tab_full_path(idx) else {
            return false;
        };
        self.tab_tooltip = Some(crate::widgets::hover_popup::HoverPopup::new(
            path,
            (col, row),
        ));
        true
    }

    pub fn drain_lsp_hover(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut changed = false;
        while let Some(result) = lsp.drain_hover() {
            if Some(result.request_id) != self.hover_request_id {
                continue;
            }
            if self.hover_doc.0 != self.editor.path || self.hover_doc.1 != self.editor.edit_seq {
                continue;
            }
            // A keyboard hover belongs to the caret (#1518), a dwell's to the
            // pointer; either may have moved off the word since the request.
            let current = if self.hover_at_caret.is_some() {
                let (line, ch) = self.caret_hover_point();
                self.editor.hover_region_at(line, ch)
            } else {
                self.hover
                    .cell()
                    .and_then(|(c, r)| self.editor.buffer_pos_at(c, r))
                    .and_then(|(line, ch)| self.editor.hover_region_at(line, ch))
            };
            if current != self.hover_word {
                continue;
            }
            // Diagnostic on top, type info below (VS Code PR #166560). Either
            // side may be empty; an all-empty compose hides the popup.
            let anchor = self.hover_anchor.unwrap_or((0, 0));
            // The server's part is about a symbol in this file's language,
            // which a `surfaces` card lays out as path, signature and docs.
            let lang = self.editor.language();
            let symbol = result.text.is_some();
            self.hover_popup =
                compose_hover(self.hover_diagnostic.as_deref(), result.text.as_deref()).map(
                    |text| {
                        let popup = crate::widgets::hover_popup::HoverPopup::new(text, anchor);
                        if symbol {
                            popup.about_symbol(lang)
                        } else {
                            popup
                        }
                    },
                );
            if self.hover_popup.is_none() && self.hover_at_caret.as_ref() == Some(&self.status) {
                self.status = String::from(NO_HOVER_AT_CARET);
            }
            changed = true;
        }
        changed
    }

    /// Tear down the popups a click invalidates, for press paths that return
    /// early and so miss the teardown every other path runs.
    ///
    /// A fired binding changes the state underneath these popups, and leaving
    /// them up paints a hover, a tab tooltip or a button hint describing what
    /// was there before the command ran. Shared by the matched-gesture
    /// dispatch and the double-click-prefix swallow: both return early, and
    /// having two copies is how the swallow branch came to skip it entirely.
    pub(super) fn dismiss_click_overlays(&mut self, m: MouseEvent) {
        if !matches!(m.kind, MouseEventKind::Up(_)) {
            self.hover_popup = None;
            self.hover_diagnostic = None;
        }
        self.tab_tooltip = None;
        self.tab_hover.clear();
        self.tab_hover_idx = None;
        self.ui_tooltip = None;
        self.ui_hover.clear();
        self.ui_tooltip_label = None;
        self.hover.clear();
    }

    pub(super) fn handle_mouse(&mut self, m: MouseEvent) {
        self.editor.sync_occurrence_word_sel();
        if self.screencast.is_on()
            && let MouseEventKind::Down(button) = m.kind
        {
            self.screencast.click(
                m.column,
                m.row,
                button,
                m.modifiers,
                std::time::Instant::now(),
            );
        }
        self.recording_file_switches(|app| app.handle_mouse_inner(m));
        self.drop_unkeyed_palette_macro_keys();
        self.trim_auto_indents();
    }

    /// Clear the indentation Enter left on a line the caret has since left
    /// (#956), in every tab of every editor group: a key or click that
    /// switched tabs or groups has left the line in the one it came from.
    pub(super) fn trim_auto_indents(&mut self) {
        self.editor.trim_auto_indents(true);
        for group in self.editor_layout.inactive_groups_mut() {
            group.trim_auto_indents(false);
        }
    }

    /// End every press and drag under way without doing what its release
    /// would (#2678 review): no drop, no reorder, no copy-on-select. A press
    /// a program in a terminal pane asked for still gets its release there,
    /// so the program never sees the button held down.
    pub(super) fn abandon_mouse_gestures(&mut self, m: &MouseEvent) {
        self.editor_tab_drag_from = None;
        self.editor_line_drag = None;
        if let Some(fp) = self.terminal_pointer_forwarded.take()
            && let Some(idx) = self.forwarded_pane_index(fp)
        {
            let (col, row) = self.terminals[idx].clamp_to_grid(m.column, m.row);
            self.terminals[idx].report_mouse(
                MouseButtonKind::Left,
                MouseAction::Release,
                col,
                row,
                mouse_mods(m),
            );
            if fp.selecting {
                self.terminals[idx].end_drag();
            }
        }
        self.terminal_select_autoscroll = None;
        self.terminal_drag_from = None;
        if self.editor.box_selecting() {
            self.editor.end_box_select();
        }
        self.splitter_drag = None;
        self.minimap_drag = false;
        self.scrollbar_drag = None;
        self.editor_hscrollbar_drag = false;
        self.breakpoint_lane_press = false;
        self.outline_scrollbar_drag = false;
        self.secondary_outline_scrollbar_drag = false;
        self.open_editors_scrollbar_drag = false;
        self.timeline_scrollbar_drag = false;
        self.deps_scrollbar_drag = false;
        self.testing_scrollbar_drag = false;
        self.graph_scrollbar_drag = false;
        self.scrub_dragging = false;
        if self.tree_drag.take().is_some() {
            self.tree.drag_target = None;
        }
    }

    pub(super) fn handle_mouse_inner(&mut self, m: MouseEvent) {
        // So does a click (#1609), except on the message itself, which opens
        // its full text below.
        let on_message = rect_contains(self.status_message_rect, m.column, m.row);
        if matches!(m.kind, MouseEventKind::Down(_)) && !on_message {
            self.status_wide = false;
        }
        // A click means the user moved on from a pending shortcut prompt.
        if matches!(m.kind, MouseEventKind::Down(_))
            && (self.recording_shortcut.take().is_some()
                | self.confirming_shortcut.take().is_some())
        {
            self.status = String::from("Shortcut unchanged");
        }
        // On-screen keyboard taps outrank every other gate - including the
        // modal overlays - because the OSK is how Termux users type into
        // those modals. Its band is laid out disjoint from all panes, so
        // swallowing events inside it never steals a pane's click.
        if self
            .osk
            .as_ref()
            .is_some_and(|k| rect_contains(k.last_area, m.column, m.row))
        {
            self.handle_osk_mouse(m);
            return;
        }
        // The pending paste was checked against the active pane and the
        // broadcast set; a click that switched panes would send it to one
        // that was never checked. Only the popup's keys answer it. A drag
        // it cut short ends without its drop, as under the others below.
        if self.pending_terminal_paste.is_some() {
            if matches!(m.kind, MouseEventKind::Up(_)) {
                self.abandon_mouse_gestures(&m);
            }
            return;
        }
        // A click on a find bar (in the editor or over a terminal) puts the
        // caret of the row it lands on, before the pane under it sees it.
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
            && !self.popup_open()
            && self.context_menu.is_none()
        {
            if self
                .editor_find
                .as_mut()
                .is_some_and(|f| f.click_at(m.column, m.row))
            {
                // The bar takes the keys only while the editor has focus, so
                // a click on it from the terminal or the side bar moves focus
                // there too, or the typing after it would land elsewhere.
                if self.focus != Pane::Editor {
                    self.focus_pane(Pane::Editor);
                }
                return;
            }
            // The terminal find bar closes whenever the terminal loses
            // focus, so a click on it always comes with focus already there.
            if self
                .terminal_find
                .as_mut()
                .is_some_and(|f| f.click_at(m.column, m.row))
            {
                return;
            }
        }
        // The wheel over an editor group's tab strip scrolls its tabs.
        if let Some(delta) = match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollLeft => Some(-1),
            MouseEventKind::ScrollDown | MouseEventKind::ScrollRight => Some(1),
            _ => None,
        } && !self.popup_open()
            && self.context_menu.is_none()
        {
            let mut scrolled = false;
            for g in self.editor_layout.all_groups_mut(&mut self.editor) {
                if g.tab_strip_contains(m.column, m.row) {
                    g.scroll_tabs_wheel(delta);
                    scrolled = true;
                    break;
                }
            }
            if scrolled {
                return;
            }
        }
        // A click on the port-detection toast's action buttons is consumed
        // before anything else; clicks elsewhere fall through to normal
        // handling, so the toast never blocks the rest of the UI. The pop-ups
        // are drawn over the toasts, so while one is open a click on a
        // button it hides must not press it (#2678 review).
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
            && !self.popup_open()
            && (self.handle_port_toast_click(m.column, m.row)
                || self.handle_update_toast_click(m.column, m.row))
        {
            return;
        }
        // A pop-up that handles no mouse of its own (an agent's proposal,
        // every confirm, the New File box, Settings, the startup warning)
        // takes every mouse
        // event: a click behind the unsaved-changes prompt must not close or
        // move the tabs it is asking about (#862), and the same holds for
        // what each of the others asks about (#2674).
        if self.popup_takes_the_mouse() {
            // A drag under way when the pop-up opened ends with the release
            // the pop-up takes, without its drop: left live, the next release
            // after the pop-up closed finished it, moving a file into the
            // folder it was over (#2678 review).
            if matches!(m.kind, MouseEventKind::Up(_)) {
                self.abandon_mouse_gestures(&m);
            }
            return;
        }
        // The history scrubber's slider (#371): a press on the track seeks
        // there, and a drag that started on it keeps seeking wherever the
        // pointer goes, so the handle can be dragged off the row and back.
        if self.scrubber.is_some() && !self.popup_open() {
            let on_track = rect_contains(self.scrub_slider, m.column, m.row);
            match m.kind {
                MouseEventKind::Down(MouseButton::Left) if on_track => {
                    self.scrub_dragging = true;
                    self.scrub_seek_column(m.column);
                    return;
                }
                MouseEventKind::Drag(MouseButton::Left) if self.scrub_dragging => {
                    self.scrub_seek_column(m.column);
                    return;
                }
                MouseEventKind::Up(MouseButton::Left) if self.scrub_dragging => {
                    self.scrub_dragging = false;
                    return;
                }
                _ => {}
            }
        }
        if self.connect_dialog.is_some() {
            if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.handle_connect_dialog_click(m.column, m.row);
            }
            return;
        }
        if self.shortcuts_modal.is_some() {
            self.handle_shortcuts_modal_mouse(m);
            return;
        }
        if self.file_finder.is_some() {
            self.handle_file_finder_mouse(m);
            return;
        }
        if self.process_picker.is_some() {
            self.handle_process_picker_mouse(m);
            return;
        }
        if self.command_palette.is_some() {
            self.handle_command_palette_mouse(m);
            return;
        }
        if self.workspace_symbols.is_some() {
            self.handle_workspace_symbols_mouse(m);
            return;
        }
        if self.go_to_symbol.is_some() {
            self.handle_go_to_symbol_mouse(m);
            return;
        }
        if self.zoxide_jump.is_some() {
            self.handle_zoxide_jump_mouse(m);
            return;
        }
        if self.command_history_popup.is_some() {
            // Keyboard-first popup: a click anywhere dismisses it (Esc's
            // mouse equivalent); the wheel moves the selection.
            match m.kind {
                MouseEventKind::ScrollUp => {
                    if let Some(pop) = self.command_history_popup.as_mut() {
                        pop.select_prev();
                    }
                }
                MouseEventKind::ScrollDown => {
                    if let Some(pop) = self.command_history_popup.as_mut() {
                        pop.select_next();
                    }
                }
                MouseEventKind::Down(_) => self.command_history_popup = None,
                _ => {}
            }
            return;
        }
        if self.branch_picker.is_some() {
            self.handle_branch_picker_mouse(m);
            return;
        }
        if self.input_prompt.is_some() {
            self.handle_input_prompt_mouse(m);
            return;
        }
        if self.list_picker.is_some() {
            self.handle_list_picker_mouse(m);
            return;
        }
        // Source Control's menus (#871) take left clicks here, where they
        // always have; the pointer, the wheel and other buttons pass them by.
        // A click outside the Commit dropdown closes it and goes on.
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            match self.context_menu.as_ref().map(|menu| menu.look) {
                Some(MenuLook::ScmActions { .. }) => {
                    self.click_scm_actions_menu(m.column, m.row);
                    return;
                }
                Some(MenuLook::CommitActions)
                    if self.click_commit_actions_menu(m.column, m.row) =>
                {
                    return;
                }
                _ => {}
            }
        }
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
            && self
                .shortcuts_hit_rect
                .is_some_and(|r| rect_contains(r, m.column, m.row))
        {
            self.open_shortcuts_modal();
            return;
        }
        // Clickable status-bar segments: encoding / EOL / language open a
        // "change" picker; the diagnostics counts jump to the PROBLEMS panel.
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            if rect_contains(self.status_language_rect, m.column, m.row) {
                self.open_status_language_menu(
                    self.status_language_rect.x,
                    self.status_language_rect.y,
                );
                return;
            }
            if rect_contains(self.status_eol_rect, m.column, m.row) {
                self.open_status_eol_menu(self.status_eol_rect.x, self.status_eol_rect.y);
                return;
            }
            if rect_contains(self.status_encoding_rect, m.column, m.row) {
                self.open_status_encoding_menu(
                    self.status_encoding_rect.x,
                    self.status_encoding_rect.y,
                );
                return;
            }
            if rect_contains(self.status_position_rect, m.column, m.row) {
                self.open_go_to_line_prompt();
                return;
            }
            if rect_contains(self.status_indent_rect, m.column, m.row) {
                self.open_status_indent_menu(self.status_indent_rect.x, self.status_indent_rect.y);
                return;
            }
            if rect_contains(self.status_codeql_rect, m.column, m.row) {
                self.cancel_codeql_run();
                return;
            }
            if rect_contains(self.status_offer_rect, m.column, m.row) {
                if let Some(t) = self.update_toast.as_mut() {
                    t.open = !t.open;
                }
                return;
            }
            if rect_contains(self.status_update_rect, m.column, m.row) {
                self.update_croft();
                return;
            }
            if rect_contains(self.status_diag_rect, m.column, m.row) {
                self.set_bottom_panel_tab(BottomPanelTab::Problems);
                return;
            }
            if on_message {
                self.show_status_messages();
                return;
            }
            if rect_contains(self.status_agents_rect, m.column, m.row) {
                self.focus_agent_pane();
                return;
            }
        }
        // Layout-control icons (Toggle Side Bar / Panel / Customize Layout) at
        // the top-right of the editor tab strip / welcome screen. Checked
        // before the panes so a click on an icon never falls through to the
        // editor below it. Skipped while a menu is open (that block wins).
        if self.context_menu.is_none() && matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
        {
            let icons = self.layout_icon_areas;
            if rect_contains(icons.toggle_side_bar, m.column, m.row) {
                self.toggle_side_bar();
                self.after_chrome_visibility_change();
                return;
            }
            if rect_contains(icons.toggle_panel, m.column, m.row) {
                self.toggle_terminal();
                return;
            }
            if rect_contains(icons.customize, m.column, m.row) {
                self.open_customize_layout_menu();
                return;
            }
        }

        // While a context menu is open, route clicks to it.
        if self
            .context_menu
            .as_ref()
            .is_some_and(|menu| menu.look == MenuLook::Panel)
        {
            match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    // 1. A click inside the open submenu panel dispatches that child.
                    if let Some(child) = self.submenu_item_at(m.column, m.row) {
                        let dispatch = self.context_menu.as_ref().and_then(|menu| {
                            let parent = menu.open_submenu?;
                            let MenuEntry::Submenu { items, .. } = menu.items.get(parent)? else {
                                return None;
                            };
                            match items.get(child) {
                                Some(MenuEntry::Item { action, .. }) => {
                                    Some((action.clone(), menu.target_dir.clone()))
                                }
                                _ => None,
                            }
                        });
                        if let Some((action, dir)) = dispatch {
                            self.context_menu = None;
                            self.dispatch_menu_action(action, dir);
                        }
                        return;
                    }
                    // 2. A click on a top-level row: dispatch an item, or open a submenu.
                    if let Some(idx) = self.menu_item_at(m.column, m.row) {
                        let action =
                            self.context_menu
                                .as_ref()
                                .and_then(|menu| match menu.items.get(idx) {
                                    Some(MenuEntry::Item { action, .. }) => {
                                        Some((action.clone(), menu.target_dir.clone()))
                                    }
                                    _ => None,
                                });
                        match action {
                            Some((action, dir)) => {
                                self.context_menu = None;
                                self.dispatch_menu_action(action, dir);
                            }
                            None => {
                                // A submenu parent (or a divider): open the submenu.
                                if let Some(menu) = self.context_menu.as_mut() {
                                    menu.selected = idx;
                                    menu.open_cursor_submenu();
                                }
                            }
                        }
                        return;
                    }
                    // 3. Click outside every panel: dismiss.
                    self.context_menu = None;
                    return;
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    self.context_menu = None;
                    // fall through so the right-click below can re-open if applicable
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    // The wheel walks the highlighted row through the level
                    // (same as Up/Down), and pans the panel once the
                    // highlight can move no further. Which level it drives
                    // depends on the pointer: over the floating submenu it is
                    // the submenu, anywhere else the top-level panel.
                    let down = m.kind == MouseEventKind::ScrollDown;
                    let over_submenu = self
                        .submenu_rect()
                        .is_some_and(|r| rect_contains(r, m.column, m.row));
                    // Over the parent panel with a submenu open, the wheel
                    // is the parent's: the submenu closes, as moving the
                    // parent's highlight off its row does, and the parent
                    // steps. Otherwise the active level steps.
                    if !over_submenu
                        && self
                            .menu_rect()
                            .is_some_and(|r| rect_contains(r, m.column, m.row))
                        && let Some(menu) = self.context_menu.as_mut()
                    {
                        menu.close_submenu();
                    }
                    let visible = self.menu_visible_rows();
                    if let Some(menu) = self.context_menu.as_mut() {
                        menu.wheel_active(if down { 3 } else { -3 }, visible);
                    }
                    return;
                }
                _ => return,
            }
        }

        // When the editor is split, a press anywhere in the NON-focused
        // column (except its draggable seam) moves focus there first, so
        // the rest of this handler - which all reads `self.editor` - then
        // operates on the clicked group. The swap keeps the physical
        // columns fixed.
        if matches!(m.kind, MouseEventKind::Down(_))
            && self.editor_layout.is_split()
            && self.seam_at(m.column, m.row).is_none()
            && let Some(target) = self.editor_layout.inactive_dfs_index_at(m.column, m.row)
        {
            let current = std::mem::take(&mut self.editor);
            self.editor = self.editor_layout.refocus_to_dfs(current, target);
            self.focus_pane(Pane::Editor);
        }

        // Hit-test the side panel against the ACTIVE widget's last_area
        // rather than always against `self.tree.last_area`. The tree is
        // only re-rendered when the Explorer view is active; switching
        // to Search / Source Control / Remote / Run-Debug freezes
        // `tree.last_area` at whatever rectangle the last Explorer
        // render captured, which can be smaller than the current side
        // panel and silently swallow clicks on rows below that rect.
        let active_sidebar_area = match self.sidebar_view {
            SidebarView::Explorer => self.tree.last_area,
            SidebarView::Search => self.search.last_area,
            SidebarView::SourceControl => self.source_control.last_area,
            SidebarView::Remote => self.remote.last_area,
            SidebarView::RunDebug => self.run_debug.last_area,
            SidebarView::Extensions => self.extensions.last_area,
            SidebarView::Testing => self.testing.last_area,
            SidebarView::CodeQL => self.codeql.last_area,
        };
        // The COMMITS graph docks BELOW the change list in its own strip, so
        // `source_control.last_area` alone misses it; without this union the
        // graph's wheel-scroll and click handlers (all gated on `in_tree`)
        // are unreachable (Codeberg #41).
        let in_commit_graph = self.sidebar_view == SidebarView::SourceControl
            && rect_contains(self.commit_graph.last_area, m.column, m.row);
        let in_tree = self.show_tree
            && (rect_contains(active_sidebar_area, m.column, m.row) || in_commit_graph);
        // The OUTLINE section sits below the tree in the Explorer sidebar and
        // scrolls independently, so wheel events over it must reach the panel
        // rather than the tree above.
        let in_outline = self.sidebar_view == SidebarView::Explorer
            && rect_contains(self.outline.last_area, m.column, m.row);
        let in_secondary_outline = self.secondary_side_bar_visible
            && rect_contains(self.secondary_outline.area(), m.column, m.row);
        let in_open_editors = self.sidebar_view == SidebarView::Explorer
            && rect_contains(self.open_editors.last_area, m.column, m.row);
        let in_timeline = self.sidebar_view == SidebarView::Explorer
            && rect_contains(self.timeline.last_area, m.column, m.row);
        let in_deps = self.sidebar_view == SidebarView::Explorer
            && rect_contains(self.dependencies.last_area, m.column, m.row);
        let in_editor_pane = rect_contains(self.editor.last_full_area, m.column, m.row);
        let in_editor = rect_contains(self.editor.last_area, m.column, m.row);
        let terminal_hit = self.terminal_at_pos(m.column, m.row);
        let in_terminal = terminal_hit.is_some();
        // User mouse bindings (#259) resolve before the built-in widget
        // behaviour, mirroring how keybindings.json wins over default chords.
        // Placed after the region predicates so a binding's `when` context is
        // known, and above every built-in so rebinding one actually takes it
        // over. A gesture croft has no binding for falls straight through.
        // Classify ONCE and reuse. `gesture_for` reads the modified-click
        // tracker to decide `Click` vs `DoubleClick`, so calling it a second
        // time after recording would let a single click arm itself and come
        // back a double — which is exactly what an earlier attempt at this did.
        // The recording therefore happens after the lookup, using the gesture
        // decided here, never a freshly re-derived one.
        let bound_mouse = if self.keymap.has_mouse_bindings()
            && let Some(ctx) = mouse_context_for(&m, in_editor, in_terminal, in_tree, self)
            && let Some(gesture) = gesture_for(&m, self, ctx, std::time::Instant::now())
        {
            Some((ctx, gesture, self.keymap.command_for_mouse(gesture, ctx)))
        } else {
            None
        };

        // A modified click is recorded whether or not it matched a binding. The
        // first click of a `ctrl+double_click` pair matches nothing by
        // construction — it classifies as `ctrl+click`, and a user who bound
        // only the double has nothing for it to hit — so recording only on a
        // match left the pair's second click seeing an empty tracker, and the
        // double could not fire at all.
        //
        // Unmodified clicks are deliberately not recorded here: their trackers
        // are shared with the built-ins, which record their own clicks on the
        // paths below. Doing it in both places would double-count.
        //
        // A click the pointer-owning child gets is not croft's to remember.
        // The built-ins record INSIDE their `child_owns_pointer` check; this
        // recording sat above it, so clicks croft deliberately declined to act
        // on still armed the tracker, and a later modified click paired with
        // one the user had aimed at the TUI in the terminal. Same predicate as
        // the dispatch below, indexed by the pane that was CLICKED.
        let child_owns_pointer = bound_mouse.is_some_and(|(ctx, gesture, _)| {
            // Only the gestures croft actually FORWARDS: a tracking child can
            // own nothing else. That is the wheel (both wheel arms below) and
            // the LEFT button -- press in the Down(Left) arm, motion in Drag,
            // release in Up -- since #474, when clicking a TUI's own controls
            // (Claude Code's diff-panel ×) started working. `Click` and
            // `DoubleClick` are both Down(Left), so both are the child's.
            // Middle and right clicks are still croft's (paste, the context
            // menu), so a binding on them fires over a tracking child too.
            matches!(
                gesture.kind,
                crate::keymap::GestureKind::WheelUp
                    | crate::keymap::GestureKind::WheelDown
                    | crate::keymap::GestureKind::Click
                    | crate::keymap::GestureKind::DoubleClick
            ) && matches!(ctx, crate::keymap::MouseContext::Terminal)
                && terminal_hit.is_some_and(|idx| {
                    // Tracking is not enough: `terminal_at_pos` hit-tests
                    // `last_area`, which includes the BORDER, while delivery
                    // goes through `cell_at` on `last_inner`, which does not.
                    // Declining a binding for a cell the child will never be
                    // told about loses the gesture for both parties. The
                    // built-in wheel path already falls through on exactly
                    // this case — see the `report_mouse` guard on both wheel
                    // arms, whose comment names the border explicitly.
                    self.terminals[idx].mouse_reporting()
                        && self.terminals[idx].cell_at(m.column, m.row).is_some()
                })
                && !m.modifiers.contains(KeyModifiers::SHIFT)
        });
        if let Some((_, gesture, _)) = bound_mouse
            && !gesture.mods.is_empty()
            && !child_owns_pointer
        {
            let now = std::time::Instant::now();
            match gesture.kind {
                // Clear on a fired double so a third click re-arms rather than
                // re-firing; record otherwise so the NEXT modified click can be
                // seen as the second of a pair.
                crate::keymap::GestureKind::DoubleClick => self.modified_click.clear(),
                crate::keymap::GestureKind::Click => {
                    self.modified_click
                        .record(now, m.column, m.row, gesture.mods)
                }
                _ => {}
            }
        }

        // The first half of a bound double-click matches no binding, so
        // without this it falls through to whatever built-in owns the same
        // modifier: binding `ctrl+double_click` in the editor made the pair's
        // first click fire go-to-definition, and in the terminal it armed the
        // plain double-click tracker so the user's NEXT ordinary click
        // selected a word they never gestured for. Both are the same bug —
        // croft acting on a click the user aimed at their own binding.
        //
        // Swallowed AFTER the modified-click recording above, which is what
        // lets the second click of the pair see the first. Only the prefix of
        // a bound double is swallowed: a Ctrl+click with no double bound
        // reaches the built-in unchanged.
        //
        // `child_owns_pointer` for the same reason the matched dispatch below
        // consults it — a TUI that asked for mouse tracking owns the pointer,
        // so croft must not suppress a built-in on its behalf. The recording
        // above already declines there, so the bound double is unreachable in
        // that state anyway: swallowing would cost the built-in and buy
        // nothing. Omitting it here disabled the built-in Ctrl+click over
        // every full-screen TUI for anyone who bound only `ctrl+double_click`.
        // Over a tracking child the click is forwarded to it (#474), so
        // `child_owns_pointer` is true for a left click there and the swallow
        // steps aside: the recording above already declined, the bound double
        // is unreachable, and swallowing would only cost the child its press.
        // Where the child does not own it, the question is narrower: would
        // declining this click COST the user a built-in that was going to act?
        // For Ctrl/Cmd the built-in opens a URL or a file reference, so defer
        // when either is under the cursor -- that link-open runs BEFORE the
        // forward in the Down(Left) arm, so it applies over a tracking child
        // as well.
        let builtin_would_act = matches!(bound_mouse, Some((ctx, _, _)) if
            matches!(ctx, crate::keymap::MouseContext::Terminal))
            && (m.modifiers.contains(KeyModifiers::CONTROL)
                || m.modifiers.contains(KeyModifiers::SUPER))
            && terminal_hit
                .is_some_and(|idx| self.terminal_would_open_something_at(idx, m.column, m.row));
        if let Some((ctx, gesture, None)) = bound_mouse
            && !gesture.mods.is_empty()
            && !builtin_would_act
            && !child_owns_pointer
            && self.keymap.is_double_click_prefix(gesture, ctx)
        {
            // Focus still moves, as a click in a pane always does — otherwise
            // the pair's second click lands in a pane the keyboard has not
            // followed. This mirrors what the matched path does below.
            match ctx {
                crate::keymap::MouseContext::Terminal => {
                    if let Some(idx) = terminal_hit {
                        self.activate_terminal_pane(idx);
                        self.focus_pane(Pane::Terminal);
                    }
                }
                crate::keymap::MouseContext::Editor | crate::keymap::MouseContext::TabStrip => {
                    self.focus_pane(Pane::Editor)
                }
                crate::keymap::MouseContext::FileTree => self.focus_pane(Pane::Tree),
            }
            // Same teardown the matched branch runs: this is an early return
            // on the same press path, and it re-focuses a pane underneath
            // whatever popups were showing.
            self.dismiss_click_overlays(m);
            return;
        }

        if let Some((ctx, gesture, Some(cmd))) = bound_mouse {
            // A terminal running a mouse-tracking TUI owns the pointer, same
            // rule the built-ins follow: croft must not steal a click the
            // child asked for. Computed once above so the recording and this
            // dispatch cannot drift apart — they must agree on whether the
            // click was croft's to act on.
            if !child_owns_pointer {
                // Every built-in pairs `is_double` with `record`/`clear` so
                // the count stays right. The early return below skips the
                // built-in branch that would have recorded, so the dispatch
                // has to keep the tracker itself: clear on a fired double
                // (or a third click re-fires it), record on a fired UNMODIFIED
                // single (or a `click` binding starves the `double_click`
                // binding beside it — `is_double` could never become true).
                //
                // Unmodified only, because the built-ins that read these
                // trackers do not consult modifiers. Recording a ctrl+click
                // would arm the editor's plain double-click, and the user's
                // next ordinary click would select a word they never asked
                // for. Bare clicks are reserved in `editor`/`terminal`, so
                // the record here only ever matters for `file_tree` and
                // `tab_strip`, which is exactly where it is needed.
                // Modified clicks are recorded above, before the command
                // lookup, so the first click of a pair is remembered even
                // though it matches nothing.
                let now = std::time::Instant::now();
                if gesture.mods.is_empty() {
                    let tracker = match ctx {
                        crate::keymap::MouseContext::Terminal => &mut self.terminal_click,
                        crate::keymap::MouseContext::FileTree => &mut self.tree_click,
                        // The strip shares the editor's tracker, as
                        // `gesture_for` does: a double-click is the same
                        // gesture whether it lands on a tab or the text below
                        // it. Spelled out rather than caught by `_` so a new
                        // variant has to choose.
                        crate::keymap::MouseContext::Editor
                        | crate::keymap::MouseContext::TabStrip => &mut self.editor_click,
                    };
                    match gesture.kind {
                        crate::keymap::GestureKind::DoubleClick => tracker.clear(),
                        // Pass the modifiers through: #317's guard then keeps
                        // a MODIFIED gesture from arming the built-in tracker,
                        // which is what the separate ModifiedClickTracker
                        // exists for. Hard-coding NONE here would re-open the
                        // exact hole - a ctrl+click binding arming the plain
                        // double-click the user never made.
                        crate::keymap::GestureKind::Click => {
                            tracker.record(now, m.column, m.row, m.modifiers)
                        }
                        _ => {}
                    }
                }
                // The early return below skips the teardown every other
                // press path runs, so do it here: a fired binding changes
                // the state underneath these popups, and leaving them up
                // paints a hover, a tab tooltip or a button hint describing
                // what was there before the command ran. The dwell timer is
                // cleared for the same reason — it is armed at coordinates
                // whose meaning the command just changed.
                self.dismiss_click_overlays(m);

                // Termux: the tap still has to raise the on-screen keyboard.
                // Skipping it leaves a device with no other keyboard unable
                // to type into the field the tap just focused. Reachable
                // despite bare clicks being reserved in editor/terminal: any
                // MODIFIED click in the editor or terminal gets here, and a
                // bound gesture there should raise the keyboard exactly as the
                // built-in press would. `ctx` carries the region, so the
                // built-in's in_editor / in_terminal / in_tree — computed
                // below this block — are not needed to make the same decision.
                if self.osk_auto
                    && self.osk.is_none()
                    && matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
                    && match ctx {
                        crate::keymap::MouseContext::Editor
                        | crate::keymap::MouseContext::Terminal => true,
                        // `mouse_context_for` only yields `FileTree` while the
                        // sidebar is showing the Explorer, so the Search view
                        // this once tested for is unreachable from here — the
                        // narrowing that fixed tree bindings firing over other
                        // sidebar views left this arm behind. The tree has no
                        // text input to raise a keyboard for, so `false` is
                        // what the reachable half always evaluated to anyway.
                        crate::keymap::MouseContext::FileTree
                        | crate::keymap::MouseContext::TabStrip => false,
                    }
                {
                    let mut osk = crate::widgets::osk::Osk::new();
                    osk.split = crate::prefs::Prefs::load_or_default().osk_split;
                    self.osk = Some(osk);
                }

                // Every terminal command reads the grid through
                // `self.terminal()`, i.e. `active_terminal`. The built-in
                // click path makes the clicked pane active before it runs
                // any of them; this early-returns above that, so it has to
                // do the same or a bound command inspects whichever pane
                // happened to be focused. Same helper, so the overlay
                // teardown cannot drift between the two paths.
                if matches!(ctx, crate::keymap::MouseContext::Terminal)
                    && let Some(idx) = terminal_hit
                {
                    self.activate_terminal_pane(idx);
                }

                // Clicking a pane focuses it — every built-in click handler
                // does this, and returning above them skips it. Distinct from
                // the pane activation above: that decides which terminal a
                // command READS, this decides where the next KEYSTROKE goes.
                // Without it a bound gesture that clicks into another pane
                // acts on the clicked pane while the keyboard silently stays
                // behind, so the user's next character lands in the pane they
                // just clicked away from.
                //
                // The tab strip focuses the editor: it has no focus of its
                // own, and that is what clicking a tab does anyway.
                match ctx {
                    crate::keymap::MouseContext::Terminal => self.focus_pane(Pane::Terminal),
                    crate::keymap::MouseContext::FileTree => self.focus_pane(Pane::Tree),
                    crate::keymap::MouseContext::Editor | crate::keymap::MouseContext::TabStrip => {
                        self.focus_pane(Pane::Editor)
                    }
                }

                // Position-carrying commands read the click through this,
                // set before dispatch and cleared after so a keyboard
                // invocation of the same command never sees a stale click.
                self.last_mouse_pos = Some((m.column, m.row));
                self.tab_click_target = self.editor.tab_at(m.column, m.row);
                self.run_command(cmd);
                self.last_mouse_pos = None;
                self.tab_click_target = None;
                return;
            }
        }
        // A middle-click on a tab closes that tab (#1427), as in VS Code and
        // browsers, asking about unsaved edits first as Ctrl+W does. The
        // active tab stays active. A `middle_click` binding in `tab_strip`
        // has already run above instead.
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Middle))
            && let Some(idx) = self.editor.tab_at(m.column, m.row)
        {
            self.dismiss_click_overlays(m);
            self.focus_pane(Pane::Editor);
            self.request_close_tab(idx);
            return;
        }
        let in_problems = self.bottom_panel_tab == BottomPanelTab::Problems
            && rect_contains(self.problems.last_area, m.column, m.row);
        let in_output = self.bottom_panel_tab == BottomPanelTab::Output
            && rect_contains(self.output.last_area, m.column, m.row);
        let in_tree_scrollbar = self.sidebar_view == SidebarView::Explorer
            && rect_contains(self.tree.last_scrollbar, m.column, m.row);
        let in_remote_scrollbar = self.sidebar_view == SidebarView::Remote
            && rect_contains(self.remote.last_scrollbar, m.column, m.row);
        let in_search_scrollbar = self.sidebar_view == SidebarView::Search
            && rect_contains(self.search.last_scrollbar, m.column, m.row);
        let in_editor_scrollbar = rect_contains(self.editor.last_scrollbar, m.column, m.row);
        let in_editor_hscrollbar = rect_contains(self.editor.last_hscrollbar, m.column, m.row);

        // The wheel over a hover scrolls its text rather than dismissing it
        // (#1254): a long docstring's Returns/Raises sit below the box.
        if let Some(popup) = self.hover_popup.as_mut()
            && rect_contains(popup.last_area, m.column, m.row)
        {
            match m.kind {
                MouseEventKind::ScrollDown => {
                    popup.scroll_by(3);
                    return;
                }
                MouseEventKind::ScrollUp => {
                    popup.scroll_by(-3);
                    return;
                }
                _ => {}
            }
        }
        if matches!(m.kind, MouseEventKind::Moved) {
            self.pointer_cell = Some((m.column, m.row));
            // Name the preview link under the pointer (#931), as a
            // browser's status bar does.
            if in_editor
                && let Some(md) = self.editor.markdown_preview.as_ref()
                && let Some(i) = md.link_at(m.column, m.row)
            {
                self.status = format!("{} (click to follow)", md.links.links[i].dest);
            }
            self.on_mouse_moved(m.column, m.row, in_editor);
            return;
        }
        // Extend / finish a rendered-preview selection (#215) before any
        // other gesture claims the drag.
        if self.update_preview_selection(m) {
            return;
        }
        // A rendered log carries its own selection for the same reason the
        // preview does: the tab's `lines` is a stub (#257).
        if self.update_log_selection(m) {
            return;
        }
        // The release of a press is not a new gesture, so it keeps the popup:
        // that is what lets a press-and-hold (touch) read the hover after
        // lifting the finger. Every other event dismisses it.
        if !matches!(m.kind, MouseEventKind::Up(_)) {
            self.hover_popup = None;
            self.hover_diagnostic = None;
        }
        // Any click dismisses the tab tooltip (selecting/closing a tab,
        // or clicking elsewhere) so it can't linger over the new state.
        self.tab_tooltip = None;
        self.tab_hover.clear();
        self.tab_hover_idx = None;
        // Same for the chrome button hint: a click acts on the control, so the
        // hint has served its purpose and must not survive the state change.
        self.ui_tooltip = None;
        self.ui_hover.clear();
        self.ui_tooltip_label = None;

        // A left press in the editor body arms the same dwell a resting
        // pointer does: a touchscreen emits no Moved events (a finger cannot
        // hover), so the tap itself is the "pointer arrived here" signal and
        // the LSP hover fires HOVER_DELAY later. This matches VS Code on
        // desktop too, where the hover re-shows when the pointer rests on a
        // symbol after a click. Drags (selection) and scrolls cancel the
        // pending dwell; the release does not, so a quick tap still fires.
        match m.kind {
            MouseEventKind::Down(MouseButton::Left)
                if in_editor
                    && !in_editor_scrollbar
                    && !in_editor_hscrollbar
                    && self.editor.tab_at(m.column, m.row).is_none() =>
            {
                // clear() before on_move(): a repeat tap on the same cell
                // must restart the timer (on_move alone keys off cell change).
                self.hover.clear();
                self.hover
                    .on_move(std::time::Instant::now(), m.column, m.row);
            }
            MouseEventKind::Up(_) => {}
            _ => self.hover.clear(),
        }

        // Termux: a tap in any editable area raises the on-screen keyboard,
        // standing in for the native soft keyboard that active mouse
        // tracking permanently suppresses. The tap still falls through to
        // its normal focus / cursor handling below.
        if self.osk_auto
            && self.osk.is_none()
            && matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
            && (in_editor || in_terminal || (in_tree && self.sidebar_view == SidebarView::Search))
        {
            let mut osk = crate::widgets::osk::Osk::new();
            // Restore the persisted split-layout choice (foldables).
            osk.split = crate::prefs::Prefs::load_or_default().osk_split;
            self.osk = Some(osk);
            // The raise reflows the whole frame, so a dwell armed at the
            // pre-raise coordinates would resolve the wrong buffer cell.
            self.hover.clear();
        }

        match m.kind {
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some(target) =
                    self.right_click_target(m.column, m.row, in_tree, in_editor, terminal_hit)
                {
                    self.open_context_menu(target, (m.column, m.row));
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // A new press starts a new gesture, even if the release of a
                // breakpoint-lane press never arrived.
                self.breakpoint_lane_press = false;
                self.editor_line_drag = None;
                if in_secondary_outline {
                    self.click_secondary_outline(m.column, m.row);
                    return;
                }
                // Markdown/rendered preview selection (#215): a press inside
                // the rendered view starts a drag-selection over what the
                // user can SEE, not the source buffer beneath it.
                if self.begin_preview_selection(m.column, m.row) {
                    return;
                }
                // Before the selection: a Ctrl+click on a printed
                // `path:line` follows it rather than starting a drag (#257).
                if m.modifiers.contains(KeyModifiers::CONTROL)
                    && self.log_file_click(m.column, m.row)
                {
                    return;
                }
                if self.begin_log_selection(m.column, m.row) {
                    return;
                }
                // (H) Go to Definition rides CTRL, matching the terminal pane's
                // file/URL clicks. SGR mouse reporting carries no super bit, so
                // ALT is left to the editor's Alt+click multi-cursor below.
                if in_editor && m.modifiers.contains(KeyModifiers::CONTROL) {
                    if m.modifiers.contains(KeyModifiers::SHIFT) {
                        self.nav_back();
                    } else if let Some(line0) = self.editor.gutter_line_at(m.column, m.row) {
                        // Ctrl+click on the gutter toggles a bookmark on that
                        // line (a plain click there still toggles a breakpoint).
                        self.toggle_bookmark_at(line0 + 1);
                        self.poke_cursor();
                    } else {
                        self.trigger_definition_at(m.column, m.row);
                    }
                    return;
                }
                // Merge-conflict accept actions on a conflict header row —
                // the [Accept …] spans the editor paints, VS Code's
                // merge-conflict CodeLens clicks. Checked before every other
                // editor click: the header row is marker text, so a caret
                // placement there serves nobody. A stale span cannot
                // mis-fire — `resolve_conflict_at` re-validates the row
                // against the live conflict set.
                // A code lens (#608) is a button, not text: run it.
                if in_editor && let Some(idx) = self.editor.code_lens_at(m.column, m.row) {
                    self.focus_pane(Pane::Editor);
                    self.run_code_lens(idx);
                    return;
                }
                // So is the inline blame (#1556): it opens its commit.
                if in_editor && self.editor.blame_commit_at(m.column, m.row).is_some() {
                    self.open_commit_for_line();
                    return;
                }
                if in_editor && self.editor.diff.is_none() {
                    let hit = self
                        .editor
                        .merge_action_spans
                        .iter()
                        .find(|(y, xs, _, _)| *y == m.row && xs.contains(&m.column))
                        .map(|(_, _, row, res)| (*row, *res));
                    if let Some((row, res)) = hit {
                        self.focus_pane(Pane::Editor);
                        self.editor.clear_selection();
                        self.editor.cursor_row = row;
                        self.editor.cursor_col = 0;
                        self.resolve_merge_at_cursor(res);
                        return;
                    }
                }
                // Merge editor (#253): a click on a source pane's
                // per-conflict checkbox toggles that side's inclusion
                // (both checked = Accept Combination), and one on an accept
                // button above a conflict sets it. Any other click in
                // the source-pane area is swallowed — the panes are
                // read-only; the Result below keeps normal click behavior.
                if in_editor && self.editor.merge.is_some() {
                    let (check_hit, in_panes) = self
                        .editor
                        .merge
                        .as_ref()
                        .map(|mv| {
                            (
                                mv.check_spans
                                    .iter()
                                    .find(|(y, xs, _, _)| *y == m.row && xs.contains(&m.column))
                                    .map(|(_, _, idx, side)| (*idx, *side)),
                                mv.last_panes_area.contains(ratatui::layout::Position {
                                    x: m.column,
                                    y: m.row,
                                }),
                            )
                        })
                        .unwrap_or((None, false));
                    if let Some((idx, side)) = check_hit {
                        self.focus_pane(Pane::Editor);
                        self.merge_toggle_side(idx, side);
                        return;
                    }
                    // An accept button above a conflict (#1169).
                    let action_hit = self.editor.merge.as_ref().and_then(|mv| {
                        mv.action_spans
                            .iter()
                            .find(|(y, xs, _, _)| *y == m.row && xs.contains(&m.column))
                            .map(|(_, _, idx, state)| (*idx, *state))
                    });
                    if let Some((idx, state)) = action_hit {
                        self.focus_pane(Pane::Editor);
                        self.merge_set_conflict(idx, state);
                        return;
                    }
                    if in_panes {
                        self.focus_pane(Pane::Editor);
                        return;
                    }
                }
                if !in_editor {
                    self.editor_click.clear();
                }
                if !in_tree {
                    self.tree_click.clear();
                }
                // Splitter hit-test runs before everything else: clicking the
                // single-column seam between sidebar and editor (or the
                // single-row seam between editor and terminal) starts a
                // resize drag instead of falling through to the underlying
                // pane's click handler.
                if let Some(x) = self.sidebar_splitter_x {
                    // Two-column hit-zone: the seam itself (`x`, the editor's
                    // left edge) and one column to the left (the tree's right
                    // border). Either grab starts a sidebar drag — a 1-cell
                    // target is too easy to miss.
                    //
                    // The OUTLINE panel draws no right border, so its scrollbar
                    // sits on the sidebar's rightmost column (`x - 1`), inside
                    // this zone. Exempt that column when it's the outline bar so
                    // the press reaches the scrollbar handler below instead of
                    // being swallowed as a resize.
                    //
                    // Likewise a lone terminal pane's left border shares this
                    // column, and its command decoration dots live there: a
                    // press on a dot cell must reach the decoration handler,
                    // not start a resize (the rest of the column still drags).
                    // The COMMITS graph draws no right border either, so its
                    // scrollbar shares the same column (Codeberg #41).
                    // Both scrollbar rects refresh only while their widget
                    // renders (outline under Explorer, the graph under
                    // Source Control), so each exemption is gated on its
                    // view — a stale rect from the last visit must not keep
                    // deadening the seam after a view switch.
                    let outline_bar = self.sidebar_view == SidebarView::Explorer
                        && rect_contains(self.outline.last_scrollbar, m.column, m.row);
                    let graph_bar = self.sidebar_view == SidebarView::SourceControl
                        && rect_contains(self.commit_graph.last_scrollbar, m.column, m.row);
                    // A collapsed terminal pane's one-column strip shares this
                    // zone too (#468): the leftmost pane starts on the seam
                    // itself, and with the side bar on the right the last
                    // pane ends one column short of it. The strip's only
                    // gesture is the click that unfolds it, so the seam must
                    // not take that click as a resize, or a folded pane on
                    // that edge can never be brought back. The rects are
                    // cleared whenever the panel is not painted, so a stale
                    // strip cannot deaden the seam later.
                    let on_strip = self
                        .terminal_strip_rects
                        .iter()
                        .any(|r| rect_contains(*r, m.column, m.row));
                    if (m.column == x || m.column == x.saturating_sub(1))
                        && !on_strip
                        && !outline_bar
                        && !graph_bar
                        && self.decoration_dot_at(m.column, m.row).is_none()
                    {
                        self.splitter_drag = Some(SplitterDrag::Sidebar);
                        return;
                    }
                }
                // Any seam between editor groups in the grid (any depth). Same
                // two-cell hit-zone as the other seams.
                if let Some(seam) = self.seam_at(m.column, m.row) {
                    self.splitter_drag = Some(SplitterDrag::EditorSeam(seam));
                    return;
                }
                // OPEN EDITORS: header toggles collapse; a row activates that
                // editor's tab; the scrollbar lane starts a thumb drag. Any
                // other click is swallowed so it can't fall through to a panel
                // visually beneath this one.
                if rect_contains(self.open_editors.last_area, m.column, m.row) {
                    if rect_contains(self.open_editors.last_scrollbar, m.column, m.row) {
                        self.open_editors.scroll_to_bar_y(m.row);
                        self.open_editors_scrollbar_drag = true;
                    } else if self.open_editors.hit_save_all(m.column, m.row) {
                        // VS Code's Save All on the section header (#852),
                        // checked first: the header row holds the button.
                        self.save_all();
                    } else if self.open_editors.hit_header(m.column, m.row) {
                        self.open_editors.toggle_collapse();
                    } else if let Some(idx) = self.open_editors.row_at(m.row)
                        && let Some(path) = self.open_editors.path_at(idx)
                    {
                        self.activate_open_editor(path);
                    }
                    return;
                }
                // TIMELINE: header toggles collapse; a row opens that commit's
                // diff for the file in a read-only editor tab.
                if rect_contains(self.timeline.last_area, m.column, m.row) {
                    if rect_contains(self.timeline.last_scrollbar, m.column, m.row) {
                        self.timeline.scroll_to_bar_y(m.row);
                        self.timeline_scrollbar_drag = true;
                    } else if self.timeline.hit_header(m.column, m.row) {
                        self.timeline.toggle_collapse();
                    } else if let Some(idx) = self.timeline.row_at(m.row)
                        && let Some((path, hash)) = self.timeline.diff_target(idx)
                    {
                        self.open_timeline_diff(path, hash);
                    }
                    return;
                }
                // AGENT LANE (#345): the header collapses it; a file row opens
                // that file's diff against its reviewed snapshot.
                if rect_contains(self.agent_lane_panel.last_area, m.column, m.row) {
                    if self.agent_lane_panel.hit_header(m.column, m.row) {
                        self.agent_lane_panel.toggle_collapse();
                    } else if let Some(crate::widgets::agent_lane::LaneRow::Agent {
                        name, ..
                    }) = self.agent_lane_panel.row_at(m.row).cloned()
                    {
                        // An agent's row folds its files away, or back.
                        self.agent_lane_panel.toggle_agent(&name);
                    } else if let Some(crate::widgets::agent_lane::LaneRow::File {
                        agent,
                        path,
                        label,
                        ..
                    }) = self.agent_lane_panel.row_at(m.row).cloned()
                    {
                        // The ● marks the file reviewed; the rest of the row
                        // opens its diff.
                        if self.agent_lane_panel.hit_dot(m.column, m.row) {
                            if self.mark_agent_file_reviewed(&agent, &path) {
                                self.status = format!("{label}: reviewed");
                            }
                        } else {
                            self.diff_agent_lane_row(&agent, &path);
                        }
                    }
                    return;
                }
                // DEPENDENCIES: display-only, so only the header (collapse)
                // and the scrollbar lane are interactive.
                if rect_contains(self.dependencies.last_area, m.column, m.row) {
                    if rect_contains(self.dependencies.last_scrollbar, m.column, m.row) {
                        self.dependencies.scroll_to_bar_y(m.row);
                        self.deps_scrollbar_drag = true;
                    } else if self.dependencies.hit_header(m.column, m.row) {
                        self.dependencies.toggle_collapse();
                    }
                    return;
                }
                // The OUTLINE section stacks just above the SYSTEM panel. A
                // click on its header toggles collapse; a click on a symbol row
                // jumps the editor to that symbol (recording nav history, like
                // go-to-definition); any other click inside its rect is
                // swallowed so it doesn't fall through to the tree above.
                if rect_contains(self.outline.last_area, m.column, m.row) {
                    if rect_contains(self.outline.last_scrollbar, m.column, m.row) {
                        self.outline.scroll_to_bar_y(m.row);
                        self.outline_scrollbar_drag = true;
                    } else if self.outline.hit_header(m.column, m.row) {
                        self.outline.toggle_collapse();
                    } else if let Some(idx) = self.outline.row_at(m.row)
                        && let Some((path, line, col)) = self.outline.jump_target(idx)
                        && !self.jump_in_scrub_view(line)
                    {
                        self.go_to_definition(path, line, col);
                    }
                    return;
                }
                // The panel group's tabs sit on the splitter row, so they must
                // win the hit-test before the splitter-drag handler claims the
                // click (same precedence as the "[+]" / "[-]" buttons below).
                // The panel's own controls share that row (#1980).
                if rect_contains(self.panel_maximize_button, m.column, m.row) {
                    self.toggle_terminal_maximize();
                    return;
                }
                if rect_contains(self.terminal_focus_button, m.column, m.row) {
                    self.focus_pane(Pane::Terminal);
                    self.toggle_terminal_pane_maximize();
                    return;
                }
                if rect_contains(self.problems_tab_rect, m.column, m.row) {
                    self.set_bottom_panel_tab(BottomPanelTab::Problems);
                    return;
                }
                if rect_contains(self.output_tab_rect, m.column, m.row) {
                    self.set_bottom_panel_tab(BottomPanelTab::Output);
                    return;
                }
                if rect_contains(self.terminal_tab_rect, m.column, m.row) {
                    self.set_bottom_panel_tab(BottomPanelTab::Terminal);
                    return;
                }
                if rect_contains(self.ports_tab_rect, m.column, m.row) {
                    self.set_bottom_panel_tab(BottomPanelTab::Ports);
                    return;
                }
                if rect_contains(self.captures_tab_rect, m.column, m.row) {
                    self.set_bottom_panel_tab(BottomPanelTab::Captures);
                    return;
                }
                // A click in the OUTPUT view hits its toolbar (dropdown / clear /
                // rpc / level) or dropdown list; if it lands in the log body the
                // widget doesn't consume it, so we just focus the panel pane.
                if self.bottom_panel_tab == BottomPanelTab::Output
                    && rect_contains(self.output.last_area, m.column, m.row)
                {
                    if !self.output.click(m.column, m.row) {
                        self.focus_pane(Pane::Terminal);
                    }
                    return;
                }
                // A click in the PROBLEMS list jumps the editor to that
                // diagnostic, or toggles a file group. Checked before the
                // terminal/splitter logic because the terminals are not
                // painted while this tab is active.
                if self.bottom_panel_tab == BottomPanelTab::Problems
                    && rect_contains(self.problems.last_area, m.column, m.row)
                {
                    use crate::widgets::problems::ProblemHit;
                    // Toolbar buttons (severity filter / group toggle) consume
                    // the click before it resolves to a row.
                    if self.problems.click(m.column, m.row) {
                        return;
                    }
                    match self.problems.hit_at(m.row) {
                        Some(ProblemHit::Header(path)) => self.problems.toggle_collapse(&path),
                        Some(ProblemHit::Diagnostic {
                            path,
                            line,
                            col,
                            col_utf16,
                        }) => {
                            // An LSP diagnostic's column is UTF-16; a build
                            // tool's is already a character column.
                            let opened = if col_utf16 {
                                self.open_at_utf16(&path, line, col)
                            } else {
                                self.open_at(&path, line as usize, col as usize)
                            };
                            if let Err(e) = opened {
                                self.status = format!("Open failed: {e}");
                            }
                        }
                        None => self.focus_pane(Pane::Terminal),
                    }
                    return;
                }
                // A click in the PORTS list selects that row (so the keyboard
                // actions act on it) or focuses the pane.
                if self.bottom_panel_tab == BottomPanelTab::Ports
                    && rect_contains(self.ports.last_area, m.column, m.row)
                {
                    match self.ports.row_at(m.column, m.row) {
                        Some(idx) => {
                            self.ports.select_index(idx);
                            // A second click on the same row opens that port,
                            // the same as pressing Enter on it.
                            let now = std::time::Instant::now();
                            if self
                                .ports_click
                                .is_double(now, m.column, m.row, m.modifiers)
                            {
                                self.ports_open_selected();
                                self.ports_click.clear();
                            } else {
                                self.ports_click.record(now, m.column, m.row, m.modifiers);
                            }
                        }
                        None => self.focus_pane(Pane::Terminal),
                    }
                    return;
                }
                // A click on a CAPTURES row jumps straight to the captured
                // line (iTerm2's captured-output click).
                if self.bottom_panel_tab == BottomPanelTab::Captures
                    && rect_contains(self.captures.last_area, m.column, m.row)
                {
                    match self.captures.row_at(m.column, m.row) {
                        Some(idx) => {
                            self.captures.select_index(idx);
                            self.captures_open_selected();
                        }
                        None => self.focus_pane(Pane::Terminal),
                    }
                    return;
                }
                // The "[+]" / "[-]" buttons sit on the same row as the
                // editor/terminal splitter, so they have to win the
                // hit-test before the splitter-drag handler claims this
                // click. Each terminal pane has its own pair so the user
                // can kill any pane, not only the active one.
                if let Some(idx) = self
                    .terminal_close_buttons
                    .iter()
                    .position(|r| rect_contains(*r, m.column, m.row))
                {
                    if self.close_terminal_at(idx) {
                        self.status =
                            format!("Closed terminal: {} remaining", self.terminals.len());
                    }
                    return;
                }
                if let Some(idx) = self
                    .terminal_profile_buttons
                    .iter()
                    .position(|r| rect_contains(*r, m.column, m.row))
                {
                    self.open_terminal_profile_picker(idx);
                    return;
                }
                if let Some(idx) = self
                    .terminal_collapse_buttons
                    .iter()
                    .position(|r| rect_contains(*r, m.column, m.row))
                {
                    self.toggle_terminal_collapse(idx);
                    return;
                }
                // A strip is one column wide, so it is tested before the
                // splitter and the pane body below: anything that claims the
                // click first makes a folded pane impossible to unfold.
                if let Some(idx) = self
                    .terminal_strip_rects
                    .iter()
                    .position(|r| rect_contains(*r, m.column, m.row))
                {
                    self.toggle_terminal_collapse(idx);
                    return;
                }
                if let Some(idx) = self
                    .terminal_rail_rects
                    .iter()
                    .position(|r| rect_contains(*r, m.column, m.row))
                {
                    self.active_terminal = idx;
                    self.focus_pane(Pane::Terminal);
                    // Holding the press arms a drag-reorder: crossing other
                    // rail rows live-moves this terminal between them.
                    self.terminal_drag_from = Some(idx);
                    return;
                }
                if let Some(idx) = self
                    .terminal_label_rects
                    .iter()
                    .position(|r| rect_contains(*r, m.column, m.row))
                {
                    // The pane name pill is the drag handle for reordering
                    // side-by-side panes (VS Code drags its terminal tabs the
                    // same way). The press itself focuses the pane, exactly
                    // like a click into its body would.
                    self.active_terminal = idx;
                    self.focus_pane(Pane::Terminal);
                    self.terminal_drag_from = Some(idx);
                    return;
                }
                if self
                    .terminal_add_buttons
                    .iter()
                    .any(|r| rect_contains(*r, m.column, m.row))
                {
                    match self.split_terminal() {
                        Ok(()) => {
                            self.status =
                                format!("Split terminal: {} active", self.live_terminal_count());
                        }
                        Err(e) => {
                            self.status = format!("Split terminal failed: {e}");
                        }
                    }
                    return;
                }
                if let Some(y) = self.terminal_splitter_y {
                    // Two-row hit-zone: the terminal's top border (`y`) and
                    // the editor / welcome's bottom border (`y - 1`).
                    // Symmetric with the sidebar drag — either edge of the
                    // visible seam grabs the splitter.
                    //
                    // Gate by column too: the editor / terminal splitter
                    // only spans the RIGHT column (everything at or past
                    // `sidebar_splitter_x`). Without this gate, a click
                    // on a tree row in the LEFT column whose y lines up
                    // with the splitter is silently hijacked into a
                    // splitter-drag and never reaches `node_at_y` -
                    // exactly the dead-zone the user hit on the last
                    // two files in the Explorer.
                    let in_splitter_x_range = self.sidebar_splitter_x.is_none_or(|x| m.column >= x);
                    if in_splitter_x_range && (m.row == y || m.row == y.saturating_sub(1)) {
                        self.splitter_drag = Some(SplitterDrag::Terminal);
                        return;
                    }
                }
                // Activity-bar hit-test takes precedence over the side panel.
                if let Some(icon) = self.activity_icon_at(m.column, m.row) {
                    match icon.view() {
                        Some(view) => self.click_activity_view(view),
                        None if icon == ActivityIcon::Sponsor => self.open_sponsor_page(),
                        None => self.open_settings_menu(),
                    }
                    return;
                }
                if rect_contains(self.welcome_tour_button, m.column, m.row) {
                    self.start_demo();
                    return;
                }
                if self.click_welcome_start(m.column, m.row) {
                    return;
                }
                if in_tree_scrollbar {
                    self.focus_pane(Pane::Tree);
                    self.tree.scroll_to_bar_y(m.row);
                    self.scrollbar_drag = Some(Pane::Tree);
                    self.tree_click.clear();
                    return;
                }
                if in_remote_scrollbar {
                    self.focus_pane(Pane::Tree);
                    self.remote.scroll_to_bar_y(m.row);
                    self.scrollbar_drag = Some(Pane::Tree);
                    self.tree_click.clear();
                    return;
                }
                if in_search_scrollbar {
                    self.focus_pane(Pane::Tree);
                    self.search.scroll_to_bar_y(m.row);
                    self.scrollbar_drag = Some(Pane::Tree);
                    self.tree_click.clear();
                    return;
                }
                if rect_contains(self.minimap_img_rect, m.column, m.row) {
                    self.focus_pane(Pane::Editor);
                    self.minimap_scroll_to_row(m.row);
                    self.minimap_drag = true;
                    self.editor_click.clear();
                    return;
                }
                if in_editor_scrollbar {
                    self.focus_pane(Pane::Editor);
                    self.editor.scroll_to_bar_y(m.row);
                    self.scrollbar_drag = Some(Pane::Editor);
                    self.editor_click.clear();
                    self.poke_cursor();
                    return;
                }
                if in_editor_hscrollbar {
                    self.focus_pane(Pane::Editor);
                    self.editor.scroll_to_bar_x(m.column);
                    self.editor_hscrollbar_drag = true;
                    self.editor_click.clear();
                    self.poke_cursor();
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::Search {
                    self.focus_pane(Pane::Tree);
                    if self.search.paste_button_at(m.column, m.row) {
                        let text = self.clipboard_text();
                        self.paste_clipboard_into_search(text.as_deref());
                        return;
                    }
                    // Header "Refresh" re-runs the current query.
                    if self.search.refresh_at(m.column, m.row) {
                        self.submit_search_query();
                        return;
                    }
                    // Header "Clear Search Results" wipes the query + results.
                    if self.search.clear_at(m.column, m.row) {
                        self.search.clear_all();
                        return;
                    }
                    // Left chevron expands/collapses the Replace row.
                    if self.search.chevron_at(m.column, m.row) {
                        self.search.toggle_replace();
                        return;
                    }
                    // "" ellipsis expands/collapses files-to-include/exclude.
                    if self.search.ellipsis_at(m.column, m.row) {
                        self.search.toggle_details();
                        return;
                    }
                    // Replace-all action icon runs Replace All across results.
                    if self.search.replace_all_at(m.column, m.row) {
                        self.run_search_replace_all();
                        return;
                    }
                    if self.search.project_exclude_toggle_at(m.column, m.row) {
                        self.toggle_project_excludes();
                        return;
                    }
                    // Click inside one of the input boxes focuses that field.
                    if let Some(field) = self.search.field_at(m.column, m.row) {
                        self.search.focus_field(field);
                        // A click in the text puts the caret under it.
                        self.search.click_caret(m.column);
                        return;
                    }
                    if let Some(t) = self.search.toggle_at(m.column, m.row) {
                        self.toggle_search_opt(t);
                        return;
                    }
                    // Click on a result row: open it. Mirrors the Explorer's
                    // preview/pin gesture — a single click opens the hit in
                    // the replaceable preview slot, a double-click pins the
                    // tab so moving on to the next result opens beside it
                    // instead of replacing the file under review.
                    if let Some(idx) = self.search.hit_at_y(m.row) {
                        self.search.selected = idx;
                        let now = std::time::Instant::now();
                        let is_double =
                            self.tree_click.is_double(now, m.column, m.row, m.modifiers);
                        if let Some(hit) = self.search.selected_hit().cloned() {
                            self.open_search_hit(&hit);
                            if is_double {
                                self.editor.pin_active();
                            }
                        }
                        if is_double {
                            self.tree_click.clear();
                        } else {
                            self.tree_click.record(now, m.column, m.row, m.modifiers);
                        }
                    } else {
                        // Click on the input/header area: just focus search.
                    }
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::SourceControl {
                    self.focus_pane(Pane::Tree);
                    // COMMITS graph: header toggles collapse; a row opens that
                    // commit's full patch in a read-only tab. Checked first —
                    // its rect is carved out of the Source Control strip.
                    if rect_contains(self.commit_graph.last_area, m.column, m.row) {
                        if rect_contains(self.commit_graph.last_scrollbar, m.column, m.row) {
                            self.commit_graph.scroll_to_bar_y(m.row);
                            self.graph_scrollbar_drag = true;
                        } else if self.commit_graph.hit_header(m.column, m.row) {
                            self.commit_graph.toggle_collapse();
                        } else if let Some(row) = self.commit_graph.row_index_at(m.row) {
                            // The clicked commit is picked and COMMITS takes
                            // the keys, as a click on a change row does for
                            // the change list (#871).
                            self.source_control.keys = KeyStop::Commits;
                            self.commit_graph.select(row);
                            if let Some((hash, short)) = self
                                .commit_graph
                                .selected_commit()
                                .map(|c| (c.hash.clone(), c.short_hash.clone()))
                            {
                                self.open_commit_patch(&hash, &short);
                            }
                        }
                        return;
                    }
                    if let Some(root) = self.source_control.click_repository(m.column, m.row) {
                        // Pin the panel to the clicked repository (#161);
                        // the next cross-folder focus releases it. Its row
                        // takes the keys, as a click on a change does for
                        // the change list (#871).
                        let row = self
                            .source_control
                            .repositories
                            .iter()
                            .position(|r| r.workspace_root == root);
                        self.pin_scm_repository(root);
                        if let Some(row) = row {
                            self.source_control.keys = KeyStop::Repositories;
                            self.source_control.repo_cursor = row;
                        }
                        return;
                    }
                    if self.source_control.click_more(m.column, m.row) {
                        self.open_scm_actions_menu(false);
                        return;
                    }
                    if self.source_control.click_header_refresh(m.column, m.row) {
                        self.refresh_source_control();
                        self.status = String::from("Refreshed Source Control");
                        return;
                    }
                    if self.source_control.click_commit_caret(m.column, m.row) {
                        self.open_commit_actions_menu(false);
                        return;
                    }
                    if self.source_control.click_init_repo_button(m.column, m.row) {
                        self.initialize_repository();
                        return;
                    }
                    if let Some(repo) = self.source_control.click_nested_repo(m.column, m.row) {
                        self.change_workspace_root(repo);
                        return;
                    }
                    if self.source_control.click_button(m.column, m.row) {
                        self.commit_source_control();
                        return;
                    }
                    // A click anywhere else in the view takes the keys from
                    // COMMITS and REPOSITORIES (#871): to the message box (or
                    // the card, with no repository), or to the change list
                    // when the click lands on one of its rows.
                    if matches!(
                        self.source_control.keys,
                        KeyStop::Commits | KeyStop::Repositories
                    ) {
                        self.source_control.keys = KeyStop::Message;
                        self.source_control.settle_keys();
                    }
                    if self.source_control.click_input(m.column, m.row) {
                        self.source_control.keys = KeyStop::Message;
                        return;
                    }
                    if self.source_control.click_branch(m.column, m.row) {
                        self.open_branch_picker();
                        return;
                    }
                    if let Some(idx) = self.source_control.click_discard_action(m.column, m.row) {
                        self.request_discard_source_control_entry(idx);
                        return;
                    }
                    if let Some(idx) = self.source_control.click_stage_action(m.column, m.row) {
                        self.stage_source_control_entry(idx);
                        return;
                    }
                    if let Some(idx) = self.source_control.click_unstage_action(m.column, m.row) {
                        self.unstage_source_control_entry(idx);
                        return;
                    }
                    if let Some(idx) = self.source_control.entry_at_y(m.row) {
                        self.source_control.keys = KeyStop::List;
                        self.open_source_control_entry(idx);
                    }
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::RunDebug {
                    self.focus_pane(Pane::Tree);
                    if let Some((index, remove)) = self.run_debug.breakpoint_at(m.column, m.row) {
                        self.breakpoint_list_click(index, remove);
                        return;
                    }
                    if self.run_debug.debug_active {
                        if let Some(control) = self.run_debug.debug_control_at(m.column, m.row) {
                            self.run_debug_control(control);
                        } else if let Some(widx) = self.run_debug.watch_remove_at(m.column, m.row) {
                            self.remove_watch_expression(widx);
                            self.refresh_debug_panel();
                        } else if let Some(idx) = self.run_debug.debug_row_at(m.row) {
                            self.run_debug.put_row_cursor(idx);
                            self.debug_panel_click(idx);
                        } else if self.run_debug.prompt_at(m.column, m.row) {
                            // The prompt takes the keys back (#2748), so
                            // Backspace and Enter edit and run what is typed.
                            self.run_debug.row_cursor = None;
                        }
                    } else if self.run_debug.click_button(m.column, m.row) {
                        self.run_debug_button_action();
                    } else if self.run_debug.click_config(m.column, m.row) {
                        self.open_debug_config_picker();
                    }
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::Extensions {
                    self.focus_pane(Pane::Tree);
                    // The filter's clear (✕) button empties the search.
                    if self.extensions.click_clear(m.column, m.row) {
                        self.extensions.clear_filter();
                        return;
                    }
                    // The refresh (⟳) button re-pulls the signed remote index.
                    if self.extensions.click_refresh(m.column, m.row) {
                        self.refresh_extension_index();
                        return;
                    }
                    // Clicking a row selects it; clicking its trash button
                    // uninstalls it; clicking its toggle switch (the right-edge
                    // pill) flips the extension. The trash zone sits left of the
                    // switch, so the two never overlap.
                    if let Some(idx) = self.extensions.row_at(m.row) {
                        let on_trash = self.extensions.click_uninstall(m.column, m.row);
                        let on_switch = self.extensions.click_action(m.column, m.row);
                        self.extensions.select(idx);
                        if on_trash {
                            self.uninstall_selected_extension();
                        } else if on_switch {
                            self.toggle_selected_extension();
                        }
                    }
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::CodeQL {
                    self.focus_pane(Pane::Tree);
                    if let Some((row, hit)) = self.codeql.hit_at(m.column, m.row) {
                        self.codeql.selected = row;
                        self.activate_codeql(hit);
                    }
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::Testing {
                    self.focus_pane(Pane::Tree);
                    // The scrollbar lane starts a thumb drag; elsewhere the play
                    // glyph runs a case or suite while a case's name reveals its
                    // source (VS Code's split: the label shows, the icon runs).
                    // Empty space does nothing.
                    if rect_contains(self.testing.last_scrollbar, m.column, m.row) {
                        self.testing.scroll_to_bar_y(m.row);
                        self.testing_scrollbar_drag = true;
                    } else if rect_contains(self.testing.last_watch_all, m.column, m.row) {
                        self.toggle_test_watch(crate::testing::watch::WatchScope::All);
                    } else if rect_contains(self.testing.last_cover_all, m.column, m.row) {
                        self.run_all_tests_with_coverage();
                    } else if rect_contains(self.testing.last_output_hint, m.column, m.row) {
                        self.show_test_runner_output();
                    } else if rect_contains(self.testing.last_stop, m.column, m.row) {
                        self.cancel_test_run();
                    } else {
                        use crate::widgets::testing::RowHit;
                        let hit = self.testing.hit_at(m.column, m.row);
                        // A run glyph on a row the panel still shows from the
                        // folder before the focus moved (#1973): the follow is
                        // worked out off the UI thread, so the panel can lag
                        // the focus. The settle rebinds and clears it, and
                        // that row's test is not the new folder's to run.
                        let runs = matches!(
                            hit,
                            Some(
                                RowHit::RunCase(_)
                                    | RowHit::RunSuite(_)
                                    | RowHit::CoverCase(_)
                                    | RowHit::CoverSuite(_)
                            )
                        );
                        if runs && self.settle_followed_test_root() {
                            self.status = String::from(
                                "Testing moved to the focused file's folder; that test was not run",
                            );
                            return;
                        }
                        match hit {
                            Some(crate::widgets::testing::RowHit::ToggleWatch(scope)) => {
                                self.toggle_test_watch(scope)
                            }
                            Some(crate::widgets::testing::RowHit::RunCase(name)) => {
                                self.run_test(name)
                            }
                            Some(crate::widgets::testing::RowHit::ShowCase(name)) => {
                                self.jump_to_test_source(name)
                            }
                            Some(crate::widgets::testing::RowHit::RunSuite(suite)) => {
                                self.run_suite(suite)
                            }
                            Some(crate::widgets::testing::RowHit::CoverCase(name)) => {
                                self.run_scope_with_coverage(name, false)
                            }
                            Some(crate::widgets::testing::RowHit::CoverSuite(suite)) => {
                                self.run_scope_with_coverage(suite, true)
                            }
                            None => {}
                        }
                    }
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::Remote {
                    self.focus_pane(Pane::Tree);
                    if rect_contains(self.remote.header_chevron_btn, m.column, m.row) {
                        self.remote.toggle_collapsed();
                        self.status = if self.remote.collapsed {
                            String::from("Collapsed SSH section")
                        } else {
                            String::from("Expanded SSH section")
                        };
                        return;
                    }
                    if rect_contains(self.remote.header_add_btn, m.column, m.row)
                        || rect_contains(self.remote.empty_primary_btn, m.column, m.row)
                        || rect_contains(self.remote.empty_secondary_btn, m.column, m.row)
                    {
                        self.open_ssh_config_in_editor();
                        return;
                    }
                    if rect_contains(self.remote.header_refresh_btn, m.column, m.row) {
                        self.remote.refresh();
                        self.status = String::from("Reloaded SSH hosts from ~/.ssh/config");
                        return;
                    }
                    if rect_contains(self.remote.empty_learn_link, m.column, m.row) {
                        self.open_ssh_learn_more();
                        return;
                    }
                    if let Some(idx) = self.remote.target_at_y(m.row) {
                        self.remote.select(idx);
                        let now = std::time::Instant::now();
                        let is_double =
                            self.tree_click.is_double(now, m.column, m.row, m.modifiers);
                        if is_double {
                            if let Some(target) = self.remote.selected_target().cloned() {
                                self.request_remote_launch(target.alias, None);
                            }
                            self.tree_click.clear();
                        } else {
                            self.tree_click.record(now, m.column, m.row, m.modifiers);
                        }
                    }
                    return;
                }
                if in_tree && self.sidebar_view == SidebarView::Explorer {
                    self.focus_pane(Pane::Tree);
                    // Header toolbar (root folder row): New File / New Folder /
                    // Refresh / Collapse Folders. Checked before row hit-testing
                    // so a click on an icon never falls through to the node.
                    if rect_contains(self.tree.header_new_file_btn, m.column, m.row) {
                        let target = self.explorer_create_target_dir();
                        self.open_create_prompt(CreateKind::File, target);
                        return;
                    }
                    if rect_contains(self.tree.header_new_folder_btn, m.column, m.row) {
                        let target = self.explorer_create_target_dir();
                        self.open_create_prompt(CreateKind::Folder, target);
                        return;
                    }
                    if rect_contains(self.tree.header_refresh_btn, m.column, m.row) {
                        self.tree.refresh_children(0);
                        self.status = String::from("Refreshed Explorer");
                        return;
                    }
                    if rect_contains(self.tree.header_collapse_btn, m.column, m.row) {
                        self.tree.collapse_all();
                        self.status = String::from("Collapsed all folders");
                        return;
                    }
                    // The ⋯ "Views and More Actions" button on the EXPLORER
                    // title line opens the menu that toggles which sub-views
                    // (Open Editors / Folders / Outline / Timeline /
                    // Dependencies) are stacked in the panel.
                    if rect_contains(self.tree.header_views_btn, m.column, m.row) {
                        self.open_explorer_views_menu();
                        return;
                    }
                    // A left-click on the sticky band jumps: select the
                    // pinned ancestor and scroll it to the top of the tree
                    // (#117). Checked before the row map, which would answer
                    // for the covered row.
                    if let Some(idx) = self.tree.sticky_row_at(m.row) {
                        self.tree.select_replace(idx);
                        self.tree.scroll = idx;
                        self.tree_click.clear();
                        return;
                    }
                    if let Some(idx) = self.tree.node_at_y(m.row) {
                        let now = std::time::Instant::now();
                        let is_double =
                            self.tree_click.is_double(now, m.column, m.row, m.modifiers);
                        let has_shift = m.modifiers.contains(KeyModifiers::SHIFT);
                        // macOS terminals (iTerm2, Terminal.app) never put the
                        // Cmd bit on mouse events — the SGR mouse encoding only
                        // carries Shift/Alt/Ctrl. Treat Alt (Option on macOS)
                        // and Ctrl as the cherry-pick modifier so the gesture
                        // actually reaches the app.
                        let has_toggle_mod = m.modifiers.contains(KeyModifiers::ALT)
                            || m.modifiers.contains(KeyModifiers::CONTROL);
                        // Shift-click extends the range from the anchor. No
                        // activation, no drag.
                        if has_shift {
                            self.tree.extend_to(idx);
                            self.tree_click.clear();
                            self.tree_drag = None;
                            return;
                        }
                        // Alt/Ctrl-click: defer the toggle until mouse-up so a
                        // movement in between can promote the gesture into an
                        // Alt-drag (copy) instead. Don't activate (no file
                        // open, no folder toggle) and don't include the row
                        // in the multi-selection yet.
                        if has_toggle_mod {
                            self.tree_click.clear();
                            let mut drag_paths = self.tree.action_paths();
                            let path_clicked = self.tree.nodes[idx].path.clone();
                            if !drag_paths.iter().any(|p| p == &path_clicked) {
                                drag_paths.push(path_clicked);
                            }
                            self.tree_drag = Some(ExplorerDrag {
                                paths: drag_paths,
                                target_idx: None,
                                armed: false,
                                started_at: (now, m.column, m.row),
                                start_idx: idx,
                                toggle_on_release: true,
                            });
                            return;
                        }
                        // Plain click: if the row is already in the marked set,
                        // keep the marks intact so a subsequent drag carries
                        // every selected entry; otherwise collapse selection
                        // to this single row.
                        let already_marked = self.tree.marked.contains(&self.tree.nodes[idx].path);
                        if !already_marked {
                            self.tree.select_replace(idx);
                        } else {
                            self.tree.selected = idx;
                            self.tree.anchor = idx;
                        }
                        let clicked_is_dir =
                            self.tree.nodes.get(idx).is_some_and(|node| node.is_dir);
                        if clicked_is_dir && is_double {
                            self.tree_click.clear();
                            self.tree_drag = None;
                            return;
                        }
                        // Arm a potential drag-source. Movement off this cell
                        // promotes it into a real drag in the Drag handler;
                        // a stationary release keeps the click semantics.
                        let drag_paths = self.tree.action_paths();
                        if !drag_paths.is_empty() {
                            self.tree_drag = Some(ExplorerDrag {
                                paths: drag_paths,
                                target_idx: None,
                                armed: false,
                                started_at: (now, m.column, m.row),
                                start_idx: idx,
                                toggle_on_release: false,
                            });
                        }
                        if let Some(path) = self.tree.activate() {
                            let result = if is_double {
                                self.editor.open_pinned(&path)
                            } else {
                                self.editor.open_preview(&path)
                            };
                            match result {
                                Ok(()) => {
                                    self.sync_open_file_poll_mtime();
                                    self.status = self.editor.status.clone();
                                    // Opening a FILE hands the keyboard to the
                                    // editor (VS Code parity), single click or
                                    // double: otherwise the new tab's own
                                    // navigation keys are dead and a PDF sits
                                    // frozen on page 1 while the arrows walk
                                    // the file list. `activate()` returns None
                                    // for a folder, so a folder click never
                                    // reaches here and browsing keeps focus.
                                    self.focus_pane(Pane::Editor);
                                    self.poke_cursor();
                                }
                                Err(e) => self.status = format!("Error: {e}"),
                            }
                        }
                        if is_double {
                            self.tree_click.clear();
                        } else {
                            self.tree_click.record(now, m.column, m.row, m.modifiers);
                        }
                    }
                } else if in_editor_pane && !in_editor {
                    let crumb = self.editor.breadcrumb_index_at(m.column, m.row);
                    if let Some(i) = crumb.filter(|_| self.scrub_view.is_none()) {
                        // A crumb opens its picker (#1608): a folder's
                        // entries, the file's folder, or the symbol's
                        // siblings.
                        self.focus_pane(Pane::Editor);
                        self.open_breadcrumb_picker(i);
                    } else if let Some((line, col)) =
                        self.editor.breadcrumb_target_at(m.column, m.row)
                    {
                        // While scrubbing a symbol crumb jumps in the
                        // commit's view, as before.
                        self.focus_pane(Pane::Editor);
                        if !self.jump_in_scrub_view(line)
                            && let Some(path) = self.editor.path.clone()
                        {
                            self.go_to_definition(path, line, col);
                        }
                        self.poke_cursor();
                    } else if self.editor.tab_list_button_at(m.column, m.row) {
                        self.focus_pane(Pane::Editor);
                        self.open_tab_picker();
                    } else if let Some(idx) = self.editor.close_at(m.column, m.row) {
                        // The close cell of a pinned tab is an unpin button (it
                        // shows a thumb-tack, not the close cross), matching VS
                        // Code: a pinned tab is never closed by that click.
                        if self.editor.is_pinned(idx) {
                            self.editor.toggle_pin(idx);
                            self.status = String::from("Unpinned tab");
                            self.poke_cursor();
                        } else {
                            self.request_close_tab(idx);
                        }
                    } else if let Some(idx) = self.editor.tab_at(m.column, m.row) {
                        self.focus_pane(Pane::Editor);
                        if self.editor.active_index() != idx {
                            self.editor.select(idx);
                            self.sync_open_file_poll_mtime();
                        }
                        // A drag from here reorders the tab (#1565).
                        self.editor_tab_drag_from = Some(idx);
                        self.poke_cursor();
                    }
                } else if in_editor {
                    self.focus_pane(Pane::Editor);
                    // A click on a pinned sticky-scroll header jumps to that
                    // scope instead of moving the caret into the covered line.
                    if let Some(line) = self.editor.sticky_line_at(m.column, m.row) {
                        if let Some(path) = self.editor.path.clone() {
                            self.go_to_definition(path, line, 0);
                        }
                        self.poke_cursor();
                        return;
                    }
                    // Mirror of the terminal branch: a selection lives in one
                    // pane only, so anchoring one in the editor drops any
                    // leftover terminal selection (issue #23).
                    self.terminal_mut().clear_selection();
                    // Sheet tab (#177): a click selects the cell under the
                    // pointer; the second click on the SAME selected cell
                    // opens the in-grid editor (editable kinds only).
                    if let Some(view) = self.editor.sheet.as_mut() {
                        if let Some((r, c)) = sheet_cell_at(view, m.column, m.row) {
                            let editable = matches!(
                                view.kind,
                                crate::sheet::SheetKind::Csv
                                    | crate::sheet::SheetKind::Tsv
                                    | crate::sheet::SheetKind::Xlsx
                            );
                            let current = view.current_sheet;
                            // An in-progress edit COMMITS when the
                            // click lands elsewhere (the spreadsheet
                            // convention; #193 review - discarding it
                            // silently lost the typed value).
                            let (pr, pc) = {
                                let d = &view.sheets[current];
                                (d.cur_row, d.cur_col)
                            };
                            let was_here = pr == r && pc == c;
                            if !was_here && view.commit_edit().is_some() {
                                self.editor.mark_dirty();
                            }
                            let view = self.editor.sheet.as_mut().expect("checked");
                            let data = &mut view.sheets[current];
                            data.cur_row = r;
                            data.cur_col = c;
                            if was_here && editable && view.editing.is_none() {
                                let value = view.sheets[current].cell(r, c).to_string();
                                let cursor = value.len();
                                view.editing = Some(crate::sheet::CellEdit { value, cursor });
                            }
                        }
                        self.poke_cursor();
                        return;
                    }
                    // Fleet tiles (#363): a click selects a tile.
                    if let Some(view) = self.editor.fleet.as_mut() {
                        if let Some(i) = view.tile_at(m.column, m.row) {
                            view.selected = i;
                        }
                        self.poke_cursor();
                        return;
                    }
                    // Archive browser (#179): click selects; a second
                    // click on the selected row opens the member.
                    if let Some(view) = self.editor.archive.as_mut() {
                        if view.rows_visible > 0
                            && m.row >= view.rows_top
                            && m.row < view.rows_top + view.rows_visible
                        {
                            let idx = view.scroll + (m.row - view.rows_top) as usize;
                            if idx < view.entries.len() {
                                if view.selected == idx {
                                    self.open_selected_archive_member();
                                } else {
                                    view.selected = idx;
                                }
                            }
                        }
                        self.poke_cursor();
                        return;
                    }
                    // SARIF viewer (#577): click selects a row; a second
                    // click on the selected row opens it (a result) or
                    // folds it (a group).
                    if let Some(view) = self.editor.sarif.as_mut() {
                        if view.rows_visible > 0
                            && m.row >= view.rows_top
                            && m.row < view.rows_top + view.rows_visible
                            && m.column >= view.list_x
                            && m.column < view.list_x + view.list_width
                        {
                            let idx = view.scroll + (m.row - view.rows_top) as usize;
                            if idx < view.rows().len() {
                                if view.selected == idx {
                                    if view.selected_entry().is_some() {
                                        self.open_selected_sarif_result();
                                    } else {
                                        view.toggle_fold();
                                    }
                                } else {
                                    view.selected = idx;
                                }
                            }
                        }
                        self.poke_cursor();
                        return;
                    }
                    // Hex tab (#172): a click on a byte cell — hex grid or
                    // ASCII gutter — parks the cursor there; Shift extends
                    // the selection; a drag from here extends it live.
                    if let Some(view) = self.editor.hex.as_mut() {
                        let rows = (view.layout.data_rows as usize).max(1);
                        if let Some(off) = view.hit_test(m.column, m.row) {
                            let select = m.modifiers.contains(KeyModifiers::SHIFT);
                            view.set_cursor(off, select, rows);
                        }
                        self.poke_cursor();
                        return;
                    }
                    // PDF preview: a plain click activates the link under the
                    // pointer — an external URL opens in the browser, an
                    // internal target flips to its page — matching every
                    // desktop PDF viewer. Only links anchored to text are
                    // found (pdftohtml reports link annotations through the
                    // text runs they cover), which is what hyperref/beamer
                    // emit. A non-link click just focuses the pane, above.
                    if self.editor.image.as_ref().is_some_and(|i| i.pdf.is_some()) {
                        match self.pdf_link_target_at(m.column, m.row) {
                            Some(crate::pdf::LinkTarget::Url(url)) => {
                                self.status = match open_url(&url) {
                                    Ok(()) => format!("Opened {url}"),
                                    Err(e) => format!("Could not open {url}: {e}"),
                                };
                            }
                            Some(crate::pdf::LinkTarget::Page(page)) => self.jump_pdf_page(page),
                            None => {}
                        }
                        return;
                    }
                    // Diff header arrows: take precedence over selection so a
                    // click on the ‹ / › glyph jumps to prev/next change
                    // instead of anchoring a selection at the header row.
                    if self.editor.diff.is_some() {
                        if rect_contains(self.editor.diff_prev_arrow, m.column, m.row) {
                            self.jump_diff_change(false);
                            return;
                        }
                        if rect_contains(self.editor.diff_next_arrow, m.column, m.row) {
                            self.jump_diff_change(true);
                            return;
                        }
                        let now = std::time::Instant::now();
                        let is_double =
                            self.editor_click
                                .is_double(now, m.column, m.row, m.modifiers);
                        let last_inner = self.editor.last_inner;
                        self.editor.sync_tab_cells();
                        if let Some(diff) = self.editor.diff.as_mut() {
                            if let Some((side, row_idx, char_col)) =
                                crate::widgets::editor::diff_hit_test(
                                    diff, last_inner, m.column, m.row,
                                )
                            {
                                if is_double {
                                    diff.select_word_at(side, row_idx, char_col);
                                } else {
                                    diff.start_selection(side, row_idx, char_col);
                                }
                            } else {
                                diff.clear_selection();
                            }
                        }
                        if is_double {
                            self.editor_click.clear();
                        } else {
                            self.editor_click.record(now, m.column, m.row, m.modifiers);
                        }
                        self.poke_cursor();
                        return;
                    }
                    // AI-stream stop button in the sign margin: cancel the
                    // pilot's stream (it reverts the streamed text). Checked
                    // before the play glyph, whose cell it borrows while a
                    // stream is live.
                    if self.editor.stream_stop_at(m.column, m.row) {
                        self.collab_cancel_stream();
                        return;
                    }
                    // Comment-box surfaces: ✕ Ignore dismisses the box; the
                    // footer's reply field (or the body) focuses it, placing
                    // the caret where the click landed in the field.
                    if let Some((id, hit)) = self.editor.comment_box_hit(m.column, m.row) {
                        use crate::widgets::editor::CommentHit;
                        match hit {
                            CommentHit::Ignore => self.ignore_comment_box(id),
                            CommentHit::Reply | CommentHit::Body => {
                                let keep = self.editor.comment_focus.take().filter(|f| f.id == id);
                                let mut focus =
                                    keep.unwrap_or(crate::widgets::editor::CommentFocus {
                                        id,
                                        reply: String::new(),
                                        cursor: 0,
                                    });
                                if hit == CommentHit::Reply {
                                    let text_x = self.editor.last_inner.x
                                        + self.editor.last_gutter_width
                                        + 1;
                                    // `╰ ❯ ` occupies 4 cells before the field.
                                    let rel = (m.column.saturating_sub(text_x) as usize)
                                        .saturating_sub(4);
                                    focus.cursor = rel.min(focus.reply.chars().count());
                                } else {
                                    focus.cursor = focus.reply.chars().count();
                                }
                                self.editor.comment_focus = Some(focus);
                            }
                        }
                        return;
                    }
                    // Any other editor press releases a focused reply field.
                    if self.editor.comment_focus.is_some() {
                        self.editor.comment_focus = None;
                    }
                    // Gutter play glyph: run the test fn defined on the
                    // clicked line (VS Code's run bead); Alt+click debugs it
                    // instead. Sits in the sign margin one cell left of the
                    // fold chevron, checked before the editor-body Alt+click
                    // caret so the modifier never adds a caret here.
                    if let Some(name) = self.editor.test_glyph_at(m.column, m.row) {
                        if m.modifiers.contains(KeyModifiers::ALT) {
                            self.debug_named_test(name);
                        } else {
                            self.run_named_test(name);
                        }
                        return;
                    }
                    // Fold chevron in the gutter: toggle the fold, don't anchor
                    // a selection or move the cursor into the (possibly hidden)
                    // body.
                    if self.editor.fold_chevron_at(m.column, m.row) {
                        self.poke_cursor();
                        return;
                    }
                    // Breakpoint lane (#1544): a click in the sign cell
                    // toggles a breakpoint on that line, as a click in VS
                    // Code's glyph margin does, and leaves the caret where
                    // it is. It used to move the caret to column 1 and set
                    // nothing. After the play glyph and the chevron, which
                    // own their cells; Shift and Alt keep their selections.
                    if !m
                        .modifiers
                        .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
                        && let Some(line0) = self.editor.breakpoint_lane_line_at(m.column, m.row)
                    {
                        self.debug_toggle_breakpoint_line(line0 + 1);
                        self.breakpoint_lane_press = true;
                        self.poke_cursor();
                        return;
                    }
                    // A press on a line number selects that line, and a drag
                    // down the numbers selects whole lines, as in VS Code
                    // (#1570). It used to put the caret at column 1. Only
                    // the number's own columns: a blank chevron cell, the
                    // spacer and a gutter with no numbers keep that click.
                    if m.modifiers.is_empty()
                        && self.editor.line_number_line_at(m.column, m.row).is_some()
                    {
                        self.editor_line_drag =
                            Some(self.editor.select_lines_at(None, m.column, m.row));
                        self.editor_click.clear();
                        self.poke_cursor();
                        return;
                    }
                    // VS Code Shift+Alt+drag: start a column (box) selection.
                    // Checked before the plain Alt+click caret so the two
                    // modifiers don't both fire.
                    if m.modifiers.contains(KeyModifiers::ALT)
                        && m.modifiers.contains(KeyModifiers::SHIFT)
                        && self.editor.begin_box_select(m.column, m.row)
                    {
                        self.poke_cursor();
                        return;
                    }
                    // VS Code Alt+click: add (or toggle off) a secondary caret
                    // at the click instead of moving the single cursor.
                    if m.modifiers.contains(KeyModifiers::ALT)
                        && !m.modifiers.contains(KeyModifiers::SHIFT)
                        && self.editor.add_caret_at_screen(m.column, m.row)
                    {
                        self.poke_cursor();
                        return;
                    }
                    let now = std::time::Instant::now();
                    let is_double = self
                        .editor_click
                        .is_double(now, m.column, m.row, m.modifiers);
                    if self
                        .editor_click
                        .is_triple(now, m.column, m.row, m.modifiers)
                    {
                        // A third click selects the whole line (#1570); a
                        // drag from it extends line by line.
                        self.editor_line_drag =
                            Some(self.editor.select_lines_at(None, m.column, m.row));
                        self.editor_click.clear();
                    } else if is_double {
                        self.editor.select_word_at(m.column, m.row);
                        self.editor_click.record_double(now, m.column, m.row);
                    } else {
                        // Anchor a fresh selection at the click; a drag widens it,
                        // a clean click ends up cleared on mouse-up.
                        self.editor.mouse_down(m.column, m.row);
                        self.editor_click.record(now, m.column, m.row, m.modifiers);
                    }
                    self.poke_cursor();
                } else if let Some(idx) = terminal_hit {
                    self.activate_terminal_pane(idx);
                    self.focus_pane(Pane::Terminal);
                    // Click on a gutter decoration dot (the pane's left
                    // border): open the command's action menu (VS Code's
                    // decoration menu), headed by its outcome.
                    if let Some(d) = self.terminal().decoration_at_screen(m.column, m.row) {
                        self.open_command_decoration_menu((m.column, m.row), d);
                        return;
                    }
                    // Cmd/Ctrl+click on a printed URL opens it (forwarding a
                    // remote dev-server port first), mirroring the editor's
                    // go-to-definition modifier. Consumes the click so it
                    // doesn't also start a text selection.
                    if (m.modifiers.contains(KeyModifiers::SUPER)
                        || m.modifiers.contains(KeyModifiers::CONTROL))
                        && (self.terminal_url_click(m.column, m.row)
                            || self.terminal_file_click(m.column, m.row))
                    {
                        return;
                    }
                    // A child tracking the mouse (Claude Code's diff panel,
                    // htop, lazygit, vim with `mouse=a`) gets the click
                    // itself (#474): an SGR/X10 press now, motion under
                    // 1002/1003 as the drag goes, and the release on
                    // mouse-up, all aimed at THIS pane via
                    // `terminal_pointer_forwarded`. Shift is the bypass,
                    // the same one the wheel has: croft's own selection when
                    // held. `report_mouse` is the whole gate -- it declines
                    // when the child is not tracking or the cell is on the
                    // border, and either way the click falls through to the
                    // selection below. The Cmd/Ctrl link-open above still
                    // wins, as in VS Code and iTerm2: the modifier is
                    // croft's, and Cmd has no mouse-report encoding anyway.
                    // A stale croft highlight goes, as it does for a plain
                    // click anywhere else -- the user is aiming at the TUI,
                    // not at the text under it -- and the double-click
                    // tracker is not armed: the pair, if any, is the child's.
                    if !m.modifiers.contains(KeyModifiers::SHIFT)
                        && self.terminal_mut().report_mouse(
                            MouseButtonKind::Left,
                            MouseAction::Press,
                            m.column,
                            m.row,
                            mouse_mods(&m),
                        )
                    {
                        self.terminal_mut().clear_selection();
                        self.editor.clear_selection();
                        self.terminal_pointer_forwarded = Some(ForwardedPointer {
                            pane: self.terminals[idx].uid(),
                            press: (m.column, m.row),
                            selecting: false,
                        });
                        self.terminal_click.clear();
                        return;
                    }
                    // Shift+click extends the existing selection to the
                    // clicked cell (VS Code / iTerm2) instead of starting a
                    // fresh one.
                    if m.modifiers.contains(KeyModifiers::SHIFT)
                        && self.terminal().selection().is_some()
                    {
                        self.terminal_mut().extend_selection_to(m.column, m.row);
                        self.terminal_click.clear();
                        return;
                    }
                    // A selection belongs to exactly one pane: starting one in
                    // the terminal drops any leftover editor selection so the
                    // two panes never paint a highlight at once (issue #23).
                    self.editor.clear_selection();
                    let now = std::time::Instant::now();
                    let is_double =
                        self.terminal_click
                            .is_double(now, m.column, m.row, m.modifiers);
                    if is_double {
                        self.terminal_mut().select_word_at(m.column, m.row);
                        self.terminal_click.clear();
                    } else {
                        // Begin a fresh selection at the click cell. Without a
                        // drag this is a single cell (no area), so the selection
                        // ends up cleared on mouse-up. With a drag, this is the
                        // selection anchor.
                        self.terminal_mut().start_selection_at(m.column, m.row);
                        self.terminal_click
                            .record(now, m.column, m.row, m.modifiers);
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                // Tab drag (#1565): live reorder, as the terminal panes below
                // do. Crossing another tab of the same bar moves the dragged
                // tab there; it stays active and the drag follows it.
                if let Some(from) = self.editor_tab_drag_from {
                    if let Some(to) = self.editor.tab_at(m.column, m.row)
                        && to != from
                        && let Some(at) = self.editor.move_tab(from, to)
                    {
                        self.editor_tab_drag_from = Some(at);
                        self.poke_cursor();
                    }
                    return;
                }
                if let Some(from) = self.terminal_drag_from {
                    // Live reorder: the moment the pointer crosses another
                    // pane (side-by-side) or rail row (maximize mode), the
                    // dragged terminal moves there and the drag follows it.
                    // No ghost or indicator needed - the panes themselves
                    // rearrange under the pointer, and `move_terminal` keeps
                    // the dragged pane active.
                    let target = if self.terminal_pane_maximized && self.terminals.len() > 1 {
                        self.terminal_rail_rects
                            .iter()
                            .position(|r| rect_contains(*r, m.column, m.row))
                    } else {
                        self.terminal_at_pos(m.column, m.row)
                    };
                    if let Some(to) = target
                        && to != from
                    {
                        self.move_terminal(from, to);
                        self.terminal_drag_from = Some(to);
                    }
                    return;
                }
                if let Some(kind) = self.splitter_drag.clone() {
                    self.handle_splitter_drag(kind, m.column, m.row);
                    return;
                }
                if self.minimap_drag {
                    self.minimap_scroll_to_row(m.row);
                    return;
                }
                if self.editor_hscrollbar_drag {
                    self.editor.scroll_to_bar_x(m.column);
                    self.poke_cursor();
                    return;
                }
                // The lane press owns its gesture: no selection drag (#1544).
                if self.breakpoint_lane_press {
                    return;
                }
                if self.outline_scrollbar_drag {
                    self.outline.scroll_to_bar_y(m.row);
                    return;
                }
                if self.secondary_outline_scrollbar_drag {
                    self.with_secondary_outline(|o| o.scroll_to_bar_y(m.row));
                    return;
                }
                if self.open_editors_scrollbar_drag {
                    self.open_editors.scroll_to_bar_y(m.row);
                    return;
                }
                if self.timeline_scrollbar_drag {
                    self.timeline.scroll_to_bar_y(m.row);
                    return;
                }
                if self.deps_scrollbar_drag {
                    self.dependencies.scroll_to_bar_y(m.row);
                    return;
                }
                if self.testing_scrollbar_drag {
                    self.testing.scroll_to_bar_y(m.row);
                    return;
                }
                if self.graph_scrollbar_drag {
                    self.commit_graph.scroll_to_bar_y(m.row);
                    return;
                }
                if let Some(pane) = self.scrollbar_drag {
                    match pane {
                        Pane::Tree => match self.sidebar_view {
                            SidebarView::Explorer => {
                                self.tree.scroll_to_bar_y(m.row);
                            }
                            SidebarView::Remote => {
                                self.remote.scroll_to_bar_y(m.row);
                            }
                            SidebarView::SourceControl => {
                                self.source_control.scroll_to_bar_y(m.row);
                            }
                            SidebarView::Search => {
                                self.search.scroll_to_bar_y(m.row);
                            }
                            SidebarView::RunDebug => {}
                            SidebarView::Extensions => {}
                            SidebarView::Testing => {}
                            SidebarView::CodeQL => {}
                        },
                        Pane::Editor => {
                            self.editor.scroll_to_bar_y(m.row);
                            self.poke_cursor();
                        }
                        Pane::Terminal => {}
                    }
                    return;
                }
                // Some terminals emit a Drag at the same cell as the Down even
                // when the user hasn't actually dragged. Only forget the prior
                // click when the pointer has truly moved off that cell.
                self.editor_click.clear_if_moved(m.column, m.row);
                self.tree_click.clear_if_moved(m.column, m.row);
                self.terminal_click.clear_if_moved(m.column, m.row);
                // The modified tracker gets the same cancellation. It was left
                // out when it was added, so a bound `ctrl+double_click` fired
                // across a drag where the built-in double correctly did not —
                // the one tracker not on this list behaved differently from
                // every other one for no reason a user could discover.
                self.modified_click.clear_if_moved(m.column, m.row);
                // A drag after a press that went to a mouse-tracking child
                // (#474) belongs to whoever can use it. A child that asked
                // for motion (1002/1003) gets it, at the pane that got the
                // press, clamped to that pane's grid so a pointer that
                // wanders out reports the edge cell instead of vanishing. A
                // child that asked only for clicks (1000: Claude Code) has
                // no use for a drag, so the drag is croft's text selection,
                // anchored at the PRESS cell on its first motion so nothing
                // is lost by the press having been forwarded, then extended
                // here -- on the ORIGIN pane, found by uid, not through
                // `terminal_mut()`: Cmd+] can move the active pane while the
                // button is held, and the generic drag path below would then
                // extend the wrong pane's selection and leave this one at its
                // one-cell anchor. Edge auto-scroll is armed only while the
                // origin is still the active pane, because the tick that
                // services it scrolls the active one. The child still gets
                // the release either way. A gesture whose pane is gone ends
                // here with nothing to deliver.
                if let Some(fp) = self.terminal_pointer_forwarded {
                    let Some(idx) = self.forwarded_pane_index(fp) else {
                        self.terminal_pointer_forwarded = None;
                        self.terminal_select_autoscroll = None;
                        return;
                    };
                    if self.terminals[idx].mouse_motion_reporting() {
                        let (col, row) = self.terminals[idx].clamp_to_grid(m.column, m.row);
                        // Motion reporting was just checked, so the verdict
                        // is known; `let _` says "already decided", not
                        // "ignored". The encoder re-checks the same modes
                        // as its own guard; the two are intentional.
                        let _ = self.terminals[idx].report_mouse(
                            MouseButtonKind::Left,
                            MouseAction::Motion,
                            col,
                            row,
                            mouse_mods(&m),
                        );
                        return;
                    }
                    if !fp.selecting {
                        self.terminals[idx].start_selection_at(fp.press.0, fp.press.1);
                        self.terminal_pointer_forwarded = Some(ForwardedPointer {
                            selecting: true,
                            ..fp
                        });
                    }
                    let dir = self.terminals[idx].drag_select_to(m.column, m.row);
                    self.terminal_select_autoscroll = (dir != 0 && idx == self.active_terminal)
                        .then_some(TerminalSelectAutoScroll {
                            dir,
                            col: m.column,
                            last: std::time::Instant::now(),
                        });
                    return;
                }
                // Terminal drag-selection is handled before the per-pane
                // branches so it keeps extending even when the pointer
                // leaves the pane: dragging past the bottom edge is how the
                // user grows a selection down through scrollback. The
                // returned direction arms edge auto-scroll (serviced each
                // main-loop tick); a pointer back inside the pane disarms it.
                if self.focus == Pane::Terminal && self.terminal().selection().is_some() {
                    let dir = self.terminal_mut().drag_select_to(m.column, m.row);
                    self.terminal_select_autoscroll =
                        (dir != 0).then_some(TerminalSelectAutoScroll {
                            dir,
                            col: m.column,
                            last: std::time::Instant::now(),
                        });
                    return;
                }
                if in_editor {
                    if self.editor.diff.is_some() {
                        let last_inner = self.editor.last_inner;
                        self.editor.sync_tab_cells();
                        if let Some(diff) = self.editor.diff.as_mut()
                            && let Some((_, row_idx, char_col)) =
                                crate::widgets::editor::diff_hit_test(
                                    diff, last_inner, m.column, m.row,
                                )
                        {
                            diff.extend_selection_to(row_idx, char_col);
                        }
                        self.poke_cursor();
                        return;
                    }
                    if let Some(view) = self.editor.hex.as_mut() {
                        let rows = (view.layout.data_rows as usize).max(1);
                        if let Some(off) = view.hit_test(m.column, m.row) {
                            view.set_cursor(off, true, rows);
                        }
                        self.poke_cursor();
                        return;
                    }
                    if self.editor.box_selecting() {
                        self.editor.box_drag_to_screen(m.column, m.row);
                        self.poke_cursor();
                        return;
                    }
                    if let Some(anchor) = self.editor_line_drag {
                        self.editor.select_lines_at(Some(anchor), m.column, m.row);
                        self.poke_cursor();
                        return;
                    }
                    self.editor.mouse_drag(m.column, m.row);
                    self.poke_cursor();
                } else if in_tree {
                    match self.sidebar_view {
                        SidebarView::Explorer => {
                            // Promote to a real drag-and-drop the moment the
                            // pointer leaves the initiating cell.
                            if let Some(drag) = self.tree_drag.as_mut() {
                                let (_, sx, sy) = drag.started_at;
                                if m.column != sx || m.row != sy {
                                    drag.armed = true;
                                }
                                if drag.armed {
                                    drag.target_idx =
                                        drag_target_index(&self.tree, m.row, &drag.paths);
                                    self.tree.drag_target = drag.target_idx;
                                }
                            }
                            // While not dragging, treat continued movement as a
                            // range-extend from the initial anchor — just like
                            // VS Code's drag-to-select.
                            if self.tree_drag.as_ref().is_none_or(|d| !d.armed)
                                && self.tree.sticky_row_at(m.row).is_none()
                                && let Some(idx) = self.tree.node_at_y(m.row)
                            {
                                // Never extend the selection onto rows the
                                // sticky band covers — they are invisible
                                // under it (#126).
                                self.tree.select(idx);
                            }
                        }
                        SidebarView::Remote => {
                            if let Some(idx) = self.remote.target_at_y(m.row) {
                                self.remote.select(idx);
                            }
                        }
                        SidebarView::Search
                        | SidebarView::SourceControl
                        | SidebarView::RunDebug
                        | SidebarView::Extensions
                        | SidebarView::Testing
                        | SidebarView::CodeQL => {}
                    }
                } else if in_terminal {
                    self.terminal_mut().extend_selection_to(m.column, m.row);
                } else if self.tree_drag.is_some() {
                    // Pointer dragged outside the tree pane: still keep the drag
                    // alive so dropping back inside lands; just clear the drop
                    // highlight so the user knows nothing will happen here.
                    if let Some(drag) = self.tree_drag.as_mut() {
                        drag.target_idx = None;
                    }
                    self.tree.drag_target = None;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.editor_tab_drag_from = None;
                self.editor_line_drag = None;
                // The release of a press that was forwarded to a tracking
                // child goes to the same pane (#474), clamped to its grid
                // like the drag. `report_mouse` may decline if the child
                // dropped tracking mid-press, which is the child's own
                // choice; there is nothing croft should do with the release
                // instead. The pane is found by uid, so one closed under
                // the held button is simply gone rather than mistaken for
                // its neighbour. The whole gesture ends here, aimed at the
                // ORIGIN pane and never the active one: a click, or a drag
                // the child asked for, gets no copy-on-select, click-to-move,
                // annotation or secret popup; a drag the Drag arm turned into
                // croft's selection is finalised on that pane (`end_drag`,
                // then copy-on-select), the same tail the ordinary mouse-up
                // below runs for the active pane.
                if let Some(fp) = self.terminal_pointer_forwarded.take() {
                    if let Some(idx) = self.forwarded_pane_index(fp) {
                        let (col, row) = self.terminals[idx].clamp_to_grid(m.column, m.row);
                        self.terminals[idx].report_mouse(
                            MouseButtonKind::Left,
                            MouseAction::Release,
                            col,
                            row,
                            mouse_mods(&m),
                        );
                        if fp.selecting {
                            self.terminals[idx].end_drag();
                            if self.copy_on_select
                                && self.terminals[idx]
                                    .selection()
                                    .is_some_and(|s| s.has_area())
                            {
                                let text = self
                                    .terminal_text_for_copy(self.terminals[idx].selection_text());
                                if !text.is_empty() {
                                    self.copy_where_pasted(&text);
                                }
                            }
                        }
                    }
                    self.terminal_select_autoscroll = None;
                    return;
                }
                // Releasing the button ends any terminal edge auto-scroll
                // and finalizes a drag-selection (the alt-screen content
                // anchor is captured at release).
                self.terminal_select_autoscroll = None;
                if self.focus == Pane::Terminal {
                    self.terminal_mut().end_drag();
                }
                // Copy-on-select (VS Code's terminal.integrated.copyOnSelection,
                // iTerm2's default): a finished terminal drag-selection lands
                // on the clipboard without the explicit Cmd+C. Silent — this
                // fires on every selection, so no status churn.
                if self.copy_on_select
                    && self.focus == Pane::Terminal
                    && self.terminal().selection().is_some_and(|s| s.has_area())
                {
                    let text = self.terminal_text_for_copy(self.terminal().selection_text());
                    if !text.is_empty() {
                        self.copy_where_pasted(&text);
                    }
                }
                // A plain click on a redacted span shows the real value in
                // a popup (#360): the mask is paint-only, so this is the
                // one way to read it without a reveal window. Checked
                // before click-to-move, like the annotation popup: on the
                // prompt row a masked token yields its value rather than a
                // caret move, which is the useful answer for a secret.
                if self.focus == Pane::Terminal
                    && !self.terminal().selection().is_some_and(|s| s.has_area())
                    && let Some(secret) = self.terminal().redacted_at(m.column, m.row)
                {
                    self.terminal_drag_from = None;
                    if self.terminal().selection().is_some() {
                        self.terminal_mut().clear_selection();
                    }
                    self.hover_popup = Some(crate::widgets::hover_popup::HoverPopup::new(
                        secret,
                        (m.column, m.row),
                    ));
                    return;
                }
                // A plain click on an annotated span pops its note (checked
                // before click-to-move so a note near the prompt is still
                // readable).
                if self.focus == Pane::Terminal
                    && !self.terminal().selection().is_some_and(|s| s.has_area())
                    && let Some((_, note)) = self.terminal().annotation_at(m.column, m.row)
                {
                    // This early return skips the shared mouse-up tail, so
                    // run its cleanup here too: drop the pane-drag latch and
                    // the zero-area selection the mouse-down planted, or the
                    // clicked cell stays painted as a one-cell highlight.
                    self.terminal_drag_from = None;
                    if self.terminal().selection().is_some() {
                        self.terminal_mut().clear_selection();
                    }
                    self.hover_popup = Some(crate::widgets::hover_popup::HoverPopup::new(
                        note,
                        (m.column, m.row),
                    ));
                    return;
                }
                // Ghostty's click-to-move-cursor: a plain click (no drag
                // selection, no reordering, program not tracking the mouse)
                // on the shell's prompt row walks its cursor to the clicked
                // cell with synthesized arrow keys. Pane-local input: a
                // click is not typing, so broadcast never mirrors it.
                if self.focus == Pane::Terminal
                    && self.terminal_drag_from.is_none()
                    && self.terminal_copy_mode.is_none()
                    && !self.terminal().selection().is_some_and(|s| s.has_area())
                    && !self.terminal().mouse_reporting()
                    && let Some(bytes) = self.terminal().prompt_click_arrows(m.column, m.row)
                {
                    self.terminal_mut().write_input(&bytes);
                }
                // Releasing ends a pane drag-reorder; the order is already
                // final because the reorder is live during the drag.
                self.terminal_drag_from = None;
                // Finish a column (box) selection, keeping the carets it built.
                if self.editor.box_selecting() {
                    self.editor.end_box_select();
                    return;
                }
                if self.splitter_drag.take().is_some() {
                    return;
                }
                if std::mem::take(&mut self.minimap_drag) {
                    return;
                }
                if self.scrollbar_drag.take().is_some() {
                    return;
                }
                if std::mem::take(&mut self.editor_hscrollbar_drag) {
                    return;
                }
                if std::mem::take(&mut self.breakpoint_lane_press) {
                    return;
                }
                if std::mem::take(&mut self.outline_scrollbar_drag) {
                    return;
                }
                if std::mem::take(&mut self.secondary_outline_scrollbar_drag) {
                    return;
                }
                if std::mem::take(&mut self.open_editors_scrollbar_drag) {
                    return;
                }
                if std::mem::take(&mut self.timeline_scrollbar_drag) {
                    return;
                }
                if std::mem::take(&mut self.deps_scrollbar_drag) {
                    return;
                }
                if std::mem::take(&mut self.testing_scrollbar_drag) {
                    return;
                }
                if std::mem::take(&mut self.graph_scrollbar_drag) {
                    return;
                }
                if let Some(drag) = self.tree_drag.take() {
                    self.tree.drag_target = None;
                    if drag.armed {
                        // Drag while Alt/Ctrl is held copies; default is move.
                        let copy = drag.toggle_on_release
                            || m.modifiers.contains(KeyModifiers::ALT)
                            || m.modifiers.contains(KeyModifiers::CONTROL);
                        let mode = if copy {
                            ExplorerClipMode::Copy
                        } else {
                            ExplorerClipMode::Cut
                        };
                        if let Some(target_idx) = drag.target_idx {
                            let target_dir = drop_target_dir(&self.tree, target_idx);
                            self.move_or_copy_in_explorer(&target_dir, &drag.paths, mode);
                        } else {
                            self.status = String::from("Drop cancelled");
                        }
                        return;
                    }
                    if drag.toggle_on_release {
                        // Stationary Alt/Ctrl-click: now perform the toggle.
                        self.tree.toggle_mark(drag.start_idx);
                        return;
                    }
                }
                // Mouse-up never auto-copies. The selection stays highlighted
                // so the user can hit Cmd/Ctrl+C themselves; a click without
                // drag (zero-area selection) is silently dropped.
                if in_terminal {
                    if let Some(sel) = self.terminal().selection()
                        && !sel.has_area()
                    {
                        self.terminal_mut().clear_selection();
                    }
                } else if in_editor
                    && let Some(sel) = self.editor.selection
                    && !sel.has_area()
                {
                    self.editor.clear_selection();
                }
            }
            MouseEventKind::ScrollDown => {
                if in_secondary_outline {
                    self.with_secondary_outline(|o| o.scroll_down(3));
                } else if in_outline {
                    self.outline.scroll_down(3);
                } else if in_open_editors {
                    self.open_editors.scroll_down(3);
                } else if in_timeline {
                    self.timeline.scroll_down(3);
                } else if in_deps {
                    self.dependencies.scroll_down(3);
                } else if in_problems {
                    self.problems.scroll_down(3);
                } else if in_output {
                    self.output.scroll_down(3);
                } else if in_tree {
                    match self.sidebar_view {
                        SidebarView::Explorer => self.tree.scroll_down(3),
                        SidebarView::Remote => self.remote.scroll_down(3),
                        SidebarView::SourceControl => {
                            if rect_contains(self.commit_graph.last_area, m.column, m.row) {
                                self.commit_graph.scroll_down(3);
                            } else {
                                self.source_control.scroll_down(3);
                            }
                        }
                        SidebarView::Search => self.search.scroll_down(3),
                        SidebarView::RunDebug => {}
                        SidebarView::Extensions => self.extensions.scroll_down(3),
                        SidebarView::Testing => self.testing.scroll_down(3),
                        SidebarView::CodeQL => self.codeql.scroll_down(3),
                    }
                } else if in_editor {
                    if let Some(diff) = self.editor.diff.as_mut() {
                        diff.scroll_down_by(3);
                    } else if self.editor.pdf_page().is_some() {
                        self.wheel_pdf_page(1);
                    } else if let Some(view) = self.editor.hex.as_mut() {
                        let rows = (view.layout.data_rows as usize).max(1);
                        view.scroll_by(3, rows);
                    } else {
                        self.editor.scroll_down(3);
                    }
                } else if self.scroll_inactive_group_at(m.column, m.row, 3) {
                } else if rect_contains(self.terminal_rail_area, m.column, m.row) {
                    // The rail is its own scrollable list, like VS Code's
                    // terminal tabs. Paint clamps the offset.
                    self.terminal_rail_scroll = self.terminal_rail_scroll.saturating_add(1);
                } else if let Some(idx) = terminal_hit {
                    let shift = m.modifiers.contains(KeyModifiers::SHIFT);
                    let t = &mut self.terminals[idx];
                    // A child tracking the mouse (Claude Code, htop, vim) gets
                    // a real wheel report so it scrolls its own content. Shift
                    // bypasses that for croft's scrollback. A report that
                    // could not be delivered (the wheel sat on the pane
                    // BORDER, outside the inner grid) falls through like any
                    // non-tracking pane instead of swallowing the notch.
                    if t.mouse_reporting()
                        && !shift
                        && t.report_mouse(
                            MouseButtonKind::WheelDown,
                            MouseAction::Press,
                            m.column,
                            m.row,
                            mouse_mods(&m),
                        )
                    {
                    } else if !t.scroll_down(3) {
                        if t.app_cursor_keys() {
                            t.write_input(b"\x1bOB\x1bOB\x1bOB");
                        } else {
                            t.write_input(b"\x1b[B\x1b[B\x1b[B");
                        }
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                if in_secondary_outline {
                    self.with_secondary_outline(|o| o.scroll_up(3));
                } else if in_outline {
                    self.outline.scroll_up(3);
                } else if in_open_editors {
                    self.open_editors.scroll_up(3);
                } else if in_timeline {
                    self.timeline.scroll_up(3);
                } else if in_deps {
                    self.dependencies.scroll_up(3);
                } else if in_problems {
                    self.problems.scroll_up(3);
                } else if in_output {
                    self.output.scroll_up(3);
                } else if in_tree {
                    match self.sidebar_view {
                        SidebarView::Explorer => self.tree.scroll_up(3),
                        SidebarView::Remote => self.remote.scroll_up(3),
                        SidebarView::SourceControl => {
                            if rect_contains(self.commit_graph.last_area, m.column, m.row) {
                                self.commit_graph.scroll_up(3);
                            } else {
                                self.source_control.scroll_up(3);
                            }
                        }
                        SidebarView::Search => self.search.scroll_up(3),
                        SidebarView::RunDebug => {}
                        SidebarView::Extensions => self.extensions.scroll_up(3),
                        SidebarView::Testing => self.testing.scroll_up(3),
                        SidebarView::CodeQL => self.codeql.scroll_up(3),
                    }
                } else if in_editor {
                    if let Some(diff) = self.editor.diff.as_mut() {
                        diff.scroll_up_by(3);
                    } else if self.editor.pdf_page().is_some() {
                        self.wheel_pdf_page(-1);
                    } else if let Some(view) = self.editor.hex.as_mut() {
                        let rows = (view.layout.data_rows as usize).max(1);
                        view.scroll_by(-3, rows);
                    } else {
                        self.editor.scroll_up(3);
                    }
                } else if self.scroll_inactive_group_at(m.column, m.row, -3) {
                } else if rect_contains(self.terminal_rail_area, m.column, m.row) {
                    self.terminal_rail_scroll = self.terminal_rail_scroll.saturating_sub(1);
                } else if let Some(idx) = terminal_hit {
                    let shift = m.modifiers.contains(KeyModifiers::SHIFT);
                    let t = &mut self.terminals[idx];
                    // Same border fall-through as the WheelDown arm below.
                    if t.mouse_reporting()
                        && !shift
                        && t.report_mouse(
                            MouseButtonKind::WheelUp,
                            MouseAction::Press,
                            m.column,
                            m.row,
                            mouse_mods(&m),
                        )
                    {
                    } else if !t.scroll_up(3) {
                        if t.app_cursor_keys() {
                            t.write_input(b"\x1bOA\x1bOA\x1bOA");
                        } else {
                            t.write_input(b"\x1b[A\x1b[A\x1b[A");
                        }
                    }
                }
            }
            MouseEventKind::ScrollLeft if in_editor => {
                if let Some(diff) = self.editor.diff.as_mut() {
                    diff.scroll_left_by(4);
                } else {
                    self.editor.scroll_left_by(4);
                }
            }
            MouseEventKind::ScrollRight if in_editor => {
                if let Some(diff) = self.editor.diff.as_mut() {
                    diff.scroll_right_by(4);
                } else {
                    self.editor.scroll_right_by(4);
                }
            }
            _ => {}
        }
    }
}
