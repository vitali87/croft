//! Editor keys, vim mode, macros, multi-cursor and line commands, typing toggles,
//! copy, cut and paste routing and the clipboard, and reading dropped file paths out
//! of a paste.

use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum VimOp {
    #[default]
    None,
    G,
    Y,
}

impl EditorVimChord {
    pub(super) fn reset(&mut self) {
        self.op = VimOp::None;
        self.count = 0;
    }
}

impl App {
    /// VS Code "Change All Occurrences" (Cmd/Ctrl+F2): select every textual
    /// match of the word under the cursor in the current file as multi-cursor
    /// carets so the next keystrokes edit them all at once.
    pub(super) fn start_change_all_occurrences(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let n = self.editor.select_all_occurrences_of_word_at_cursor();
        if n == 0 {
            self.status = String::from("No symbol under cursor");
        } else {
            self.status = format!("Selected {n} occurrences (type to replace, Esc to cancel)");
        }
    }

    /// VS Code "Select All Occurrences of Find Match" (`Cmd+Shift+L`,
    /// #1558): a selection on every match of the selection, or of the word
    /// at the caret.
    pub(super) fn select_all_occurrences(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let n = self.editor.select_all_occurrences();
        self.status = if n == 0 {
            String::from("Nothing to select: select text on one line, or put the caret on a word")
        } else {
            format!("Selected {n} occurrences (type to replace, Esc to cancel)")
        };
    }

    /// VS Code "Add Cursors to Line Ends" (`Shift+Alt+I`, #1558): a caret at
    /// the end of each selected line.
    pub(super) fn add_cursors_to_line_ends(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let n = self.editor.add_cursors_to_line_ends();
        self.status = if n == 0 {
            String::from("Select the lines to put cursors at the ends of")
        } else {
            format!("{n} cursors at line ends")
        };
    }

    /// VS Code "Expand Line Selection" (`Cmd+L` / `Ctrl+L`, #1570): select
    /// the caret's whole line, and one more line on each further press.
    pub(super) fn expand_line_selection(&mut self) {
        if self.editor_is_text() {
            self.editor.expand_line_selection();
        }
    }

    /// VS Code "Add Selection to Next Find Match" (`Cmd+D`): select the word
    /// under the cursor, then grow the multi-cursor one occurrence per press.
    pub(super) fn start_select_next_occurrence(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let n = self.editor.select_next_occurrence();
        if n == 0 {
            self.status = String::from("No symbol under cursor");
        } else if n > 1 {
            self.status = format!("Selected {n} occurrences (type to replace, Esc to cancel)");
        }
    }

    /// VS Code "Delete Line" (`Cmd+Shift+K`): delete the cursor's line, or
    /// every line a selection touches, yanking the removed text to the
    /// clipboard. Replaces the old `Cmd+d d` convenience, whose leader now
    /// belongs to Add Selection to Next Match.
    pub(super) fn delete_current_line(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let yanked = self.editor.delete_caret_lines();
        if !yanked.is_empty() {
            copy_to_clipboard(&yanked);
        }
        let n = yanked.lines().count();
        self.status = format!("Deleted {n} {}", if n == 1 { "line" } else { "lines" });
    }

    /// Kill from the caret to the end of its line, yanking the text to the
    /// clipboard (emacs `kill-line`). `Ctrl+K` in the editor on macOS and in
    /// vim mode; elsewhere `Ctrl+K` leads the `Cmd+K` chords (#843), and the
    /// palette's "Kill to End of Line" is the way here.
    pub(super) fn kill_to_end_of_line(&mut self) {
        if self.editor.has_non_text_view() {
            return;
        }
        let killed = self.editor.kill_to_eol();
        if !killed.is_empty() {
            copy_to_clipboard(&killed);
            self.status = format!("Killed {} chars", killed.chars().count());
        }
    }

    /// Whether the single-line prompt (New File, Rename, ...) is the one
    /// taking keys: open, with nothing `handle_key_inner` answers ahead of it
    /// on top. A paste goes into the prompt only then, so it never edits the
    /// name held under a picker, dialog or confirm drawn over it.
    pub(super) fn prompt_takes_input(&self) -> bool {
        self.prompt.is_some()
            && !self.modal_overlay_open()
            && self.connect_dialog.is_none()
            && self.settings_editor.is_none()
            && self.pending_lossy_save.is_none()
            && self.pending_unsaved.is_none()
            && self.pending_replace_all.is_none()
            && self.pending_unsaved_scm.is_none()
            && self.pending_stash_drop.is_none()
            && !self.pending_discard_all
            && self.pending_run_block.is_none()
            && self.pending_terminal_paste.is_none()
            && !self.pending_broadcast_enable
    }

    /// Flip native modal (vim) editing on or off. Driven by the global Cmd+E
    /// shortcut, so it can fire from any pane. Turning it off also dismisses
    /// any lingering `/` `?` search-match highlights, since the modal layer is
    /// the only place that clears them.
    pub(super) fn toggle_vim_mode(&mut self) {
        // The Vim modal layer is a bundled, toggleable extension: when disabled
        // in the Extensions panel, Cmd+E is inert (and any active session is
        // force-ended so a disable can't strand the editor in modal mode).
        if !self.is_extension_enabled("vim") {
            if self.vim.enabled {
                self.vim.toggle();
                self.end_vim_undo_groups();
            }
            self.status = String::from("Vim extension is disabled");
            return;
        }
        self.vim.toggle();
        // A change left open in Insert mode ends with the mode (#934).
        self.end_vim_undo_groups();
        if self.vim.enabled {
            self.status = String::from("vim mode on");
        } else {
            self.editor
                .set_search_highlight(None, crate::widgets::search::SearchOpts::default());
            self.status = String::from("vim mode off");
        }
        // Saved as the mode the next launch starts in (#1286). Under test
        // only into a config dir the test chose, never the developer's.
        self.vim_mode_pref = self.vim.enabled;
        if !cfg!(test) || self.config_dir != crate::paths::config_dir() {
            self.report_unsaved_setting(crate::prefs::save_vim_mode_in(
                &self.config_dir,
                self.vim.enabled,
            ));
        }
    }

    /// Follow the `vim_mode` pref (#1286) at launch and on a settings
    /// reload: Vim mode on when it is set and the `vim` extension is
    /// enabled. Only a change of the pref moves the live mode, so a reload
    /// for another setting leaves the mode, and Insert or Normal, as it is.
    pub(super) fn follow_vim_mode_pref(&mut self, want: bool) {
        if want == self.vim_mode_pref {
            return;
        }
        self.vim_mode_pref = want;
        let on = want && self.is_extension_enabled("vim");
        if on != self.vim.enabled {
            self.vim.toggle();
            self.end_vim_undo_groups();
            if !on {
                self.editor
                    .set_search_highlight(None, crate::widgets::search::SearchOpts::default());
            }
        }
    }

    /// Increment (`delta` 1) or decrement (`delta` -1) the number under the
    /// cursor, reporting in the status bar when there is nothing to bump so
    /// the keystroke never looks broken.
    pub(super) fn bump_number_under_cursor(&mut self, delta: i64) {
        if !self.editor_is_text() {
            return;
        }
        if !self.editor.bump_number(delta) {
            self.status = String::from("No number at or after the cursor on this line");
        }
    }

