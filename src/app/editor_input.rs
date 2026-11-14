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