    pub(super) fn handle_editor_key(&mut self, key: KeyEvent) {
        // Inline find bar (Cmd+F / Ctrl+F) eats every key while open.
        if self.editor_find.is_some() {
            self.handle_editor_find_key(key);
            return;
        }
        // F7 / Shift+F7 in a buffer with merge conflicts: jump between the
        // blocks, the same chord the diff view uses for its hunks (VS Code's
        // "Go to Next/Previous Conflict"). Only claimed while conflicts
        // exist, so F7 keeps its normal meaning everywhere else.
        if matches!(key.code, KeyCode::F(7)) && !self.editor.conflicts().is_empty() {
            self.jump_conflict(key.modifiers.contains(KeyModifiers::SHIFT));
            return;
        }
        // F7 / Shift+F7 elsewhere: Go to Next / Previous Change over the
        // gutter's change bars (#1352), as the diff view walks its hunks.
        if matches!(key.code, KeyCode::F(7)) && self.editor.merge.is_none() {
            self.jump_editor_change(!key.modifiers.contains(KeyModifiers::SHIFT));
            return;
        }
        // Merge editor (#253): F7 hops the tracked Result regions (the
        // Result has no markers, so the arm above never claims it) and
        // Alt+Up/Down scrolls the source panes together. Every other key
        // falls through to the ordinary text path — the Result is a real
        // buffer.
        if self.editor.merge.is_some() {
            if matches!(key.code, KeyCode::F(7)) {
                self.merge_jump(key.modifiers.contains(KeyModifiers::SHIFT));
                return;
            }
            if key.modifiers.contains(KeyModifiers::ALT)
                && matches!(key.code, KeyCode::Up | KeyCode::Down)
            {
                let delta = if key.code == KeyCode::Up { -3 } else { 3 };
                if let Some(mv) = self.editor.merge.as_mut() {
                    mv.scroll_panes(delta);
                }
                return;
            }
        }
        if is_editor_find_key(key) {
            self.open_editor_find();
            return;
        }
        if is_editor_replace_key(key) {
            self.open_editor_replace();
            return;
        }
        // `.http`/`.rest` (#370): Cmd/Ctrl+Enter sends the request under
        // the caret, REST Client's own chord. Checked BEFORE the runnable
        // fence (#353), which claims the same chord: this arm is scoped to
        // request files, so the two compose — a `.http` file sends, and
        // every other file falls through to the fence path with its
        // meaning intact.
        if matches!(key.code, KeyCode::Enter)
            && key
                .modifiers
                .intersects(KeyModifiers::SUPER | KeyModifiers::CONTROL)
            && self
                .editor
                .path
                .as_deref()
                .is_some_and(crate::http_file::is_http_file)
        {
            self.send_http_request_under_caret();
            return;
        }
        // Runnable docs (#353): Cmd+Enter runs the fence under the caret.
        // Outside a runnable fence it falls through to Insert Line Below
        // (#1571).
        if is_run_fence_key(key) && self.focus == Pane::Editor && self.run_fence_at_cursor(false) {
            return;
        }
        // Markdown: Toggle Preview (Cmd/Ctrl+Shift+V, the VS Code default).
        if is_markdown_preview_key(key) {
            self.toggle_markdown_preview();
            return;
        }
        // While the rendered preview is up the buffer is not visible, so
        // navigation scrolls the preview and every other key is swallowed
        // (no invisible edits). Cmd+Shift+V above flips back to the source.
        if self.editor.markdown_preview.is_some() {
            // Copy Path / Copy Relative Path first: their chords are
            // Cmd+Opt+C and Cmd+Opt+Shift+C, which the looser copy-key
            // predicate below also matches. They are meaningful while a
            // preview is up (the file has a path either way), and this
            // block returns unconditionally, so they must be dispatched
            // here or they never run.
            if is_copy_path_key(key) {
                if let Some(path) = self.editor.path.clone() {
                    self.copy_path_to_clipboard(path);
                }
                return;
            }
            if is_copy_relative_path_key(key) {
                if let Some(path) = self.editor.path.clone() {
                    self.copy_relative_path_to_clipboard(path);
                }
                return;
            }
            // Close Tab (Cmd/Ctrl+W) for the same reason: the close-tab check
            // further down, above the diff / sheet / image guards, is never
            // reached from here, so a rendered Markdown, document or media
            // preview could not be closed from the keyboard (#2069).
            if is_close_tab_key(key) {
                self.request_close_tab(self.editor.active_index());
                return;
            }
            // Copy the rendered selection (#215): the preview swallows every
            // other key, so without this branch Cmd+C here never copies.
            if is_editor_copy_key(key) {
                self.copy_editor_selection();
                return;
            }
            if matches!(key.code, KeyCode::Esc)
                && let Some(md) = self.editor.markdown_preview.as_mut()
                && md.has_selection()
            {
                md.selection = None;
                md.dragging = false;
                return;
            }
            let page = self.editor.text_rows() as i32;
            match key.code {
                KeyCode::Up => {
                    self.editor.scroll_markdown_preview(-1);
                }
                KeyCode::Down => {
                    self.editor.scroll_markdown_preview(1);
                }
                KeyCode::PageUp => {
                    self.editor.scroll_markdown_preview(-page);
                }
                KeyCode::PageDown | KeyCode::Char(' ') => {
                    self.editor.scroll_markdown_preview(page);
                }
                KeyCode::Home => {
                    self.editor.scroll_markdown_preview(i16::MIN as i32);
                }
                // The render pass clamps to the wrapped height, so jumping
                // far past the end lands exactly on the last page.
                KeyCode::End => {
                    self.editor.scroll_markdown_preview(i16::MAX as i32);
                }
                _ => {}
            }
            return;
        }
        // Completion popup intercepts navigation / accept / dismiss keys
        // before normal editor handling. Any other key dismisses the popup
        // and falls through so the character lands in the buffer.
        if self.handle_completion_popup_key(key) {
            return;
        }
        // A snippet tab-stop session survives only plain typing, Backspace, and
        // Tab (which advances it). Any other key ends the session so Tab returns
        // to its normal indent role.
        if self.editor.snippet_active() && !snippet_key_continues(key) {
            self.editor.cancel_snippet();
        }
        // Esc closes the parameter-hints popup first (VS Code parity); a second
        // Esc then falls through to clear the selection / collapse cursors.
        if self.signature_help_popup.is_some() && matches!(key.code, KeyCode::Esc) {
            self.dismiss_signature_help();
            return;
        }
        // F4: focus the active file's next navigator comment box;
        // Shift+F4 ignores the nearest one.
        if matches!(key.code, KeyCode::F(4)) {
            if key.modifiers == KeyModifiers::SHIFT {
                self.ignore_focused_comment();
                return;
            }
            if key.modifiers.is_empty() {
                self.cycle_comment_box();
                return;
            }
        }
        if is_completion_trigger_key(key) {
            self.trigger_completion();
            return;
        }
        // Rename Symbol (F2) and Change All Occurrences (Cmd/Ctrl+F2) are only
        // meaningful on a text buffer, so they sit above the read-only diff /
        // sheet / image guards below. The sheet grid is editable and F2 is its
        // own "edit this cell" key (#1618), so there F2 falls through to it.
        if is_rename_symbol_key(key) && self.editor.sheet.is_none() {
            self.start_rename_symbol();
            return;
        }
        if is_change_all_occurrences_key(key) {
            self.start_change_all_occurrences();
            return;
        }
        if is_select_next_occurrence_key(key) {
            self.start_select_next_occurrence();
            return;
        }
        if is_select_all_occurrences_key(key) {
            self.select_all_occurrences();
            return;
        }
        if is_add_cursors_to_line_ends_key(key) {
            self.add_cursors_to_line_ends();
            return;
        }
        if is_expand_line_selection_key(key) {
            self.expand_line_selection();
            return;
        }
        if is_delete_line_key(key) {
            self.delete_current_line();
            return;
        }
        // VS Code "Reveal in Finder" (Cmd+Opt+R) for the active file. Works on
        // any tab backed by a real path (text, diff, sheet, image), so it sits
        // above the text-only guards. Local-macOS only, matching the Explorer
        // chord and the tab-menu entry; withheld on remote/headless sessions.
        if !self.is_remote && cfg!(target_os = "macos") && is_reveal_in_finder_key(key) {
            if let Some(path) = self.editor.path.clone() {
                self.reveal_in_finder(path);
            }
            return;
        }
        // VS Code "Copy Path" (Cmd+Opt+C): copy the active file's absolute path
        // to the clipboard. Works on any tab with a real path, local or remote;
        // a blank/untitled buffer is a no-op (the keystroke is still swallowed).
        if is_copy_path_key(key) {
            if let Some(path) = self.editor.path.clone() {
                self.copy_path_to_clipboard(path);
            }
            return;
        }
        // VS Code "Copy Relative Path" (Cmd+Opt+Shift+C): copy the active file's
        // path relative to the workspace root. Same path-or-no-op rule as Copy
        // Path; the keystroke is swallowed even on a blank/untitled buffer.
        if is_copy_relative_path_key(key) {
            if let Some(path) = self.editor.path.clone() {
                self.copy_relative_path_to_clipboard(path);
            }
            return;
        }
        // VS Code "Toggle Line Comment" (Cmd+/ / Ctrl+/), "Toggle Block
        // Comment" (Shift+Alt+A), and "View: Toggle Word Wrap" (Alt+Z). All
        // act on a text buffer, so they no-op on read-only diff / sheet /
        // image tabs (the keystroke is still swallowed). Each is also
        // reachable from the Command Palette, which guarantees parity on
        // terminals that don't deliver the Alt+letter chords.
        if is_toggle_line_comment_key(key) {
            // The palette's message too: a key that does nothing because the
            // language has no comment must say so (#1181); a blank line in
            // one that has does nothing quietly.
            if self.editor_is_text()
                && !self.editor.toggle_line_comment()
                && !self.editor.has_line_comment_syntax()
            {
                self.status = String::from("Toggle Line Comment: no comment for this language");
            }
            return;
        }
        if is_toggle_block_comment_key(key) {
            if self.editor_is_text() {
                self.editor.toggle_block_comment();
            }
            return;
        }
        if is_toggle_wrap_key(key) {
            if self.editor_is_text() {
                self.editor.toggle_wrap();
            }
            return;
        }
        // Alt+O: C/C++: Switch Header/Source (#1360), as VS Code's clangd
        // extension binds it. Other files keep the chord.
        if is_switch_header_source_key(key)
            && self
                .editor
                .path
                .as_deref()
                .is_some_and(crate::header_source::is_c_family)
        {
            self.switch_header_source();
            return;
        }
        // VS Code "Go to Bracket" (Cmd+Shift+\): jump the cursor to the
        // matching bracket. Also reachable from the Command Palette.
        if is_goto_bracket_key(key) {
            if self.editor_is_text() {
                self.editor.jump_to_matching_bracket();
            }
            return;
        }
        // VS Code "Select to Bracket" (Cmd+Opt+\): select the region between
        // the matching brackets, including the brackets.
        if is_select_to_bracket_key(key) {
            if self.editor_is_text() {
                self.editor.select_to_matching_bracket();
            }
            return;
        }
        // VS Code "Go Back" (Ctrl+-): the keyboard twin of the
        // Ctrl+Shift+click chord, so navigation history is reachable
        // without depending on the host terminal's mouse delivery (#31).
        if is_navigate_forward_key(key) {
            self.nav_forward();
            return;
        }
        if is_navigate_back_key(key) {
            self.nav_back();
            return;
        }
        // VS Code "Transpose Characters around the Cursor" (Ctrl+T).
        if is_transpose_key(key) {
            if self.editor_is_text() {
                self.editor.transpose_chars();
            }
            return;
        }
        // VS Code "Convert Indentation to Spaces" (Cmd+Opt+Shift+S).
        if is_indentation_to_spaces_key(key) {
            if self.editor_is_text() {
                self.editor.indentation_to_spaces();
                self.status = String::from("Converted indentation to spaces");
            }
            return;
        }
        // VS Code "Convert Indentation to Tabs" (Cmd+Opt+Shift+T).
        if is_indentation_to_tabs_key(key) {
            if self.editor_is_text() {
                self.editor.indentation_to_tabs();
                self.status = String::from("Converted indentation to tabs");
            }
            return;
        }
        // VS Code's `files.trimFinalNewlines`, as a command (Cmd+Opt+Shift+N).
        if is_trim_final_newlines_key(key) {
            if self.editor_is_text() && self.editor.trim_final_newlines() {
                self.status = String::from("Trimmed final newlines");
            }
            return;
        }
        // Bookmarks: toggle on the cursor's line, then walk them with
        // Cmd+Opt+. / Cmd+Opt+, (VS Code's Bookmarks extension, nvim marks).
        if is_toggle_bookmark_key(key) {
            self.toggle_bookmark();
            return;
        }
        if is_next_bookmark_key(key) {
            self.goto_bookmark(true);
            return;
        }
        if is_prev_bookmark_key(key) {
            self.goto_bookmark(false);
            return;
        }
        if is_clear_bookmarks_key(key) {
            self.clear_bookmarks();
            return;
        }
        // Formerly palette-only editor commands, now each on a chord so none
        // ships without an accelerator (croft tenet: everything has a shortcut).
        if is_join_lines_key(key) {
            if self.editor_is_text() {
                self.editor.join_lines();
            }
            return;
        }
        if is_transform_upper_key(key) {
            if self.editor_is_text() {
                self.editor
                    .transform_selection_case(crate::widgets::editor::CaseTransform::Upper);
            }
            return;
        }
        if is_transform_lower_key(key) {
            if self.editor_is_text() {
                self.editor
                    .transform_selection_case(crate::widgets::editor::CaseTransform::Lower);
            }
            return;
        }
        if is_sort_lines_asc_key(key) {
            if self.editor_is_text() {
                self.editor.sort_lines(true);
            }
            return;
        }
        if is_sort_lines_desc_key(key) {
            if self.editor_is_text() {
                self.editor.sort_lines(false);
            }
            return;
        }
        if is_trim_trailing_whitespace_key(key) {
            if self.editor_is_text() && self.editor.trim_trailing_whitespace() {
                self.status = String::from("Trimmed trailing whitespace");
            }
            return;
        }
        if is_increment_number_key(key) {
            self.bump_number_under_cursor(1);
            return;
        }
        if is_decrement_number_key(key) {
            self.bump_number_under_cursor(-1);
            return;
        }
        if is_emmet_expand_key(key) {
            self.expand_emmet_abbreviation();
            return;
        }
        // VS Code "Format Document" (Cmd+Opt+Shift+F): reformat the whole buffer
        // through the language server. No-op on read-only diff / sheet / image.
        if is_format_document_key(key) {
            if self.editor_is_text() {
                self.start_format_document();
            }
            return;
        }
        // VS Code "Quick Fix" (Cmd+. / Ctrl+.): code actions at the cursor.
        if is_quick_fix_key(key) {
            if self.editor_is_text() {
                self.start_code_action();
            }
            return;
        }
        // Rebase todo (#620): single-key actions on commit lines.
        if self.rebase_todo_key(key) {
            return;
        }
        // Inline suggestion (#607): Tab accepts, Esc dismisses, Ctrl+Right
        // and Ctrl+Down take a word or a line of it (#1625).
        if self.inline_suggestion_key(key) {
            return;
        }
        // Search Editor (#615): Enter or F12 on a result row opens the match;
        // anywhere else both keep their usual meaning.
        if (key.code == KeyCode::Enter && key.modifiers.is_empty() || is_go_to_definition_key(key))
            && self.open_search_editor_result()
        {
            return;
        }
        // Go to Definition (F12) / References (Shift+F12) / Type Definition
        // (Ctrl+F12) / Implementations (Cmd+F12) / Declaration (Ctrl+Shift+F12),
        // the VS Code F12-family bindings, also need a real text buffer. All are
        // no-ops on read-only diff / sheet / image tabs; the keystroke is still
        // swallowed so it never leaks into the buffer.
        if is_peek_definition_key(key) {
            if self.editor.diff.is_none()
                && self.editor.sheet.is_none()
                && self.editor.image.is_none()
            {
                self.peek_definition_at_cursor();
            }
            return;
        }
        if is_go_to_definition_key(key) {
            if self.editor.diff.is_none()
                && self.editor.sheet.is_none()
                && self.editor.image.is_none()
            {
                self.request_definition_at_cursor();
            }
            return;
        }
        if is_go_to_declaration_key(key) {
            if self.editor.diff.is_none()
                && self.editor.sheet.is_none()
                && self.editor.image.is_none()
            {
                self.request_declaration_at_cursor();
            }
            return;
        }
        if is_go_to_type_definition_key(key) {
            if self.editor.diff.is_none()
                && self.editor.sheet.is_none()
                && self.editor.image.is_none()
            {
                self.request_type_definition_at_cursor();
            }
            return;
        }
        if is_go_to_implementation_key(key) {
            if self.editor.diff.is_none()
                && self.editor.sheet.is_none()
                && self.editor.image.is_none()
            {
                self.request_implementation_at_cursor();
            }
            return;
        }
        if is_peek_references_key(key) {
            if self.editor.diff.is_none()
                && self.editor.sheet.is_none()
                && self.editor.image.is_none()
            {
                self.peek_references_at_cursor();
            }
            return;
        }
        if is_go_to_references_key(key) {
            if self.editor.diff.is_none()
                && self.editor.sheet.is_none()
                && self.editor.image.is_none()
            {
                self.request_references_at_cursor();
            }
            return;
        }
        // Column (box) selection from the keyboard (#1560): VS Code's
        // Cmd+Shift+Opt (Ctrl+Shift+Alt) arrows and PageUp/PageDown, and
        // plain Shift+arrows while Column Selection Mode is on. Ahead of the
        // multi-cursor collapse below, since the box is carets it keeps
        // growing, and of Add Cursor, which used to take the Up/Down chords
        // and leave one caret.
        if let Some((rows, cols)) = self.column_select_key(key) {
            self.editor.column_select(rows, cols);
            return;
        }
        // While multi-cursor mode is live, printable typing / backspace /
        // delete fan out to every caret and Esc collapses back to one cursor.
        // Any other key collapses the carets and falls through to normal
        // single-cursor handling (e.g. an arrow moves just the primary).
        if self.editor.has_multi_cursor() {
            match key.code {
                KeyCode::Esc => {
                    self.editor.collapse_carets();
                    self.status = String::from("Cursors collapsed");
                    return;
                }
                KeyCode::Backspace => {
                    self.editor.multi_backspace();
                    return;
                }
                KeyCode::Delete => {
                    self.editor.multi_delete_forward();
                    return;
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT)
                        && !key.modifiers.contains(KeyModifiers::SUPER) =>
                {
                    self.editor.multi_insert_char(c);
                    return;
                }
                _ => self.editor.collapse_carets(),
            }
        }
        // Close-tab (Cmd/Ctrl+W) applies to every tab type, so it sits above
        // the read-only diff / sheet / image guards below. Those guards each
        // short-circuit to a view-specific handler that has no close branch,
        // so without this hoist Cmd+W is silently swallowed on a diff, sheet,
        // or image tab and the tab can never be closed from the keyboard.
        if is_close_tab_key(key) {
            self.request_close_tab(self.editor.active_index());
            return;
        }
        if self.editor.diff.is_some() {
            self.handle_diff_key(key);
            return;
        }
        if self.editor.sheet.is_some() {
            self.handle_sheet_key(key);
            return;
        }
        if self.editor.hex.is_some() {
            self.handle_hex_key(key);
            return;
        }
        if self.editor.archive.is_some() {
            self.handle_archive_key(key);
            return;
        }
        if self.editor.pr_review.is_some() {
            self.handle_pr_review_key(key);
            return;
        }
        if self.editor.fleet.is_some() {
            self.handle_fleet_key(key);
            return;
        }
        if self.editor.sarif.is_some() {
            self.handle_sarif_key(key);
            self.refresh_sarif_preview();
            return;
        }
        if self.editor.log.is_some() && self.handle_log_key(key) {
            return;
        }
        // Image preview tabs are read-only. PDF tabs page with every
        // navigation key (arrows, PageUp/PageDown, Space, Home/End) and with
        // the wheel; everything else is swallowed.
        if self.editor.image.is_some() {
            match key.code {
                KeyCode::Right | KeyCode::PageDown | KeyCode::Down | KeyCode::Char(' ') => {
                    self.step_pdf_page(1);
                }
                KeyCode::Left | KeyCode::PageUp | KeyCode::Up => {
                    self.step_pdf_page(-1);
                }
                KeyCode::Home => self.jump_pdf_page(1),
                KeyCode::End => self.jump_pdf_page(u32::MAX),
                _ => {}
            }
            return;
        }
        // When modal editing is on it fully owns the editor pane and the
        // always-on `editor_vim_chord` convenience layer is bypassed. A
        // PassThrough means the key is not claimed by vim (Insert-mode typing,
        // or any Cmd/Ctrl shortcut in Normal mode) and falls through to the
        // editor's normal handling below.
        if self.vim.enabled {
            match self.vim.on_key(key) {
                crate::vim::VimKeyResult::Consumed(actions) => {
                    for action in actions {
                        self.apply_vim_action(action);
                    }
                    return;
                }
                crate::vim::VimKeyResult::PassThrough => {}
            }
        } else if self.handle_editor_vim_chord(key) {
            return;
        }
        if is_editor_line_home_key(key) {
            self.editor.clear_selection();
            self.editor.home_line();
            return;
        }
        if is_editor_line_end_key(key) {
            self.editor.clear_selection();
            self.editor.end_line();
            return;
        }
        if is_editor_kill_to_eol_key(key) {
            self.kill_to_end_of_line();
            return;
        }
        if is_editor_kill_to_bol_key(key) {
            let killed = self.editor.kill_to_bol();
            if !killed.is_empty() {
                copy_to_clipboard(&killed);
                self.status = format!("Killed {} chars", killed.chars().count());
            }
            return;
        }
        // Insert Line Below (#1571): VS Code's `Cmd+Enter` (`Ctrl+Enter` off
        // macOS), with `Cmd+O` kept as an alias. A Markdown fence under the
        // caret ran above, so here it only opens a line.
        if is_editor_open_line_below_key(key) || is_run_fence_key(key) {
            self.editor.open_line_below();
            self.status = String::from("Opened line below");
            return;
        }
        if is_editor_open_line_above_key(key) {
            self.editor.open_line_above();
            self.status = String::from("Opened line above");
            return;
        }
        // Clipboard gestures take precedence over text input. They never
        // conflict with normal typing (Ctrl/Cmd + C/X/A) so the order is
        // safe even when the user is in the middle of editing.
        if is_editor_copy_key(key) {
            self.copy_editor_selection();
            return;
        }
        if is_editor_cut_key(key) {
            self.cut_editor_selection();
            return;
        }
        if is_editor_paste_key(key) {
            let text = self.clipboard_text();
            self.paste_clipboard_into_editor(text.as_deref());
            return;
        }
        if is_editor_select_all_key(key) {
            self.editor.select_all();
            self.status = format!(
                "Selected {} chars",
                self.editor.selection_text().chars().count()
            );
            return;
        }
        if is_editor_undo_key(key) {
            if self.editor.undo() {
                self.status = String::from("Undo");
            } else {
                self.status = String::from("Nothing to undo");
            }
            return;
        }
        if is_editor_redo_key(key) {
            if self.editor.redo() {
                self.status = String::from("Redo");
            } else {
                self.status = String::from("Nothing to redo");
            }
            return;
        }
        if let Some(idx) = jump_to_tab_index(key) {
            if self.editor.select(idx) {
                self.sync_open_file_poll_mtime();
                self.status = format!("Tab {}", idx + 1);
            }
            return;
        }
        if matches!(key.code, KeyCode::Esc) && self.editor.selection.is_some() {
            self.editor.clear_selection();
            return;
        }

        // Shift+<motion> extends selection in the motion's direction.
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let is_motion = matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
        );
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // The command modifier: Cmd (SUPER) on macOS, Ctrl on Linux / Termux.
        let cmd = key.modifiers.contains(KeyModifiers::SUPER)
            || key.modifiers.contains(KeyModifiers::CONTROL);
        // The word modifier for ←/→, Backspace and Delete: Option (Alt) on
        // macOS, Ctrl on Linux as in VS Code there (#909). Ctrl with Cmd
        // held is not it: Cmd with these keys is no word chord on macOS.
        let word = alt
            || key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::SUPER);
        // VS Code "Add Cursor Above/Below" (Cmd+Opt+Up / Cmd+Opt+Down). Must
        // come before the plain-Alt move-line check so the cmd+alt combo is
        // routed here, not there.
        if cmd && alt && !shift && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            match key.code {
                KeyCode::Up => self.editor.add_cursor_above(),
                KeyCode::Down => self.editor.add_cursor_below(),
                _ => unreachable!(),
            }
            return;
        }
        if shift && alt && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            match key.code {
                KeyCode::Up => self.editor.duplicate_lines_up(),
                KeyCode::Down => self.editor.duplicate_lines_down(),
                _ => unreachable!(),
            }
            return;
        }
        // Expand / Shrink Selection (#254): Shift+Alt+Right/Left, VS Code's
        // smart-select chords. This deliberately claims what used to be
        // word-wise selection extension — the same collision VS Code
        // resolves the same way.
        if shift && alt && !cmd && matches!(key.code, KeyCode::Left | KeyCode::Right) {
            if key.code == KeyCode::Right {
                self.expand_selection();
            } else {
                self.shrink_selection();
            }
            return;
        }
        // VS Code "Move Line Up/Down" (Alt+Up / Alt+Down, no Shift, no Cmd).
        if alt && !shift && !cmd && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            match key.code {
                KeyCode::Up => self.editor.move_lines_up(),
                KeyCode::Down => self.editor.move_lines_down(),
                _ => unreachable!(),
            }
            return;
        }
        if is_motion && shift {
            if self.editor.selection.is_none() {
                self.editor.start_selection_at_cursor();
            }
            match key.code {
                KeyCode::Up => self.editor.arrow_up(),
                KeyCode::Down => self.editor.arrow_down(),
                KeyCode::Left if word => self.editor.move_word_left(),
                KeyCode::Right if word => self.editor.move_word_right(),
                KeyCode::Left => self.editor.move_left(),
                KeyCode::Right => self.editor.move_right(),
                KeyCode::Home if cmd => self.editor.cursor_top(),
                KeyCode::End if cmd => self.editor.cursor_bottom(),
                KeyCode::Home => self.editor.home_key(),
                KeyCode::End => self.editor.end_key(),
                KeyCode::PageUp => self.editor.page_up_one_screen(),
                KeyCode::PageDown => self.editor.page_down_one_screen(),
                _ => {}
            }
            self.editor.extend_selection_to_cursor();
            self.editor.ensure_cursor_col_visible();
            return;
        }
        if is_motion {
            // Plain motion clears any prior selection (VS Code / Sublime
            // convention: arrows without Shift collapse the selection).
            self.editor.clear_selection();
        }

        match key.code {
            KeyCode::Up => self.editor.arrow_up(),
            KeyCode::Down => self.editor.arrow_down(),
            KeyCode::Left if word => self.editor.move_word_left(),
            KeyCode::Right if word => self.editor.move_word_right(),
            KeyCode::Left => self.editor.move_left(),
            KeyCode::Right => self.editor.move_right(),
            KeyCode::PageUp => self.editor.page_up_one_screen(),
            KeyCode::PageDown => self.editor.page_down_one_screen(),
            KeyCode::Home if cmd => self.editor.cursor_top(),
            KeyCode::End if cmd => self.editor.cursor_bottom(),
            KeyCode::Home => self.editor.home_key(),
            KeyCode::End => self.editor.end_key(),
            KeyCode::Backspace => {
                if word {
                    self.editor.delete_word_left();
                } else {
                    self.editor.backspace();
                }
                self.note_incomplete_completion_edit();
            }
            KeyCode::Delete if word => self.editor.delete_word_right(),
            KeyCode::Delete => self.editor.delete_forward(),
            // Insert flips between insert and overwrite (Replace) typing.
            KeyCode::Insert if !shift && !cmd && !alt && !word => self.toggle_overwrite_mode(),
            KeyCode::Enter => self.editor.insert_newline(),
            // Tab advances a live snippet, else expands a snippet whose prefix
            // was just typed, else indents (whole block for a multi-line
            // selection, otherwise to the next tab stop).
            KeyCode::Tab => self.handle_editor_tab(),
            // Shift+Tab arrives as BackTab. Outdent the current line, or every
            // line a selection touches, mirroring VS Code's Shift+Tab.
            KeyCode::BackTab => self.editor.dedent_lines(),
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER) =>
            {
                self.type_char_in_editor(c);
                // With an incomplete list pending but none of it on screen,
                // typing on still re-asks (#1529).
                self.note_incomplete_completion_edit();
                // The server's trigger characters, and with quick
                // suggestions a word begun, open the list (#929).
                self.note_completion_typed(c);
                // Parameter hints: `(` / `,` open or advance a call. Closing
                // (typing `)`, moving away, deleting) is handled by the tick's
                // re-query, which also covers nested calls.
                if is_signature_help_trigger(c) {
                    self.trigger_signature_help();
                }
            }
            _ => {}
        }
        // Home, End, typing, Backspace, Delete and Enter move the caret
        // sideways too; only ←/→ pulled the view along, so End on a wide
        // line or typing past the edge left the caret off-screen (#1215).
        self.editor.ensure_cursor_col_visible();
    }

    /// Vim-style chord layer for the editor: `cmd+gg` jumps to top, `cmd+G`
    /// jumps to bottom, `cmd+yy` yanks a line. Counts go between the chord
    /// start and the completion key (`cmd+g 12 g` jumps to line 12), and
    /// Enter completes a `cmd+g` holding one, so VS Code's Go to Line
    /// (`ctrl+g 12 Enter`) lands there too (#902). There is no `cmd+dd`:
    /// `cmd+d` is Add Selection to Next Match, and Delete Line moved to
    /// `cmd+shift+k`.
    /// Returns true when the key was consumed; false means the caller
    /// should continue with normal editor handling.
    pub(super) fn handle_editor_vim_chord(&mut self, key: KeyEvent) -> bool {
        if is_vim_goto_bottom(key) {
            let count = self.editor_vim_chord.count;
            self.editor_vim_chord.reset();
            if count > 0 {
                self.editor.goto_line(count);
                self.status = format!("Line {count}");
            } else {
                self.editor.goto_bottom();
                self.status = String::from("Bottom of file");
            }
            return true;
        }

        let count_pending = self.editor_vim_chord.count > 0;
        let op_pending = self.editor_vim_chord.op != VimOp::None;

        if (op_pending || count_pending)
            && let Some(d) = chord_digit_value(key)
        {
            self.editor_vim_chord.count = self
                .editor_vim_chord
                .count
                .saturating_mul(10)
                .saturating_add(d);
            let op = match self.editor_vim_chord.op {
                VimOp::G => 'g',
                VimOp::Y => 'y',
                VimOp::None => ' ',
            };
            self.status = chord_status_text(op, self.editor_vim_chord.count);
            return true;
        }

        if !op_pending {
            if is_vim_chord_g(key) {
                self.editor_vim_chord.op = VimOp::G;
                self.status = chord_status_text('g', self.editor_vim_chord.count);
                return true;
            }
            if is_vim_chord_y(key) {
                self.editor_vim_chord.op = VimOp::Y;
                self.status = chord_status_text('y', self.editor_vim_chord.count);
                return true;
            }
            if !count_pending {
                if let Some(d) = cmd_only_digit_value(key) {
                    self.editor_vim_chord.count = d;
                    self.status = chord_status_text(' ', self.editor_vim_chord.count);
                    return true;
                }
                return false;
            }
            return self.cancel_editor_vim_chord(key);
        }

        let completes = match self.editor_vim_chord.op {
            VimOp::G => {
                is_plain_letter(key, 'g')
                    || is_vim_chord_g(key)
                    || (count_pending && key.code == KeyCode::Enter && key.modifiers.is_empty())
            }
            VimOp::Y => is_plain_letter(key, 'y') || is_vim_chord_y(key),
            VimOp::None => false,
        };

        if completes {
            let count = self.editor_vim_chord.count;
            match self.editor_vim_chord.op {
                VimOp::G => {
                    if count > 0 {
                        self.editor.goto_line(count);
                        self.status = format!("Line {count}");
                    } else {
                        self.editor.goto_top();
                        self.status = String::from("Top of file");
                    }
                }
                VimOp::Y => {
                    let n = count.max(1);
                    let yanked = self.editor.yank_lines(n);
                    if !yanked.is_empty() {
                        copy_to_clipboard(&yanked);
                    }
                    self.status = format!("Yanked {n} {}", if n == 1 { "line" } else { "lines" });
                }
                VimOp::None => {}
            }
            self.editor_vim_chord.reset();
            return true;
        }

        self.cancel_editor_vim_chord(key)
    }

    /// Drop the pending chord on a key that completes none, returning whether
    /// that key is consumed. It keeps its normal meaning only when nothing but
    /// the leader was typed: after a count the digits were swallowed, so the
    /// last key alone would edit the buffer with a fragment of what was typed
    /// (`cmd+g 25 Tab` indenting the line). Esc only ever cancels (#902).
    pub(super) fn cancel_editor_vim_chord(&mut self, key: KeyEvent) -> bool {
        let consumed = self.editor_vim_chord.count > 0 || key.code == KeyCode::Esc;
        self.editor_vim_chord.reset();
        self.status = String::from("Chord cancelled");
        consumed
    }

    /// End the vim change's undo group (#934): on the active editor, which
    /// also starts the next edit's own step, and on whichever other tab or
    /// editor group the change was entered on.
    pub(super) fn end_vim_undo_groups(&mut self) {
        self.editor.end_undo_group();
        for group in
            std::iter::once(&mut self.editor).chain(self.editor_layout.inactive_groups_mut())
        {
            group.end_open_undo_groups();
        }
    }

    /// Apply one resolved vim intent to the editor. The vim state machine has
    /// already advanced its own mode; this only mutates the buffer/cursor.
    pub(super) fn apply_vim_action(&mut self, action: crate::vim::VimAction) {
        use crate::vim::{Op, Target, VimAction};
        // A change runs from the command that enters Insert to the next vim
        // command, the `Esc` that leaves it: the operator's delete, the line
        // `o` opens and the typed text undo as one step, as in Vim (#934).
        self.end_vim_undo_groups();
        if matches!(
            action,
            VimAction::Insert(_)
                | VimAction::Operate { op: Op::Change, .. }
                | VimAction::VisualOperate(Op::Change)
        ) {
            self.editor.begin_undo_group();
        }
        match action {
            VimAction::Move(motion, count) => {
                self.vim_apply_motion(motion, count);
                self.vim_sync_visual_selection();
            }
            VimAction::Operate { op, target, count } => match target {
                Target::Lines => self.vim_operate_lines(op, self.editor.cursor_row, count),
                Target::Object(obj) => self.vim_operate_textobject(op, obj),
                Target::Motion(m) => self.vim_operate_motion(op, m, count),
            },
            VimAction::DeleteChar(count) => self.vim_delete_chars(count),
            VimAction::Insert(at) => self.vim_enter_insert(at),
            VimAction::Paste(at, count) => self.vim_paste(at, count),
            VimAction::Undo => {
                self.status = if self.editor.undo() {
                    String::from("Undo")
                } else {
                    String::from("Nothing to undo")
                };
                // The step an operator opened holds the selection it acted
                // on; Normal mode shows none, and `x` must not delete it.
                self.editor.clear_selection();
            }
            VimAction::Redo => {
                self.status = if self.editor.redo() {
                    String::from("Redo")
                } else {
                    String::from("Nothing to redo")
                };
                self.editor.clear_selection();
            }
            VimAction::EnterVisual { line } => {
                self.editor.break_undo_coalescing();
                self.vim_visual_line = line;
                self.vim
                    .set_visual_anchor(self.editor.cursor_row, self.editor.cursor_col);
                self.editor.start_selection_at_cursor();
                self.status = String::from(if line {
                    "-- VISUAL LINE --"
                } else {
                    "-- VISUAL --"
                });
            }
            VimAction::ExitVisual => {
                self.editor.clear_selection();
                self.vim_visual_line = false;
                self.status.clear();
            }
            VimAction::VisualOperate(op) => self.vim_visual_operate(op),
            VimAction::ExitInsert => {
                if self.editor.cursor_col > 0 {
                    self.editor.cursor_col -= 1;
                }
                self.status.clear();
            }
            VimAction::RunEx(cmd) => self.vim_run_ex(&cmd),
            VimAction::Search(dir, term) => self.vim_search(dir, &term),
            VimAction::ClearPending => {
                self.editor.clear_selection();
                self.vim_visual_line = false;
            }
            // `q{a-z}` starts, a bare `q` stops. The vim machine reports the
            // gesture; the recording itself lives on App so a macro recorded
            // in vim mode replays from the palette and vice versa (#255).
            // Recording control is inert during replay. `macro_replaying`
            // gates capture, not dispatch, so without this a replayed `q`
            // would reach here and leave the user silently recording after
            // the replay returned — the class of command the issue's
            // exclusion list exists for.
            VimAction::MacroRecord(_) | VimAction::MacroReplay(..) if self.macro_replaying => {}
            VimAction::MacroRecord(register) => match register {
                // A register letter STARTS; a bare `q` stops. The app owns
                // the recording state, so vim never has to track it.
                Some(r) if self.macro_recording.is_none() => self.start_macro_recording(Some(r)),
                _ => self.stop_macro_recording(),
            },
            VimAction::MacroReplay(register, count) => match register {
                Some(r) => self.replay_macro_register(r, count),
                None => self.replay_last_macro(count),
            },
            VimAction::Unsupported(what) => {
                self.status = format!("{what} is not supported in vim mode");
            }
        }
    }

    pub(super) fn vim_line_len(&self, row: usize) -> usize {
        self.editor
            .lines
            .get(row)
            .map(|l| l.chars().count())
            .unwrap_or(0)
    }

    pub(super) fn vim_first_non_blank(&self, row: usize) -> usize {
        self.editor
            .lines
            .get(row)
            .map(|l| l.chars().take_while(|c| c.is_whitespace()).count())
            .unwrap_or(0)
    }

    pub(super) fn vim_apply_motion(&mut self, motion: crate::vim::Motion, count: usize) {
        use crate::vim::Motion;
        let n = count.max(1);
        match motion {
            Motion::Left => {
                self.editor.cursor_col = self.editor.cursor_col.saturating_sub(n);
            }
            Motion::Right => {
                let len = self.vim_line_len(self.editor.cursor_row);
                self.editor.cursor_col = (self.editor.cursor_col + n).min(len);
            }
            Motion::Up => {
                for _ in 0..n {
                    self.editor.move_up();
                }
            }
            Motion::Down => {
                for _ in 0..n {
                    self.editor.move_down();
                }
            }
            Motion::WordForward => {
                for _ in 0..n {
                    self.vim_word_forward();
                }
            }
            Motion::WordBackward => {
                for _ in 0..n {
                    self.vim_word_backward();
                }
            }
            Motion::WordEnd => {
                for _ in 0..n {
                    self.vim_word_end();
                }
            }
            Motion::LineStart => self.editor.cursor_col = 0,
            Motion::FirstNonBlank => {
                self.editor.cursor_col = self.vim_first_non_blank(self.editor.cursor_row);
            }
            Motion::LineEnd => {
                self.editor.cursor_col = self.vim_line_len(self.editor.cursor_row);
            }
            Motion::FileStart => self.editor.goto_top(),
            Motion::FileEnd => self.editor.goto_bottom(),
            Motion::GotoLine(line) => self.editor.goto_line(line),
            Motion::Find(kind, ch) => {
                self.vim_last_find = Some((kind, ch));
                self.vim_find_char(kind, ch, n);
            }
            Motion::RepeatFind => {
                if let Some((kind, ch)) = self.vim_last_find {
                    self.vim_find_char(kind, ch, n);
                }
            }
            Motion::RepeatFindRev => {
                if let Some((kind, ch)) = self.vim_last_find {
                    self.vim_find_char(reverse_find(kind), ch, n);
                }
            }
        }
        self.editor.ensure_cursor_col_visible();
    }

    pub(super) fn vim_find_char(&mut self, kind: crate::vim::FindKind, ch: char, count: usize) {
        use crate::vim::FindKind;
        let row = self.editor.cursor_row;
        let Some(line) = self.editor.lines.get(row) else {
            return;
        };
        let chars: Vec<char> = line.chars().collect();
        let mut col = self.editor.cursor_col;
        for _ in 0..count.max(1) {
            match kind {
                FindKind::CharForward => match (col + 1..chars.len()).find(|&i| chars[i] == ch) {
                    Some(idx) => col = idx,
                    None => return,
                },
                FindKind::TillForward => match (col + 1..chars.len()).find(|&i| chars[i] == ch) {
                    Some(idx) if idx > 0 => col = idx - 1,
                    _ => return,
                },
                FindKind::CharBackward => match (0..col).rev().find(|&i| chars[i] == ch) {
                    Some(idx) => col = idx,
                    None => return,
                },
                FindKind::TillBackward => match (0..col).rev().find(|&i| chars[i] == ch) {
                    Some(idx) => col = idx + 1,
                    None => return,
                },
            }
        }
        self.editor.cursor_col = col;
    }

    /// Character class for vim word motions: 0 = blank (or past line end),
    /// 1 = keyword (alphanumeric/underscore), 2 = punctuation. A `w`/`b`/`e`
    /// word is a maximal run of one non-blank class.
    pub(super) fn vim_class_at(&self, row: usize, col: usize) -> u8 {
        match self.editor.lines.get(row).and_then(|l| l.chars().nth(col)) {
            None => 0,
            Some(c) if c.is_whitespace() => 0,
            Some(c) if c.is_alphanumeric() || c == '_' => 1,
            Some(_) => 2,
        }
    }

    pub(super) fn vim_next_pos(&self, row: usize, col: usize) -> Option<(usize, usize)> {
        if col < self.vim_line_len(row) {
            Some((row, col + 1))
        } else if row + 1 < self.editor.lines.len() {
            Some((row + 1, 0))
        } else {
            None
        }
    }

    pub(super) fn vim_prev_pos(&self, row: usize, col: usize) -> Option<(usize, usize)> {
        if col > 0 {
            Some((row, col - 1))
        } else if row > 0 {
            Some((row - 1, self.vim_line_len(row - 1)))
        } else {
            None
        }
    }

    pub(super) fn vim_word_forward(&mut self) {
        let mut row = self.editor.cursor_row;
        let mut col = self.editor.cursor_col;
        let start = self.vim_class_at(row, col);
        if start != 0 {
            while self.vim_class_at(row, col) == start {
                match self.vim_next_pos(row, col) {
                    Some((r, c)) => {
                        row = r;
                        col = c;
                    }
                    None => {
                        self.editor.cursor_row = row;
                        self.editor.cursor_col = col;
                        return;
                    }
                }
            }
        }
        while self.vim_class_at(row, col) == 0 {
            match self.vim_next_pos(row, col) {
                Some((r, c)) => {
                    row = r;
                    col = c;
                }
                None => break,
            }
        }
        self.editor.cursor_row = row;
        self.editor.cursor_col = col;
    }

    pub(super) fn vim_word_backward(&mut self) {
        let mut row = self.editor.cursor_row;
        let mut col = self.editor.cursor_col;
        let Some((r, c)) = self.vim_prev_pos(row, col) else {
            return;
        };
        row = r;
        col = c;
        while self.vim_class_at(row, col) == 0 {
            match self.vim_prev_pos(row, col) {
                Some((r, c)) => {
                    row = r;
                    col = c;
                }
                None => {
                    self.editor.cursor_row = row;
                    self.editor.cursor_col = col;
                    return;
                }
            }
        }
        let class = self.vim_class_at(row, col);
        while let Some((pr, pc)) = self.vim_prev_pos(row, col) {
            if self.vim_class_at(pr, pc) != class {
                break;
            }
            row = pr;
            col = pc;
        }
        self.editor.cursor_row = row;
        self.editor.cursor_col = col;
    }

    pub(super) fn vim_word_end(&mut self) {
        let mut row = self.editor.cursor_row;
        let mut col = self.editor.cursor_col;
        let Some((r, c)) = self.vim_next_pos(row, col) else {
            return;
        };
        row = r;
        col = c;
        while self.vim_class_at(row, col) == 0 {
            match self.vim_next_pos(row, col) {
                Some((r, c)) => {
                    row = r;
                    col = c;
                }
                None => {
                    self.editor.cursor_row = row;
                    self.editor.cursor_col = col;
                    return;
                }
            }
        }
        let class = self.vim_class_at(row, col);
        while let Some((nr, nc)) = self.vim_next_pos(row, col) {
            if self.vim_class_at(nr, nc) != class {
                break;
            }
            row = nr;
            col = nc;
        }
        self.editor.cursor_row = row;
        self.editor.cursor_col = col;
    }

    /// Where `{count}cw` from a non-blank ends: `e` `count` times, except
    /// that on a word's last char the first `e` stays put, so `cw` there
    /// changes that char rather than running to the next word's end.
    pub(super) fn vim_change_word_end(&mut self, count: usize) {
        let (row, col) = (self.editor.cursor_row, self.editor.cursor_col);
        let at_word_end = self.vim_class_at(row, col + 1) != self.vim_class_at(row, col);
        for _ in usize::from(at_word_end)..count.max(1) {
            self.vim_word_end();
        }
    }

    pub(super) fn vim_operate_lines(&mut self, op: crate::vim::Op, start_row: usize, count: usize) {
        use crate::vim::Op;
        // A symbol tab's operator stays inside its symbol (#369): `dgg`
        // from its second line reaches its first line, not the file's.
        let (lo, hi) = self
            .editor
            .symbol_clip()
            .unwrap_or((0, self.editor.lines.len().max(1)));
        let first = start_row.clamp(lo, hi - 1);
        let end = (start_row + count.max(1)).clamp(first + 1, hi);
        self.editor.cursor_row = first;
        let n = end - first;
        match op {
            Op::Delete => {
                let text = self.editor.delete_lines(n);
                copy_to_clipboard(&text);
                self.status = format!("Deleted {n} line(s)");
            }
            Op::Yank => {
                let text = self.editor.yank_lines(n);
                copy_to_clipboard(&text);
                self.status = format!("Yanked {n} line(s)");
            }
            Op::Change => {
                let text = self.editor.delete_lines(n);
                copy_to_clipboard(&text);
                self.editor.open_line_above();
                self.vim.enter_insert_mode();
                self.status = String::from("-- INSERT --");
            }
        }
    }

    pub(super) fn vim_operate_motion(
        &mut self,
        op: crate::vim::Op,
        motion: crate::vim::Motion,
        count: usize,
    ) {
        if vim_motion_is_linewise(motion) {
            let (first, last) = self.vim_line_range_for_motion(motion, count);
            self.vim_operate_lines(op, first, last + 1 - first);
            return;
        }
        let start = (self.editor.cursor_row, self.editor.cursor_col);
        // `cw` on a word changes up to its end and keeps the blank after it
        // (Vim's `:help cw`, #934); on a blank it is `dw`'s span.
        let change_word = op == crate::vim::Op::Change
            && motion == crate::vim::Motion::WordForward
            && self.vim_class_at(start.0, start.1) != 0;
        if change_word {
            self.vim_change_word_end(count);
        } else {
            self.vim_apply_motion(motion, count);
        }
        let end = (self.editor.cursor_row, self.editor.cursor_col);
        let (lo, hi) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        // Forward-inclusive motions (f/t/e) cover the char they land on.
        let head = if start <= end && (change_word || vim_motion_is_forward_inclusive(motion)) {
            let len = self.vim_line_len(hi.0);
            (hi.0, (hi.1 + 1).min(len))
        } else {
            hi
        };
        self.editor.selection = Some(crate::widgets::editor::EditorSelection { anchor: lo, head });
        self.vim_apply_operator_to_selection(op, lo);
    }

    pub(super) fn vim_operate_textobject(
        &mut self,
        op: crate::vim::Op,
        obj: crate::vim::TextObject,
    ) {
        use crate::vim::TextObject;
        let row = self.editor.cursor_row;
        let Some((start, mut end)) = self.editor.word_at(row, self.editor.cursor_col) else {
            return;
        };
        if obj == TextObject::AWord
            && let Some(line) = self.editor.lines.get(row)
        {
            let chars: Vec<char> = line.chars().collect();
            while end < chars.len() && chars[end].is_whitespace() {
                end += 1;
            }
        }
        self.editor.selection = Some(crate::widgets::editor::EditorSelection {
            anchor: (row, start),
            head: (row, end),
        });
        self.vim_apply_operator_to_selection(op, (row, start));
    }

    /// Delete / yank / change whatever the editor selection currently spans,
    /// leaving the cursor at `start` and entering Insert mode for a change.
    pub(super) fn vim_apply_operator_to_selection(
        &mut self,
        op: crate::vim::Op,
        start: (usize, usize),
    ) {
        use crate::vim::Op;
        // A motion can leave a symbol tab's lines (`db` at its first
        // column); the operator covers only the part inside them (#369).
        let start = self.clamp_to_symbol_clip(start);
        if let Some(sel) = self.editor.selection {
            self.editor.selection = Some(crate::widgets::editor::EditorSelection {
                anchor: self.clamp_to_symbol_clip(sel.anchor),
                head: self.clamp_to_symbol_clip(sel.head),
            });
        }
        let text = self.editor.selection_text();
        copy_to_clipboard(&text);
        match op {
            Op::Yank => {
                self.editor.clear_selection();
                self.editor.cursor_row = start.0;
                self.editor.cursor_col = start.1;
            }
            Op::Delete => {
                self.editor.delete_selection();
            }
            Op::Change => {
                self.editor.delete_selection();
                self.vim.enter_insert_mode();
                self.status = String::from("-- INSERT --");
            }
        }
    }

    /// `pos` pulled inside a symbol tab's lines: above them to the first
    /// line's start, below them to the last line's end. Unchanged on an
    /// ordinary tab.
    pub(super) fn clamp_to_symbol_clip(&self, pos: (usize, usize)) -> (usize, usize) {
        match self.editor.symbol_clip() {
            Some((first, _)) if pos.0 < first => (first, 0),
            Some((_, end)) if pos.0 >= end => (end - 1, self.vim_line_len(end - 1)),
            _ => pos,
        }
    }

    pub(super) fn vim_line_range_for_motion(
        &self,
        motion: crate::vim::Motion,
        count: usize,
    ) -> (usize, usize) {
        use crate::vim::Motion;
        let row = self.editor.cursor_row;
        let last = self.editor.lines.len().saturating_sub(1);
        let n = count.max(1);
        match motion {
            Motion::Down => (row, (row + n).min(last)),
            Motion::Up => (row.saturating_sub(n), row),
            Motion::FileStart => (0, row),
            Motion::FileEnd => (row, last),
            Motion::GotoLine(line) => {
                let target = line.saturating_sub(1).min(last);
                if target >= row {
                    (row, target)
                } else {
                    (target, row)
                }
            }
            _ => (row, row),
        }
    }

    pub(super) fn vim_delete_chars(&mut self, count: usize) {
        let row = self.editor.cursor_row;
        let Some(line) = self.editor.lines.get(row) else {
            return;
        };
        let chars: Vec<char> = line.chars().collect();
        let col = self.editor.cursor_col;
        let end = (col + count.max(1)).min(chars.len());
        if col < end {
            let deleted: String = chars[col..end].iter().collect();
            copy_to_clipboard(&deleted);
            for _ in col..end {
                self.editor.delete_forward();
            }
        }
    }

    pub(super) fn vim_enter_insert(&mut self, at: crate::vim::InsertAt) {
        use crate::vim::InsertAt;
        let row = self.editor.cursor_row;
        match at {
            InsertAt::Cursor => {}
            InsertAt::After => {
                let len = self.vim_line_len(row);
                if self.editor.cursor_col < len {
                    self.editor.cursor_col += 1;
                }
            }
            InsertAt::LineStart => self.editor.cursor_col = self.vim_first_non_blank(row),
            InsertAt::LineEnd => self.editor.cursor_col = self.vim_line_len(row),
            InsertAt::Below => self.editor.open_line_below(),
            InsertAt::Above => self.editor.open_line_above(),
        }
        self.status = String::from("-- INSERT --");
    }

    pub(super) fn vim_paste(&mut self, at: crate::vim::PasteAt, count: usize) {
        use crate::vim::PasteAt;
        let Some(text) = self.clipboard_text() else {
            self.status = String::from("Nothing to paste: the clipboard can't be read");
            return;
        };
        if text.is_empty() {
            self.status = String::from("Nothing to paste: the clipboard is empty");
            return;
        }
        let n = count.max(1);
        if let Some(body) = text.strip_suffix('\n') {
            // Linewise paste: the yank ended on a newline, so it lands on
            // fresh lines rather than inside the current one.
            let block = std::iter::repeat_n(body, n).collect::<Vec<_>>().join("\n");
            match at {
                PasteAt::After => {
                    self.editor.cursor_col = self.vim_line_len(self.editor.cursor_row);
                    self.editor.insert_str(&format!("\n{block}"));
                }
                PasteAt::Before => {
                    self.editor.cursor_col = 0;
                    self.editor.insert_str(&format!("{block}\n"));
                }
            }
        } else {
            let block = text.repeat(n);
            if at == PasteAt::After {
                let len = self.vim_line_len(self.editor.cursor_row);
                if self.editor.cursor_col < len {
                    self.editor.cursor_col += 1;
                }
            }
            self.editor.insert_str(&block);
        }
        self.status = String::from("Pasted");
    }

    pub(super) fn vim_visual_operate(&mut self, op: crate::vim::Op) {
        use crate::vim::Op;
        let Some(sel) = self.editor.selection else {
            return;
        };
        if self.vim_visual_line {
            let ((sr, _), (er, _)) = sel.normalised();
            self.vim_operate_lines(op, sr, er + 1 - sr);
            self.vim_visual_line = false;
            return;
        }
        // Charwise visual is inclusive of the cell under the cursor, so extend
        // the high end one char before operating.
        let ((sr, sc), (er, ec)) = sel.normalised();
        let len = self.vim_line_len(er);
        self.editor.selection = Some(crate::widgets::editor::EditorSelection {
            anchor: (sr, sc),
            head: (er, (ec + 1).min(len)),
        });
        self.vim_apply_operator_to_selection(op, (sr, sc));
        self.vim_visual_line = false;
        if !matches!(op, Op::Change) {
            self.status.clear();
        }
    }

    pub(super) fn vim_run_ex(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        if let Ok(line) = cmd.parse::<usize>() {
            self.editor.goto_line(line);
            return;
        }
        match cmd {
            "w" | "write" => self.save(),
            "q" | "quit" => self.request_close_tab(self.editor.active_index()),
            // vim's `!` is the answer to "discard the changes?" given up
            // front, so these two never ask (#862).
            "q!" => self.close_tab_now(self.editor.active_index()),
            "qa!" => {
                self.clear_hot_exit();
                self.quit = true;
            }
            "wq" | "x" | "wq!" => {
                self.save();
                // A save that did not land (a disk conflict, a format still
                // running) leaves the tab dirty, and the close asks.
                self.request_close_tab(self.editor.active_index());
            }
            "qa" | "quitall" => self.guard_unsaved(UnsavedExit::Quit),
            other => self.status = format!("Unknown command: {other}"),
        }
    }

    pub(super) fn vim_search(&mut self, dir: crate::vim::SearchDir, term: &str) {
        self.editor.set_search_highlight(
            Some(term.to_string()),
            crate::widgets::search::SearchOpts::default(),
        );
        match self.vim_find_match(dir, term) {
            Some((row, col)) => {
                self.editor.cursor_row = row;
                self.editor.cursor_col = col;
                self.editor.ensure_cursor_col_visible();
                // Record the hit the cursor jumped to so the renderer paints it
                // in the distinct active colour (orange) while the remaining
                // matches keep the regular yellow. Literal search, so the match
                // is exactly `term` long in characters.
                self.editor.active_search_match = Some((row, col, term.chars().count()));
                self.status = match dir {
                    crate::vim::SearchDir::Forward => format!("/{term}"),
                    crate::vim::SearchDir::Backward => format!("?{term}"),
                };
            }
            None => {
                self.editor.active_search_match = None;
                self.status = format!("Pattern not found: {term}");
            }
        }
    }

    pub(super) fn vim_find_match(
        &self,
        dir: crate::vim::SearchDir,
        term: &str,
    ) -> Option<(usize, usize)> {
        use crate::vim::SearchDir;
        let lines = &self.editor.lines;
        if term.is_empty() || lines.is_empty() {
            return None;
        }
        let (cr, cc) = (self.editor.cursor_row, self.editor.cursor_col);
        let total = lines.len();
        match dir {
            SearchDir::Forward => {
                for off in 0..=total {
                    let row = (cr + off) % total;
                    let from = if off == 0 { cc + 1 } else { 0 };
                    if let Some(col) = find_char_index(&lines[row], term, from) {
                        return Some((row, col));
                    }
                }
                None
            }
            SearchDir::Backward => {
                for off in 0..=total {
                    let row = (cr + total - off) % total;
                    let limit = if off == 0 { cc } else { usize::MAX };
                    if let Some(col) = rfind_char_index(&lines[row], term, limit) {
                        return Some((row, col));
                    }
                }
                None
            }
        }
    }

    pub(super) fn vim_sync_visual_selection(&mut self) {
        if !matches!(
            self.vim.mode(),
            crate::vim::VimMode::Visual | crate::vim::VimMode::VisualLine
        ) {
            return;
        }
        if let Some(anchor) = self.vim.visual_anchor() {
            self.editor.selection = Some(crate::widgets::editor::EditorSelection {
                anchor,
                head: (self.editor.cursor_row, self.editor.cursor_col),
            });
        }
    }
}

impl App {
    /// Copy the terminal pane's current selection to the user's clipboard.
    /// Selection stays visible so the user can verify what was copied.
    ///
    /// A copy that finds NOTHING now says so (#538). Inside a program that has
    /// ASKED for mouse tracking — Claude Code and most full-screen TUIs, but
    /// not a program that never requested it — a plain drag is the child's
    /// gesture and croft deliberately holds no selection of its own (#474), so
    /// the old silent return left the user pressing copy with nothing
    /// happening and no way to learn that Shift+drag selects there. The hint
    /// is keyed on `mouse_reporting`, so a pane whose child is not tracking is
    /// told to drag, which is what works there.
    pub(super) fn copy_terminal_selection(&mut self) {
        if !self.terminal().selection().is_some_and(|s| s.has_area()) {
            self.status = String::from(if self.terminal().mouse_reporting() {
                "Nothing selected: Shift+drag selects inside a program that tracks the mouse"
            } else {
                "Nothing selected: drag to select first"
            });
            return;
        }
        let text = self.terminal_text_for_copy(self.terminal().selection_text());
        if text.is_empty() {
            // A real selection, but over blank cells: saying "nothing
            // selected" here would describe the wrong thing to fix.
            self.status = String::from("Selection has no text to copy");
            return;
        }
        self.deliver_copied_text(&text);
    }

    /// Put copied text where the user can actually paste it.
    ///
    /// "The clipboard" is not always this machine's. On a relay session croft
    /// runs on a remote box while the user types at another machine, and it is
    /// THAT clipboard a paste comes from — so the text goes home over the drop
    /// relay, the same road `request_remote_clipboard` uses to fetch it for a
    /// paste, travelled in the other direction (#538). Writing the remote
    /// box's clipboard instead put the text somewhere nothing could reach, and
    /// an SSH session usually has no clipboard tool to write to at all.
    ///
    /// Everywhere else it is this machine's clipboard, with the OSC 52
    /// fallback `copy_to_clipboard` already keeps for a host terminal that
    /// honours it. That fallback is reported HONESTLY: whether the sequence is
    /// acted on is the host terminal's choice, not something croft can see, so
    /// claiming a copy landed would be a claim croft cannot make.
    pub(super) fn deliver_copied_text(&mut self, text: &str) {
        let n = text.chars().count();
        self.status = self.copy_where_pasted(text).status(&format!("{n} chars"));
    }

    /// The copy `deliver_copied_text` makes, without the status line, for a
    /// caller that words its own. Every terminal copy goes this way (#1667):
    /// Copy Output, copy mode, quick select and copy on select all wrote
    /// the remote box's clipboard on a relay session.
    pub(super) fn copy_where_pasted(&mut self, text: &str) -> CopyLanding {
        if self.is_relay_session && self.push_clipboard_via_relay(text) {
            crate::clipboard::remember_copy(text);
            CopyLanding::Local
        } else if copy_to_clipboard(text) {
            CopyLanding::Clipboard
        } else {
            CopyLanding::Osc52
        }
    }

    /// Push copied text to the LOCAL machine's clipboard over the drop relay.
    /// Returns whether the request was queued.
    ///
    /// The payload goes in a file in the relay INBOX rather than into the
    /// request line: a request is one TAB-SEPARATED LINE, while copied text
    /// carries newlines and tabs of its own and can be megabytes of
    /// scrollback. The line names only the request id — the pump derives
    /// `<inbox>/<id>.txt` from the inbox it already owns, so no path from
    /// this side is ever handed to a remote shell.
    ///
    /// An oversized copy is REFUSED here rather than truncated there. The pump
    /// caps what it will read into the local machine's memory and drops
    /// anything past it, and a selection that pastes back half-formed is worse
    /// than falling through to the ordinary clipboard route and saying so.
    pub(super) fn push_clipboard_via_relay(&mut self, text: &str) -> bool {
        if text.len() > crate::remote::MAX_COPY_PAYLOAD_BYTES {
            return false;
        }
        let (Some(log_path), Some(inbox)) = (self.relay_log_path(), self.relay_inbox_path()) else {
            return false;
        };
        self.ensure_relay_dir();
        let request_id = Self::relay_request_id("copy");
        if std::fs::write(inbox.join(format!("{request_id}.txt")), text).is_err() {
            return false;
        }
        append_to_relay_log(&log_path, &format!("copy\t{request_id}\n")).is_ok()
    }

    /// Paste the host clipboard into the terminal: the chord (Cmd+V /
    /// Ctrl+Shift+V) and the pane menu's "Paste" both land here, so
    /// bracketed paste, broadcast fan-out and the remote-clipboard fallback
    /// behave identically whichever one the user reached for.
    ///
    /// The messages name the ACTION rather than the chord: the menu route has
    /// no Cmd+V in it, and a status line quoting a key the user never pressed
    /// reads as a bug report about the wrong thing.
    pub(super) fn paste_clipboard_into_terminal(&mut self) {
        let text = crate::clipboard::read_string();
        if text.is_none() && self.drop_relay_active() {
            self.request_remote_clipboard();
            return;
        }
        // Without a relay, what croft last copied, as for any other paste
        // inside croft (#933).
        match text.or_else(crate::clipboard::last_copy) {
            Some(text) if !text.is_empty() => {
                self.paste_terminal_input(text.as_bytes());
            }
            Some(_) => {
                self.status = String::from("Paste: clipboard is empty");
            }
            None => {
                self.status =
                    String::from("Paste: clipboard read failed (no pbpaste / NSPasteboard)");
            }
        }
    }

    pub(super) fn copy_editor_selection(&mut self) {
        // The rendered preview hides the source buffer, so its selection
        // is the only one the user can see (#215).
        if let Some(md) = self.editor.markdown_preview.as_ref() {
            let text = md.selection_text();
            if !text.is_empty() {
                copy_to_clipboard(&text);
                self.status = format!("Copied {} chars to clipboard", text.chars().count());
            }
            return;
        }
        // A rendered log likewise hides its own text behind a stub buffer,
        // and the return is unconditional: falling through when there is no
        // selection would run the editor's copy over that stub and put its
        // placeholder content on the clipboard, which is worse than copying
        // nothing.
        if self.editor.log.is_some() {
            self.copy_log_selection();
            return;
        }
        if let Some(diff) = self.editor.diff.as_ref() {
            if let Some(text) = diff.selection_text()
                && !text.is_empty()
            {
                copy_to_clipboard(&text);
                self.status = format!("Copied {} chars to clipboard", text.chars().count());
            }
            return;
        }
        let Some(sel) = self.editor.selection else {
            return;
        };
        if !sel.has_area() {
            return;
        }
        let text = self.editor.selection_text();
        if text.is_empty() {
            return;
        }
        copy_to_clipboard(&text);
        self.status = format!("Copied {} chars to clipboard", text.chars().count());
    }

    /// Whether the active editor view shows a selection for Copy to take,
    /// read as [`Self::copy_editor_selection`] reads it: a rendered preview's
    /// or log's own selection, a diff's, else the buffer's in a text view.
    pub(super) fn editor_view_has_selection(&self) -> bool {
        if let Some(md) = self.editor.markdown_preview.as_ref() {
            return !md.selection_text().is_empty();
        }
        if let Some(log) = self.editor.log.as_ref() {
            return log.has_selection();
        }
        if let Some(diff) = self.editor.diff.as_ref() {
            return diff.selection_text().is_some_and(|t| !t.is_empty());
        }
        self.editor_is_text() && self.editor.selection.is_some_and(|s| s.has_area())
    }

    pub(super) fn cut_editor_selection(&mut self) {
        let Some(sel) = self.editor.selection else {
            return;
        };
        if !sel.has_area() {
            return;
        }
        let text = self.editor.selection_text();
        if text.is_empty() {
            self.editor.clear_selection();
            return;
        }
        copy_to_clipboard(&text);
        let n = text.chars().count();
        self.editor.delete_selection();
        self.status = format!("Cut {n} chars to clipboard");
    }

    pub(super) fn paste_clipboard_into_search(&mut self, text: Option<&str>) {
        let Some(s) = text else {
            self.status = String::from("Cmd+V: clipboard read failed");
            return;
        };
        if s.is_empty() {
            self.status = String::from("Cmd+V: clipboard is empty");
            return;
        }
        self.paste_str_into_search(s);
        self.status = format!(
            "Cmd+V: pasted into {} field",
            search_field_name(self.search.field)
        );
    }

    /// Paste `s` at the caret of the focused Search field, whichever it is,
    /// for the clipboard key and the bracketed-paste event alike.
    pub(super) fn paste_str_into_search(&mut self, s: &str) {
        self.search.insert_str(s);
        // Replace text doesn't change which files match, so only re-run the
        // scan when the paste landed in a search-affecting field.
        if self.search.field != crate::widgets::search::SearchField::Replace {
            self.submit_search_query();
        }
    }

    pub(super) fn paste_clipboard_into_editor(&mut self, text: Option<&str>) {
        let Some(s) = text else {
            self.status = String::from("Cmd+V: clipboard read failed");
            return;
        };
        if s.is_empty() {
            self.status = String::from("Cmd+V: clipboard is empty");
            return;
        }
        self.editor.paste_str(s, self.paste_reindent);
        self.status = format!("Pasted {} chars", s.chars().count());
    }

    /// What a paste inside croft inserts: the host clipboard, or what croft
    /// itself last copied when the host clipboard can't be read (#933), as
    /// on a Linux host with no clipboard tool, where copies go out only by
    /// OSC 52.
    pub(super) fn clipboard_text(&self) -> Option<String> {
        (self.clipboard_reader)().or_else(crate::clipboard::last_copy)
    }

    /// A terminal paste. While a macro records, a paste into the editor is a
    /// step of it, on the same terms `handle_key` records a key: not while
    /// replaying, and only when the editor had focus before and after and
    /// the text went into it (#1464). Before, the replay silently left the pasted text out.
    pub(super) fn handle_paste(&mut self, s: &str) {
        if self.screencast.is_on() {
            self.screencast.pasted(std::time::Instant::now());
        }
        let capture =
            self.macro_recording.is_some() && !self.macro_replaying && self.focus == Pane::Editor;
        let into_editor = self.handle_paste_inner(s);
        if self.macro_recording.is_none() || self.macro_replaying {
            return;
        }
        // A paste the palette, a prompt or the find bar took never reached
        // the document, so replaying it there would insert stray text.
        if capture && into_editor && self.focus == Pane::Editor {
            if let Some(rec) = self.macro_recording.as_mut() {
                rec.recorded.push_paste(s);
            }
        } else if !s.is_empty() {
            // Logged as skipped keys are, by size only: a paste into a
            // terminal can be a password.
            crate::output::push(
                "Macros",
                crate::output::OutputLevel::Info,
                &format!(
                    "skipped a paste of {} chars: recording captures the editor pane only",
                    s.chars().count()
                ),
            );
        }
    }

    /// Route a paste to whatever owns it; true only when it went into the
    /// editor's text, the one place a recording macro can replay it.
    pub(super) fn handle_paste_inner(&mut self, s: &str) -> bool {
        // Modal overlays own paste entirely while open — paste lands in
        // the modal's input field, never in the editor / tree / terminal
        // behind it. Without this, hitting Cmd+V right after Cmd+P pastes
        // into whatever pane was last focused instead of into the finder
        // query, which makes the "open + paste a filename" path that the
        // VS Code Quick Open is meant to support unusable.
        if let Some(prompt) = self.input_prompt.as_mut() {
            for c in s.chars() {
                if !c.is_control() {
                    prompt.push_char(c);
                }
            }
            return false;
        }
        if let Some(palette) = self.command_palette.as_mut() {
            for c in s.chars() {
                if !c.is_control() {
                    palette.push_char(c);
                }
            }
            return false;
        }
        if let Some(picker) = self.go_to_symbol.as_mut() {
            for c in s.chars() {
                if !c.is_control() {
                    picker.push_char(c);
                }
            }
            return false;
        }
        if let Some(picker) = self.workspace_symbols.as_mut() {
            for c in s.chars() {
                if !c.is_control() {
                    picker.push_char(c);
                }
            }
            self.workspace_symbols_requery();
            return false;
        }
        if let Some(finder) = self.file_finder.as_mut() {
            for c in s.chars() {
                if !c.is_control() {
                    finder.push_char(c);
                }
            }
            self.follow_quick_open_prefix();
            return false;
        }
        // New File, Rename and the other single-line prompts take a paste at
        // the caret, as the find bar does, but only while they take the keys.
        // Quick Open and the palette above take it when open over a prompt;
        // under any other picker, dialog or confirm the paste is dropped,
        // so it never edits the name the prompt holds out of sight.
        if self.prompt.is_some() {
            if self.prompt_takes_input()
                && let Some(prompt) = self.prompt.as_mut()
            {
                let text: String = s.chars().filter(|c| !c.is_control()).collect();
                prompt.edit(|t, at| crate::widgets::line_edit::insert_at(t, at, &text));
            }
            return false;
        }
        // The in-file find bar is a pane-scoped widget, not a modal: it only
        // owns the paste while the editor itself is focused. Without the focus
        // guard, an open find bar would swallow a paste meant for the terminal
        // (or tree) the user has since clicked into — the keystroke path is
        // already focus-gated via `match self.focus`, so paste must match it.
        if self.focus == Pane::Editor && self.editor_find.is_some() {
            // Pasted at the caret (#1729), like typing: the bar is an input
            // line, and a middle-of-query paste must not jump to the end.
            let text = s.to_string();
            self.editor_find_edit_at_caret(|t, c| {
                crate::widgets::editor_find::insert_at(t, c, &text)
            });
            return false;
        }
        // Finder drag-and-drop into croft arrives via the host terminal as
        // a bracketed paste containing absolute filesystem path(s). The
        // drop in iTerm2 does NOT shift mouse focus, so we cannot require
        // `self.focus == Pane::Tree` for the gesture: the user can have
        // last clicked into the editor or the embedded terminal and still
        // expect the drop on the sidebar to import.
        //
        //   * Remote view: any path-shaped paste is an scp upload. There
        //     is no other reasonable thing the user could want, since the
        //     visible UI is a host list.
        //   * Explorer view: a path-shaped paste is an import only when
        //     focus is on the tree, otherwise we keep the current
        //     behaviour (typing a path string into the focused editor or
        //     terminal command line) because that is a legitimate use.
        // iTerm2's inline-image drag artefact ALWAYS short-circuits the
        // paste pipeline: a stray click-and-microdrag on an OSC-1337 cell
        // (activity-bar icon, welcome wordmark, no-repo / run-debug / SSH
        // hero) makes iTerm2 write the decoded PNG to $TMPDIR/iTerm2.<rand>.png
        // and re-inject the path as a bracketed paste. parse_dropped_paths
        // already filters those paths out of the import list, but without
        // an early-return here the fallthrough would still paste the raw
        // path string into the editor body (the user reported this as
        // "Pasted 67 chars" appearing in a freshly-opened CLAUDE.local.md
        // tab while they were trying to resize the sidebar near the
        // Explorer icon). Detect the artefact at the payload level and
        // silently drop the entire paste — no text, no import, no status.
        if parsed_drop_tokens(s)
            .iter()
            .any(|p| p.is_absolute() && is_iterm2_inline_image_drop(p))
        {
            return false;
        }
        let dropped = parse_dropped_paths(s);
        if !dropped.is_empty() {
            match self.sidebar_view {
                SidebarView::Remote if self.focus == Pane::Tree => {
                    self.import_paths_into_remote(&dropped);
                    return false;
                }
                SidebarView::Explorer if self.focus == Pane::Tree => {
                    self.import_paths_into_explorer(&dropped);
                    return false;
                }
                _ => {}
            }
        }
        // Remote-launched croft case: the path the user dragged from
        // their local Finder doesn't exist on this remote box, so the
        // strict parser above returned nothing. If the drop-relay env
        // is plumbed (set by the local-croft parent over the SSH
        // session) and the paste shape is path-like, request a reverse
        // pull through the relay.
        //
        // Two triggers gate the relay pull. Tree focus is the explicit one,
        // matching the strict branch above. The second, `is_pure_path_payload`,
        // recovers a genuine Finder drag: a drop arrives as a bracketed paste
        // and does NOT move keyboard focus to the tree, so a focus-only gate
        // silently dropped every drag where the user had last clicked the
        // terminal or editor. A real drop is nothing but absolute-path tokens,
        // whereas the text pastes the focus guard protects — a shell command or
        // code snippet that merely contains a `/...` token — always carry a
        // non-path token, so they still fall through to a normal paste rather
        // than being hijacked into a relay pull with no local file to fetch
        // (the regression seen on remote sessions opened from Termux).
        if self.drop_relay_active()
            && self.sidebar_view == SidebarView::Explorer
            && (self.focus == Pane::Tree || is_pure_path_payload(s))
        {
            let foreign = parse_foreign_dropped_paths(s);
            if !foreign.is_empty() {
                self.request_remote_pulls(&foreign);
                return false;
            }
        }
        if self.sidebar_view == SidebarView::Search && self.focus == Pane::Tree {
            self.paste_str_into_search(s);
            self.status = format!("Pasted {} chars", s.chars().count());
            return false;
        }
        // Source Control commit message box: bracketed paste must
        // populate it the same way Cmd+V via the key path does
        // (`handle_source_control_key` -> `source_control.insert_str`).
        // Without this branch, in the production setup where iTerm2
        // emits Cmd+V as a bracketed paste rather than a key event,
        // the payload silently vanishes when the user has the SC
        // sidebar focused.
        if self.sidebar_view == SidebarView::SourceControl
            && self.focus != Pane::Editor
            && self.focus != Pane::Terminal
        {
            self.source_control.insert_str(s);
            self.status = format!("Pasted {} chars", s.chars().count());
            return false;
        }
        match self.focus {
            Pane::Editor => {
                self.editor.paste_str(s, self.paste_reindent);
                self.status = format!("Pasted {} chars", s.chars().count());
                return true;
            }
            Pane::Terminal => {
                if self.terminal_find.is_some() {
                    // At the caret, like the clipboard key (#1729).
                    self.terminal_find_edit_at_caret(|t, at| {
                        crate::widgets::line_edit::insert_at(t, at, s)
                    });
                } else {
                    self.paste_terminal_input(s.as_bytes());
                }
            }
            Pane::Tree => {}
        }
        false
    }

    /// Ask the local-croft drop pump to fetch the user's macOS clipboard
    /// and stage it on this remote box. The drained pull resolves to a
    /// `paste_input` call on the embedded terminal — see `drain_remote_pulls`.
    pub(super) fn request_remote_clipboard(&mut self) {
        let Some(log_path) = self.relay_log_path() else {
            self.status = String::from("Cmd+V: drop relay vanished");
            return;
        };
        self.ensure_relay_dir();
        let request_id = Self::relay_request_id("clip");
        let mut line = String::from("clipboard\t");
        line.push_str(&request_id);
        line.push('\n');
        match append_to_relay_log(&log_path, &line) {
            Ok(()) => {
                self.pending_remote_pulls.push(PendingRemotePull {
                    request_id,
                    src_display: String::from("clipboard"),
                    basename: String::from("clipboard.txt"),
                    dest_dir: PathBuf::new(),
                    started_at: std::time::Instant::now(),
                    kind: RemotePullKind::Clipboard,
                });
                self.status = String::from("Cmd+V: fetching local Mac clipboard via croft relay");
            }
            Err(e) => {
                self.status = format!("Cmd+V: relay write failed: {e}");
            }
        }
    }

    /// Capture one key into the live recording. Recording control keys are
    /// excluded by the callers that own them (vim's `q`, the palette
    /// commands), so a macro can never contain the gesture that ends it.
    pub(super) fn record_macro_key(&mut self, key: crossterm::event::KeyEvent) {
        if let Some(rec) = self.macro_recording.as_mut() {
            rec.recorded.push_key(key);
        }
    }

    /// Hold back a recorded key that opened or went into the Command Palette
    /// (#952) until the palette closes: it then joins the macro with the rest
    /// of that palette session, so a palette command replays whole. The
    /// session that runs Macro: Start/Stop Recording never gets here with
    /// its Enter, because stopping drops the recording and what it held.
    /// One the palette was opened for without a key (a click) is dropped:
    /// replayed without the chord, its query would be typed into the file.
    /// So is one whose command took focus out of the editor, such as
    /// Terminal: Focus Terminal: the click that brings it back is not
    /// recorded, so a replay would type the keys after it into the shell.
    pub(super) fn hold_palette_macro_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        palette_before: bool,
    ) {
        let open_now = self.command_palette.is_some();
        let in_editor = self.focus == Pane::Editor;
        let Some(rec) = self.macro_recording.as_mut() else {
            return;
        };
        if !palette_before {
            // The key that opened the palette starts a session that
            // replays; whatever an earlier one held was never closed by a
            // key, so it has nothing to replay.
            rec.palette_keys.clear();
            rec.palette_keys.push(key);
        } else if !rec.palette_keys.is_empty() {
            rec.palette_keys.push(key);
        } else {
            crate::output::push(
                "Macros",
                crate::output::OutputLevel::Info,
                &format!("skipped {key:?}: the palette was not opened from the keyboard"),
            );
        }
        if open_now {
            return;
        }
        let held = std::mem::take(&mut rec.palette_keys);
        if in_editor {
            for k in held {
                rec.recorded.push_key(k);
            }
        } else if !held.is_empty() {
            crate::output::push(
                "Macros",
                crate::output::OutputLevel::Info,
                "skipped a palette command that left the editor: recording captures the editor pane only",
            );
        }
    }

    /// Drop what a palette session held for the macro once the palette has
    /// closed without a key, as a click outside it or on a row does (#952):
    /// a session that is never closed from the keyboard has nothing that
    /// replays, and a later session must not pick its keys up.
    pub(super) fn drop_unkeyed_palette_macro_keys(&mut self) {
        if self.command_palette.is_none()
            && let Some(rec) = self.macro_recording.as_mut()
        {
            rec.palette_keys.clear();
        }
    }

    /// Start recording into `register` (vim `q{a-z}`) or into the unnamed
    /// last-recording slot (the palette command). Starting while already
    /// recording stops instead, so one gesture toggles.
    pub(super) fn start_macro_recording(&mut self, register: Option<char>) {
        if self.macro_recording.is_some() {
            self.stop_macro_recording();
            return;
        }
        self.macro_recording = Some(crate::macros::Recording {
            register,
            recorded: crate::macros::Macro::default(),
            palette_keys: Vec::new(),
        });
        // One source of truth: vim's `q` branches on this, and a copy the
        // machine kept for itself drifted whenever the palette was used.
        self.vim.set_recording(true);
        self.status = match register {
            Some(r) => format!("Recording macro into @{r}"),
            None => String::from("Recording macro"),
        };
    }

    /// Finish the live recording: it becomes the last macro either way, and
    /// a named register is also persisted. An empty recording is discarded
    /// rather than replacing a useful macro with a no-op.
    pub(super) fn stop_macro_recording(&mut self) {
        let Some(rec) = self.macro_recording.take() else {
            return;
        };
        self.vim.set_recording(false);
        if rec.recorded.is_empty() {
            self.status = String::from("Macro discarded (nothing recorded)");
            return;
        }
        let len = rec.recorded.len();
        if let Some(r) = rec.register {
            let name = r.to_string();
            self.macro_registers
                .insert(name.clone(), rec.recorded.clone());
            if let Err(e) =
                crate::macros::save_register(&self.macros_path, &name, rec.recorded.clone())
            {
                crate::output::push("Macros", crate::output::OutputLevel::Warn, &e);
            }
            // Written by croft itself: nothing for the config watch to reload.
            self.config_watch.note(&self.macros_path.clone());
            self.status = format!("Recorded {len} keys into @{r}");
        } else {
            self.status = format!("Recorded {len} keys");
        }
        self.macro_last = Some(rec.recorded);
    }

    /// Replay `mac` `count` times by feeding its keys back through
    /// `handle_key`. Guarded three ways: a re-entrancy flag (a macro cannot
    /// invoke itself), a total-key budget (a pathological count cannot spin
    /// the UI), and an abort on the first key that errors, which leaves the
    /// buffer at the abort point with earlier iterations intact.
    pub(super) fn replay_macro(&mut self, mac: crate::macros::Macro, count: usize) {
        const MAX_REPLAY_KEYS: usize = 100_000;
        if self.macro_replaying {
            self.status = String::from("Macro is already replaying");
            return;
        }
        if mac.is_empty() {
            self.status = String::from("Macro is empty");
            return;
        }
        let count = count.max(1);
        // Budget the steps actually fed, not the recorded step count:
        // `replay_steps` drops steps this build cannot replay, so `len()` is
        // an upper bound rather than the real total.
        let keys = mac.replay_steps();
        if keys.is_empty() {
            // Every step was one this build cannot replay (a macro from a
            // newer croft). Say so rather than reporting a successful run
            // of nothing.
            self.status = String::from("Macro has no steps this version can replay");
            return;
        }
        // A paste is one step but inserts all of its text, so its characters
        // count against the budget like typed ones.
        let per_run: usize = keys
            .iter()
            .map(|step| match step {
                crate::macros::Replay::Key(_) => 1,
                crate::macros::Replay::Paste(text) => text.chars().count().max(1),
            })
            .sum();
        let total = per_run.saturating_mul(count);
        if total > MAX_REPLAY_KEYS {
            self.status =
                format!("Macro would run {total} keys (limit {MAX_REPLAY_KEYS}) — refused");
            return;
        }
        self.macro_replaying = true;
        let mut done = 0usize;
        let mut aborted = None;
        'outer: for iteration in 0..count {
            // Break coalescing so a new iteration's opening keystrokes never
            // merge into the previous iteration's typing run. This bounds
            // iterations apart; it does NOT make one iteration one undo step
            // — `push_undo` coalesces only a typing run, so a macro
            // holding a delete plus an insert still undoes in two. Wrapping
            // the iteration in `Editor::begin_undo_group` would not do it
            // either: every vim command the macro runs ends that group (#934).
            self.editor.break_undo_coalescing();
            for step in &keys {
                match step {
                    crate::macros::Replay::Key(k) => {
                        if let Err(e) = self.handle_key(*k) {
                            aborted = Some((iteration + 1, e.to_string()));
                            break 'outer;
                        }
                    }
                    // Pasted, not typed, as it was recorded (#1464).
                    crate::macros::Replay::Paste(text) => self.handle_paste(text),
                }
            }
            done = iteration + 1;
        }
        self.macro_replaying = false;
        self.status = match aborted {
            Some((at, e)) => format!("Macro aborted on iteration {at}: {e}"),
            None if done == 1 => String::from("Macro replayed"),
            None => format!("Macro replayed {done}×"),
        };
    }

    /// Replay the last recording (palette "Macro: Replay Last", vim `@@`).
    pub(super) fn replay_last_macro(&mut self, count: usize) {
        match self.macro_last.clone() {
            Some(m) => self.replay_macro(m, count),
            None => self.status = String::from("No macro recorded yet"),
        }
    }

    /// Replay a named register (vim `@{a-z}`).
    pub(super) fn replay_macro_register(&mut self, register: char, count: usize) {
        match self.macro_registers.get(&register.to_string()).cloned() {
            Some(m) => {
                self.macro_last = Some(m.clone());
                self.replay_macro(m, count);
            }
            None => self.status = format!("Register @{register} is empty"),
        }
    }

    /// The `(rows, cols)` a column-select key moves the box head (#1560):
    /// Cmd/Ctrl+Shift+Alt with an arrow moves one row or column and with
    /// PageUp/PageDown one screen; while Column Selection Mode is on, plain
    /// Shift does the same. `None` for every other key.
    pub(super) fn column_select_key(&self, key: KeyEvent) -> Option<(isize, isize)> {
        let m = key.modifiers;
        let shift = m.contains(KeyModifiers::SHIFT);
        let alt = m.contains(KeyModifiers::ALT);
        let cmd = m.contains(KeyModifiers::SUPER) || m.contains(KeyModifiers::CONTROL);
        let chord = cmd && shift && alt;
        let mode = self.editor.column_selection_mode && shift && !cmd && !alt;
        if !chord && !mode {
            return None;
        }
        let page = self.editor.column_select_page();
        Some(match key.code {
            KeyCode::Up => (-1, 0),
            KeyCode::Down => (1, 0),
            KeyCode::Left => (0, -1),
            KeyCode::Right => (0, 1),
            KeyCode::PageUp => (-page, 0),
            KeyCode::PageDown => (page, 0),
            _ => return None,
        })
    }

    /// View: Toggle Column Selection Mode (#1560): while on, Shift+arrows
    /// grow a column selection.
    pub(super) fn toggle_column_selection_mode(&mut self) {
        self.editor.column_selection_mode = !self.editor.column_selection_mode;
        self.status = format!(
            "Column Selection Mode {}",
            if self.editor.column_selection_mode {
                "on"
            } else {
                "off"
            }
        );
    }

    /// Type `c` into the active editor in the app's typing mode. The mode is
    /// copied first: a tab switched to in the same input batch has not had
    /// its `overwrite` flag synced by the draw yet.
    pub(super) fn type_char_in_editor(&mut self, c: char) {
        self.editor.overwrite = self.overwrite_mode;
        self.editor.insert_char(c);
    }

    /// Flip Insert/Overwrite typing (the Insert key).
    pub(super) fn toggle_overwrite_mode(&mut self) {
        self.overwrite_mode = !self.overwrite_mode;
        self.editor.overwrite = self.overwrite_mode;
        // Text typed before and after the switch undo as separate steps.
        self.editor.break_undo_coalescing();
        self.status = format!(
            "{} mode",
            if self.overwrite_mode {
                "Overwrite"
            } else {
                "Insert"
            }
        );
    }

    /// Make the terminal's caret match the typing mode: a block while the
    /// editor has focus in overwrite mode, the steady bar croft runs with
    /// otherwise. Emitted only when the wanted shape changes.
    pub(super) fn sync_caret_style(&mut self) {
        let want_block = self.overwrite_mode && self.focus == Pane::Editor;
        if want_block == self.caret_block_sent {
            return;
        }
        self.caret_block_sent = want_block;
        let style = if want_block {
            crossterm::cursor::SetCursorStyle::SteadyBlock
        } else {
            crossterm::cursor::SetCursorStyle::SteadyBar
        };
        let mut out = stdout();
        let _ = crossterm::execute!(out, style);
    }

    /// Step the gutter's line numbers through on, relative, interval and
    /// off (#1576), on every editor, and save the choice.
    pub(super) fn cycle_line_numbers(&mut self) {
        self.line_numbers = self.line_numbers.next();
        self.apply_line_numbers();
        self.status = format!("Line Numbers: {}", self.line_numbers.pref_id());
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_line_numbers(
                self.line_numbers.pref_id(),
            ));
        }
    }

    /// Give every open editor, in every split group, the current line
    /// number mode (#1576). A split beside the active one is on screen
    /// too, and nothing else resyncs it until focus next moves.
    pub(super) fn apply_line_numbers(&mut self) {
        let mode = self.line_numbers;
        for group in
            std::iter::once(&mut self.editor).chain(self.editor_layout.inactive_groups_mut())
        {
            for ed in &mut group.editors {
                ed.line_numbers = mode;
            }
        }
    }

    pub(super) fn toggle_render_whitespace(&mut self) {
        self.whitespace_mode = self.whitespace_mode.next();
        self.editor.whitespace_mode = self.whitespace_mode;
        self.status = format!("Render Whitespace: {}", self.whitespace_mode.label());
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_render_whitespace(
                self.whitespace_mode.pref_id(),
            ));
        }
    }

    pub(super) fn toggle_bracket_colors(&mut self) {
        self.bracket_colors_enabled = !self.bracket_colors_enabled;
        self.editor.show_bracket_colors = self.bracket_colors_enabled;
        self.status = format!(
            "Bracket Pair Colorization: {}",
            if self.bracket_colors_enabled {
                "on"
            } else {
                "off"
            }
        );
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_bracket_colors(
                self.bracket_colors_enabled,
            ));
        }
    }

    pub(super) fn toggle_indent_guides(&mut self) {
        self.indent_guides_enabled = !self.indent_guides_enabled;
        self.editor.show_indent_guides = self.indent_guides_enabled;
        self.status = format!(
            "Indent Guides: {}",
            if self.indent_guides_enabled {
                "on"
            } else {
                "off"
            }
        );
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_indent_guides(
                self.indent_guides_enabled,
            ));
        }
    }

    pub(super) fn toggle_auto_close_pairs(&mut self) {
        self.auto_close_pairs = !self.auto_close_pairs;
        self.editor.auto_close_pairs = self.auto_close_pairs;
        self.status = format!(
            "Auto Closing Pairs: {}",
            if self.auto_close_pairs { "on" } else { "off" }
        );
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_auto_close_pairs(self.auto_close_pairs));
        }
    }

    /// VS Code's `editor.formatOnType` toggle (#254), persisted like the
    /// other editor preferences.
    pub(super) fn toggle_format_on_type(&mut self) {
        self.format_on_type = !self.format_on_type;
        self.status = if self.format_on_type {
            String::from("Format on Type: on")
        } else {
            String::from("Format on Type: off")
        };
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_format_on_type(self.format_on_type));
        }
    }

    /// Flip `editor.formatOnSave`, persist it, and report the new state.
    /// Persistence is skipped under test so the suite never writes the
    /// developer's `~/.config/croft/config.json` (mirrors the layout prefs).
    pub(super) fn toggle_format_on_save(&mut self) {
        self.format_on_save = !self.format_on_save;
        self.status = if self.format_on_save {
            String::from("Format on Save: on")
        } else {
            String::from("Format on Save: off")
        };
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_format_on_save(self.format_on_save));
        }
    }
}

/// Read the system clipboard. Used by the search input when Cmd+V arrives as
/// a key event rather than bracketed paste content.
pub(super) fn read_system_clipboard() -> Option<String> {
    crate::clipboard::read_string()
}

/// VS Code "Copy Relative Path" text: `path` relative to the workspace `root`,
/// or the absolute `path` when it lives outside the root (never a `../` walk).
/// Pure so the relativization is unit-tested without touching the clipboard.
pub(super) fn relative_clipboard_text(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Where `App::copy_where_pasted` put a copy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum CopyLanding {
    /// Queued home over the drop relay, to the machine the user types at.
    /// The relay sends no word back on whether that clipboard took it, so
    /// the status says it was sent, not that it landed.
    Local,
    /// This machine's clipboard.
    Clipboard,
    /// Only an OSC 52 sequence, which the host terminal may ignore.
    Osc52,
}

impl CopyLanding {
    /// The status line for copying `what`, saying honestly where it went.
    pub(super) fn status(self, what: &str) -> String {
        match self {
            CopyLanding::Local => format!("Sent {what} to the local clipboard"),
            CopyLanding::Clipboard => format!("Copied {what} to clipboard"),
            CopyLanding::Osc52 => format!("Copied {what} via OSC 52 (the host terminal decides)"),
        }
    }
}

pub(super) fn chord_status_text(op: char, count: usize) -> String {
    if count > 0 {
        format!("Pending: {op} (count {count})")
    } else {
        format!("Pending: {op}")
    }
}

pub(super) fn is_vim_chord_letter(key: KeyEvent, letter: char) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if c != letter {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::CONTROL)
}

pub(super) fn is_vim_chord_g(key: KeyEvent) -> bool {
    is_vim_chord_letter(key, 'g')
}

pub(super) fn is_vim_chord_y(key: KeyEvent) -> bool {
    is_vim_chord_letter(key, 'y')
}

pub(super) fn is_vim_goto_bottom(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'g') {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::CONTROL)
}

pub(super) fn chord_digit_value(key: KeyEvent) -> Option<usize> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    let d = c.to_digit(10)? as usize;
    if key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::SHIFT) {
        return None;
    }
    Some(d)
}

pub(super) fn cmd_only_digit_value(key: KeyEvent) -> Option<usize> {
    let d = chord_digit_value(key)?;
    if !key.modifiers.contains(KeyModifiers::SUPER) {
        return None;
    }
    Some(d)
}

pub(super) fn reverse_find(kind: crate::vim::FindKind) -> crate::vim::FindKind {
    use crate::vim::FindKind;
    match kind {
        FindKind::CharForward => FindKind::CharBackward,
        FindKind::CharBackward => FindKind::CharForward,
        FindKind::TillForward => FindKind::TillBackward,
        FindKind::TillBackward => FindKind::TillForward,
    }
}

/// Vertical / whole-file motions operate linewise under an operator (`dj`,
/// `dG`), unlike the charwise horizontal and word motions.
pub(super) fn vim_motion_is_linewise(motion: crate::vim::Motion) -> bool {
    use crate::vim::Motion;
    matches!(
        motion,
        Motion::Up | Motion::Down | Motion::FileStart | Motion::FileEnd | Motion::GotoLine(_)
    )
}

/// `f`/`t`/`e` land on a char that the operator should include, so the
/// deletion range needs to extend one past the landing column.
pub(super) fn vim_motion_is_forward_inclusive(motion: crate::vim::Motion) -> bool {
    use crate::vim::{FindKind, Motion};
    matches!(
        motion,
        Motion::WordEnd
            | Motion::Find(FindKind::CharForward, _)
            | Motion::Find(FindKind::TillForward, _)
    )
}

/// First char index `>= from` where `term` begins in `line`, comparing by
/// char (not byte) so the result indexes the editor's char-based cursor.
pub(super) fn find_char_index(line: &str, term: &str, from: usize) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let needle: Vec<char> = term.chars().collect();
    let tlen = needle.len();
    if tlen == 0 || chars.len() < tlen {
        return None;
    }
    (from..=chars.len() - tlen).find(|&i| chars[i..i + tlen] == needle[..])
}

/// Last char index `< limit` where `term` begins in `line`.
pub(super) fn rfind_char_index(line: &str, term: &str, limit: usize) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let needle: Vec<char> = term.chars().collect();
    let tlen = needle.len();
    if tlen == 0 || chars.len() < tlen {
        return None;
    }
    let upper = limit.min(chars.len() - tlen + 1);
    (0..upper).rev().find(|&i| chars[i..i + tlen] == needle[..])
}

pub(super) fn is_plain_letter(key: KeyEvent, letter: char) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if c != letter {
        return false;
    }
    !(key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER)
        || key.modifiers.contains(KeyModifiers::ALT))
}
