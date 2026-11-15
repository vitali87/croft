//! `handle_key` and `handle_key_inner`, Cmd+K chords, every `is_*_key` predicate, the
//! key and mouse contexts for user keymaps, voice input and the on-screen keyboard.

use super::*;

/// How long an armed `Cmd+K` leader waits for its second key.
pub(super) const CMD_K_LEADER_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

impl App {
    /// [`ctrl_k_leads`] for the pane that has the keyboard now.
    pub(super) fn ctrl_k_leads_here(&self) -> bool {
        let shell_focused = self.focus == Pane::Terminal
            && matches!(self.bottom_panel_tab, BottomPanelTab::Terminal);
        ctrl_k_leads(
            cfg!(target_os = "macos"),
            shell_focused,
            self.vim.enabled && self.focus == Pane::Editor,
        )
    }

    /// Dispatch the second key of a `Cmd+K`-prefixed chord (VS Code's two-key
    /// model). Returns `true` if the key completed a chord and was consumed,
    /// `false` if it matched nothing (the caller then processes it normally).
    /// The second key reaches us as ordinary input, so `Cmd` on the trailing
    /// key (e.g. `Cmd+K Cmd+T`) is accepted but not required.
    pub(super) fn handle_cmd_k_chord(&mut self, key: KeyEvent) -> bool {
        let plain = !key.modifiers.contains(KeyModifiers::ALT);
        let shifted = key.modifiers.contains(KeyModifiers::SHIFT);
        // `Cmd` held on the second key, for the chords that ask for it. Off
        // macOS `Ctrl` counts, as it does for the leader (#843): VS Code's
        // Linux `Ctrl+K Ctrl+S` is the Keyboard Shortcuts editor.
        let cmd_held = has_cmd_or_linux_ctrl(key.modifiers);
        match key.code {
            // Cmd+K N in the editor: leave a sticky note on this line (#367).
            // In a terminal the same chord keeps its meaning (below).
            KeyCode::Char(c)
                if plain
                    && !shifted
                    && c.eq_ignore_ascii_case(&'n')
                    && self.focus == Pane::Editor =>
            {
                self.open_sticky_note_prompt();
                true
            }
            // Cmd+K ⇧C / ⇧S / ⇧R with the terminal focused: the keyboard
            // forms of the decoration-menu actions, applied to the LAST
            // finished command (VS Code's copyLastCommandOutput family).
            // Must precede the case-insensitive C/S/R editor arms below.
            KeyCode::Char('C' | 'S' | 'R') if shifted && plain && self.focus == Pane::Terminal => {
                let KeyCode::Char(c) = key.code else {
                    return false;
                };
                let Some(d) = self.last_command_decoration() else {
                    self.status = String::from("No finished commands yet");
                    return true;
                };
                let idx = self.active_terminal;
                match c {
                    'C' => self.copy_command_output(idx, d),
                    'S' => self.select_command_output(idx, d),
                    _ => self.rerun_command(idx, d),
                }
                true
            }
            // Cmd+K Shift+T: reopen the most recently closed terminal pane
            // (the browser reopen-tab convention under the Cmd+K leader).
            // Cmd+K Cmd+S (Cmd held on the second key): the Keyboard Shortcuts
            // editor, VS Code's binding (#612). Before the plain S arm, which
            // keeps Select for Compare.
            KeyCode::Char(c) if c.eq_ignore_ascii_case(&'s') && cmd_held && !shifted => {
                self.open_keyboard_shortcuts();
                true
            }
            // Cmd+K Shift+F / Shift+R: open the search results as a Search
            // Editor, and re-run the active one (#615). Before the plain F
            // and R arms; Shift+R in the terminal keeps its own meaning.
            KeyCode::Char(c) if shifted && plain && c.eq_ignore_ascii_case(&'f') => {
                self.open_search_editor();
                true
            }
            KeyCode::Char(c)
                if shifted
                    && plain
                    && c.eq_ignore_ascii_case(&'r')
                    && self.focus != Pane::Terminal =>
            {
                self.rerun_search_editor();
                true
            }
            // Cmd+K Shift+G: inline AI suggestions on / off (#607).
            KeyCode::Char(c) if shifted && plain && c.eq_ignore_ascii_case(&'g') => {
                self.toggle_inline_suggestions();
                true
            }
            // Cmd+K Shift+E: run the caret line's code lens (#608). Must
            // precede the plain E (reveal in Explorer) arm below.
            KeyCode::Char(c) if shifted && plain && c.eq_ignore_ascii_case(&'e') => {
                self.run_code_lens_at_cursor();
                true
            }
            // Cmd+K Shift+U / Shift+D: supertypes ("up") and subtypes
            // ("down") of the type at the caret (#613). Case-insensitive for
            // CSI-u hosts; must precede the plain U and D arms below.
            KeyCode::Char(c) if shifted && plain && c.eq_ignore_ascii_case(&'u') => {
                self.request_type_hierarchy_at_cursor(true);
                true
            }
            KeyCode::Char(c) if shifted && plain && c.eq_ignore_ascii_case(&'d') => {
                self.request_type_hierarchy_at_cursor(false);
                true
            }
            // Must precede the case-insensitive T theme arm below.
            KeyCode::Char('T') if shifted && plain => {
                self.undo_close_terminal();
                true
            }
            // Cmd+K Cmd+M (Ctrl+K Ctrl+M off macOS): View Bookmarks. Needs the
            // trailing Cmd, so plain `Cmd+K M` below still maximizes the
            // terminal pane.
            KeyCode::Char(c) if plain && cmd_held && c.eq_ignore_ascii_case(&'m') => {
                self.open_bookmark_picker();
                true
            }
            // Cmd+K Shift+W: reopen the most recently closed editor tab
            // (VS Code "Reopen Closed Editor"; ⇧W pairs with Cmd+W the way
            // ⇧T pairs with the terminal close). Matched case-insensitively
            // because CSI-u hosts report Shift+W as `Char('w') + SHIFT`
            // while iTerm2's forwarder delivers `Char('W')`. Must precede
            // any case-insensitive W arm.
            KeyCode::Char(c) if shifted && plain && c.eq_ignore_ascii_case(&'w') => {
                self.reopen_closed_tab();
                true
            }
            // Cmd+K Shift+N with the terminal focused: delete the
            // annotation(s) under the selection. Must precede any
            // case-insensitive N arm.
            KeyCode::Char('N') if shifted && plain && self.focus == Pane::Terminal => {
                self.delete_terminal_annotations_in_selection();
                true
            }
            // Cmd+K N with the terminal focused: pin a note to the selected
            // output span (iTerm2's annotations); over an existing note it
            // edits in place.
            KeyCode::Char(c)
                if plain && c.eq_ignore_ascii_case(&'n') && self.focus == Pane::Terminal =>
            {
                self.begin_annotate_terminal();
                true
            }
            // Cmd+K Cmd+T / Cmd+K T: open the Color Theme picker.
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'t') => {
                self.open_theme_picker();
                true
            }
            // Cmd+K A: who is attached to this multiplayer session
            // (Session: Participants).
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'a') => {
                self.open_participants_picker();
                true
            }
            // Cmd+K X: cancel the AI stream (croft pair) — the keyboard
            // form of the gutter stop button; the pilot reverts the
            // streamed text.
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'x') => {
                self.collab_cancel_stream();
                true
            }
            // Cmd+K Cmd+Q: Go to Last Edit Location (VS Code's chord; the
            // second key carries Cmd, which keeps it distinct from the
            // navigator's plain Cmd+K Q below).
            KeyCode::Char(c) if c.eq_ignore_ascii_case(&'q') && cmd_held => {
                self.goto_last_edit_location();
                true
            }
            // Cmd+K Shift+Q: Session: Detach (#679). Normally the attach
            // client takes this chord before it reaches croft; this arm
            // serves a leader the client released after its pause.
            KeyCode::Char(c) if shifted && plain && c.eq_ignore_ascii_case(&'q') => {
                self.detach_session_client();
                true
            }
            // Cmd+K Q: ask the navigator about the caret line, or the
            // selected lines when a selection is active (Q for question).
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'q') => {
                let (range, selection) = match self.editor.selection_rows() {
                    Some(rows) => (rows, self.editor.selection_text()),
                    None => {
                        let row = self.editor.cursor_row;
                        ((row, row), String::new())
                    }
                };
                self.open_ask_navigator(range, selection);
                true
            }
            // Cmd+K Y: yield the turn — the navigator reviews the active
            // file comment-only; its say lands as gutter diamonds and
            // OUTPUT commentary, never as edits.
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'y') => {
                self.yield_to_navigator();
                true
            }
            // Cmd+K Shift+Cmd+\: open a second view of the active file beside it
            // ("Split in Group"). Must precede the plain Cmd+\ (Split Up) arm,
            // which ignores Shift. Shift+\ may arrive as '|' on some layouts.
            KeyCode::Char('\\' | '|') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.split_in_group();
                true
            }
            // Cmd+K Cmd+\ (or Cmd+K \): split the active group UP (stacked, the
            // new group above). Plain Cmd+\ already splits Right.
            KeyCode::Char('\\') if plain => {
                self.split_editor_dir(editor_layout::SplitDir::Vertical, false);
                true
            }
            // Cmd+K →: close the editors to the right of the active tab.
            KeyCode::Right if plain => {
                self.close_tabs_to_right(self.editor.active_index());
                true
            }
            // Cmd+K S: mark the active file as the compare anchor.
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'s') => {
                if let Some(path) = self.editor.path.clone() {
                    let label = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    self.compare_anchor = Some(path);
                    self.status = format!("Selected {label} for compare");
                } else {
                    self.status = String::from("Select for Compare: no active file");
                }
                true
            }
            // Cmd+K C: diff the active file against the compare anchor.
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'c') => {
                match (self.compare_anchor.clone(), self.editor.path.clone()) {
                    (Some(anchor), Some(other)) => {
                        match self.open_diff_across_groups(&anchor, &other) {
                            Ok(()) => {
                                self.focus_pane(Pane::Editor);
                                self.compare_anchor = None;
                                self.sync_open_file_poll_mtime();
                                let l = anchor
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| anchor.display().to_string());
                                let r = other
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| other.display().to_string());
                                self.status = format!("Diff: {l} \u{2194} {r}");
                            }
                            Err(e) => self.status = format!("Diff failed: {e}"),
                        }
                    }
                    (None, _) => {
                        self.status = String::from("Compare with Selected: nothing selected yet")
                    }
                    (_, None) => {
                        self.status = String::from("Compare with Selected: no active file")
                    }
                }
                true
            }
            // Cmd+K W: close all editor tabs.
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'w') => {
                self.close_all_tabs();
                true
            }
            // Cmd+K U: close all saved (non-dirty) editor tabs.
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'u') => {
                self.close_saved_tabs();
                true
            }
            // Cmd+K E: reveal the active file in the Explorer tree.
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'e') => {
                if let Some(path) = self.editor.path.clone() {
                    self.reveal_in_explorer(path);
                } else {
                    self.status = String::from("Reveal in Explorer View: no active file");
                }
                true
            }
            // Cmd+K Shift+H: outgoing calls (what does this call?). Must
            // precede the case-insensitive H arm below.
            KeyCode::Char('H') if shifted && plain => {
                self.request_call_hierarchy_at_cursor(false);
                true
            }
            // Cmd+K G: accept the ssh-pane workspace offer (#364) — "go to
            // the host". The same remote flow the palette command runs; the
            // offer only saves the reaching.
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'g') => {
                match self.accept_ssh_offer() {
                    Some((host, path)) => self.request_remote_launch(host, path),
                    None => {
                        self.status = String::from(
                            "No ssh workspace offer is open (it appears when a pane connects to a host in ~/.ssh/config)",
                        );
                    }
                }
                true
            }
            // Cmd+K H: incoming calls — who calls the symbol at the caret
            // (VS Code's call Hierarchy, peek replaced by croft's picker).
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'h') => {
                self.request_call_hierarchy_at_cursor(true);
                true
            }
            // Cmd+K Shift+V: view the symbol at the caret in a tab of its
            // own (#369; croft binding, VS Code has no such command). Must
            // precede Live Run's case-insensitive Cmd+K V arm.
            KeyCode::Char(c) if shifted && plain && c.eq_ignore_ascii_case(&'v') => {
                self.open_symbol_tab();
                true
            }
            // Cmd+K P: pin / unpin the active tab. This chord was Cmd+K
            // Shift+Enter (and Keep Open bare Cmd+K Enter) — both silently
            // SHADOWED the later, documented Cmd+K Enter run-test-at-cursor
            // arms below, so the Enter family now belongs to Testing alone.
            // Cmd+K ⇧P: keep the active preview tab open (sibling of the pin
            // chord below). Must precede the case-insensitive 'p' arm, which
            // would swallow the shifted press.
            KeyCode::Char('P') if shifted && plain => {
                self.keep_tab_open(self.editor.active_index());
                true
            }
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'p') => {
                self.toggle_tab_pin(self.editor.active_index());
                true
            }
            // Cmd+K Shift+O: move the active tab into a new window. Must precede
            // the plain Cmd+K O (Copy) arm, which ignores Shift.
            KeyCode::Char(c)
                if c.eq_ignore_ascii_case(&'o') && key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.move_into_new_window(self.editor.active_index());
                true
            }
            // Cmd+K O: copy the active tab into a new window (VS Code's binding).
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'o') => {
                self.copy_into_new_window(self.editor.active_index());
                true
            }
            // Cmd+K Z: toggle Zen Mode (VS Code's binding).
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'z') => {
                self.toggle_zen_mode();
                true
            }
            // Cmd+K V: Live Run, "values" for the active Python file. On a
            // Markdown tab it is VS Code's Open Preview to the Side (#1605);
            // Live Run runs only Python, so no tab wants both.
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'v') => {
                if self.editor.language() == Some(crate::highlight::LangKind::Markdown) {
                    self.open_markdown_preview_to_side();
                } else {
                    self.toggle_live_run();
                }
                true
            }
            // Cmd+K Cmd+F (SUPER/CTRL held on the second key): Format
            // Selection, VS Code's binding (#254). Must precede the plain
            // Cmd+K F arm, which keeps croft's format-on-save toggle.
            KeyCode::Char(c)
                if plain
                    && c.eq_ignore_ascii_case(&'f')
                    && (key.modifiers.contains(KeyModifiers::SUPER)
                        || key.modifiers.contains(KeyModifiers::CONTROL)) =>
            {
                self.format_selection();
                true
            }
            // Cmd+K F: toggle Format on Save (croft binding; VS Code leaves this
            // preference unbound, but every croft action carries an accelerator).
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'f') => {
                self.toggle_format_on_save();
                true
            }
            // Cmd+K K: clear the active terminal (VS Code clears with Cmd+K).
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'k') => {
                if !self.show_terminal {
                    self.show_terminal = true;
                }
                self.clear_terminal_at(self.active_terminal);
                true
            }
            // Cmd+K R: rename the active terminal pane.
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'r') => {
                if !self.show_terminal {
                    self.show_terminal = true;
                }
                self.begin_rename_terminal(self.active_terminal);
                true
            }
            // Cmd+K M: maximize the active terminal pane / restore the even
            // split (croft binding; VS Code leaves its panel-maximize
            // relative unbound, and every croft action carries an
            // accelerator).
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'m') => {
                if !self.show_terminal {
                    self.show_terminal = true;
                }
                self.toggle_terminal_pane_maximize();
                true
            }
            // Cmd+K [ / Cmd+K ]: fold the active terminal pane to a strip,
            // and give every folded pane its width back (#313). VS Code binds
            // its own fold / unfold pair to ⌘K ⌘[ and ⌘K ⌘], so the gesture
            // keeps the keys it already has elsewhere.
            //
            // NOT ⌘K E, which the design proposal had picked as "even": that
            // is croft's reveal-in-Explorer and the arm would never have been
            // reached, since an earlier `e` arm in this same match wins.
            // NOT ⌘K -, either: `-` is close, on the button and here.
            //
            // Cmd+K M (pane-maximize) is untouched: maximize and collapse are
            // orthogonal and compose.
            KeyCode::Char('[') if plain => {
                if !self.show_terminal {
                    self.show_terminal = true;
                }
                self.collapse_active_terminal_pane();
                true
            }
            KeyCode::Char(']') if plain => {
                if !self.show_terminal {
                    self.show_terminal = true;
                }
                self.restore_all_terminal_panes();
                true
            }
            // Cmd+K D: dump the active terminal's scrollback into a scratch
            // editor tab (kitty's show_scrollback, with the editor as the
            // pager: find, vim mode, save, and path:line jumps come free).
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'d') => {
                self.open_scrollback_in_editor();
                true
            }
            // Cmd+K B: show the Testing view (B for the beaker icon). Cmd+Shift+T
            // — VS Code's Testing-ish chord — is already croft's focus-Terminal
            // chord, so the Cmd+K leader hosts this the way it hosts Zen (Cmd+K Z).
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'b') => {
                self.open_testing_view();
                true
            }
            // Cmd+K Cmd+I in the editor (Ctrl+K Ctrl+I off macOS): Show or
            // Focus Hover at the caret, VS Code's chord (#1518). Needs the
            // trailing Cmd, so plain Cmd+K I below stays broadcast input,
            // and outside the editor the chord broadcasts as before.
            KeyCode::Char(c)
                if plain
                    && cmd_held
                    && !shifted
                    && c.eq_ignore_ascii_case(&'i')
                    && self.focus == Pane::Editor =>
            {
                self.run_command(crate::widgets::command_palette::Command::ShowHover);
                true
            }
            // Cmd+K Shift+I: toggle whether the active pane receives
            // broadcast input (iTerm2's per-session broadcast-domain
            // toggle). Must precede the case-insensitive I arm below.
            KeyCode::Char('I') if shifted && plain => {
                self.toggle_broadcast_exclusion();
                true
            }
            // Cmd+K I: toggle broadcast input — type into every terminal
            // pane at once (iTerm2's Cmd+Opt+I; I for Input). Enabling is
            // gated on a confirm popup.
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'i') => {
                self.toggle_broadcast_input();
                true
            }
            // Cmd+K Shift+Enter: debug the test the caret sits in (checked
            // before the plain-Enter run arm, which also matches this chord).
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.debug_test_at_cursor();
                true
            }
            // Cmd+K Enter: run the test the editor caret sits in. Enter already
            // means "run" in the Testing view (runs all), so it carries over as
            // the run verb under the Cmd+K leader (no new terminal registration).
            KeyCode::Enter => {
                self.run_test_at_cursor();
                true
            }
            // Cmd+K Cmd+0 / Cmd+K 0: collapse every fold (VS Code "Fold All").
            KeyCode::Char('0') if plain => {
                self.editor.fold_all();
                self.poke_cursor();
                true
            }
            // Cmd+K Cmd+J / Cmd+K J: expand every fold (VS Code "Unfold All").
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'j') => {
                self.editor.unfold_all();
                self.poke_cursor();
                true
            }
            // Cmd+K Shift+L: Agent: New Worktree Lane (#348). Matched
            // case-insensitively for the same CSI-u / iTerm2 split as ⇧W,
            // and so it must precede the case-insensitive L arm below.
            KeyCode::Char(c) if shifted && plain && c.eq_ignore_ascii_case(&'l') => {
                self.open_new_lane_prompt();
                true
            }
            // Cmd+K Cmd+L / Cmd+K L: toggle the fold at the cursor line
            // (VS Code "Toggle Fold").
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'l') => {
                let row = self.editor.cursor_row;
                self.editor.toggle_fold(row);
                self.poke_cursor();
                true
            }
            // Cmd+K Cmd+/ : collapse every comment block (VS Code
            // "Fold All Block Comments", #254).
            KeyCode::Char('/') if plain => {
                self.editor
                    .fold_all_of_kind(crate::lsp::manager::FoldRangeKind::Comment);
                self.poke_cursor();
                true
            }
            // Cmd+K Cmd+8 / Cmd+K Cmd+9: fold / unfold every #region
            // (VS Code "Fold/Unfold All Regions", #254).
            KeyCode::Char('8') if plain => {
                self.editor
                    .fold_all_of_kind(crate::lsp::manager::FoldRangeKind::Region);
                self.poke_cursor();
                true
            }
            KeyCode::Char('9') if plain => {
                self.editor
                    .unfold_all_of_kind(crate::lsp::manager::FoldRangeKind::Region);
                self.poke_cursor();
                true
            }
            // Cmd+K Cmd+1 … Cmd+K Cmd+7: Fold Level N (#1653).
            KeyCode::Char(c @ '1'..='7') if plain => {
                self.editor.fold_level(c as usize - '0' as usize);
                self.poke_cursor();
                true
            }
            _ => false,
        }
    }

    /// Dispatch one key, capturing it into a live macro recording (#255).
    ///
    /// The capture happens AFTER dispatch and only if recording was already
    /// on both before and after: that is what keeps the gesture that ENDS a
    /// recording (vim's closing `q`, the palette's stop command) out of the
    /// macro it ends, and the gesture that STARTS one out of the macro it
    /// opens. Keys are recorded rather than resolved commands because only
    /// palette selections and user-rebound chords reach `run_command` — the
    /// built-in defaults call their app methods directly, so a command-level
    /// recorder would miss most of what a user does.
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        let key = if self.legacy_ctrl_punct {
            legacy_ctrl_punct(key)
        } else {
            key
        };
        self.editor.sync_occurrence_word_sel();

        // A key press means the status message has been seen: the right-hand
        // cluster takes its room back (#1609).
        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            self.status_wide = false;
        }
        // Screencast Mode reads where the key goes before it is dispatched,
        // while the prompt or pane it goes to is still the one open: typing
        // bound for a password field or the shell is then known to be that.
        // A key answering an armed `Cmd+K` is shown once it is dispatched,
        // as a chord step only if it completed one: one that matched nothing
        // goes on to the prompt or shell like any other key.
        let screencast_key = self.screencast.is_on()
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat);
        let leader_armed = screencast_key
            && self
                .cmd_k_leader
                .is_some_and(|at| at.elapsed() <= CMD_K_LEADER_TIMEOUT);
        let hide_typing = screencast_key && self.screencast_hides_typing();
        if screencast_key && !leader_armed {
            self.screencast
                .press(key, std::time::Instant::now(), false, hide_typing);
        }
        // Esc dismisses an open ssh-pane offer (#364) without being consumed:
        // the offer is a status-bar hint, not a modal, so whatever Esc meant
        // to the focused pane still happens.
        if key.code == KeyCode::Esc
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        {
            self.dismiss_ssh_offer();
        }
        let capture =
            self.macro_recording.is_some() && !self.macro_replaying && self.focus == Pane::Editor;
        let palette_before = self.command_palette.is_some();
        // Esc Esc leaves Zen Mode (#1585), counting only Escapes in the
        // editor that closed or cleared nothing: one that dismisses a popup
        // or a selection is that, not half of the pair.
        let zen_esc = self.zen_mode
            && self.focus == Pane::Editor
            && key.code == KeyCode::Esc
            && key.modifiers.is_empty()
            && key.kind == KeyEventKind::Press;
        let before = zen_esc.then(|| self.esc_fingerprint());
        let result = self.recording_file_switches(|app| app.handle_key_inner(key));
        // A line Enter only indented is cleared once the caret leaves it.
        self.trim_auto_indents();
        if leader_armed {
            let chord = self.screencast.take_chord_step();
            self.screencast
                .press(key, std::time::Instant::now(), chord, hide_typing);
        }
        self.screencast.end_key();
        match before {
            Some(before) if before == self.esc_fingerprint() => {
                self.note_idle_zen_esc(std::time::Instant::now());
            }
            _ if key.kind != KeyEventKind::Release => self.zen_esc_at = None,
            _ => {}
        }
        // Focus is re-checked after dispatch too: a key that hands focus to
        // the terminal would otherwise be recorded while everything after it
        // is dropped, producing a macro that replays a focus change and then
        // silently loses its payload.
        if capture && self.macro_recording.is_some() {
            if palette_before || self.command_palette.is_some() {
                self.hold_palette_macro_key(key, palette_before);
            } else if self.focus == Pane::Editor {
                self.record_macro_key(key);
            } else {
                // The issue asks for skipped keys to be logged rather than
                // vanishing: a macro that quietly stopped recording halfway
                // is otherwise indistinguishable from one that recorded
                // everything.
                crate::output::push(
                    "Macros",
                    crate::output::OutputLevel::Info,
                    &format!("skipped {key:?}: recording captures the editor pane only"),
                );
            }
        }
        result
    }

    pub(super) fn handle_key_inner(&mut self, key: KeyEvent) -> Result<()> {
        // Peek Definition popup: Enter converts to the real jump, Esc
        // closes, and any other key closes it and keeps its normal
        // meaning — the popup is a glance, never a mode (#115). Gated on
        // Press/Repeat like every other modal (the app-wide filter runs
        // just below), so a key RELEASE never dismisses or jumps.
        if self.peek_popup.is_some()
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        {
            match key.code {
                KeyCode::Enter => {
                    if let Some((path, line, col)) = self.peek_target.take() {
                        self.close_peek_popup();
                        self.go_to_definition(path, line, col);
                    }
                    return Ok(());
                }
                KeyCode::Esc => {
                    self.close_peek_popup();
                    return Ok(());
                }
                KeyCode::Up | KeyCode::Down if self.peek_refs.is_some() => {
                    self.step_peeked_reference(if key.code == KeyCode::Up { -1 } else { 1 });
                    return Ok(());
                }
                _ => self.close_peek_popup(),
            }
        }
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            return Ok(());
        }
        // Keyboard Shortcuts editor (#612): the next key is the new chord.
        // Only the key that answers the prompt: once the user has clicked
        // away or let it sit, a later Cmd+S must save, not become a binding.
        if let Some((cmd, at)) = self.recording_shortcut.take()
            && at.elapsed() < SHORTCUT_RECORD_WINDOW
        {
            self.record_shortcut(cmd, key);
            return Ok(());
        }
        // A chord that was taken (#1482): Enter reassigns it, any other key
        // keeps both shortcuts as they were. The recorded chord's own key
        // repeat, from holding it a moment too long, is not an answer.
        if let Some((cmd, chord, owners)) = self.confirming_shortcut.take() {
            if key.kind == KeyEventKind::Repeat
                && crate::keymap::Chord::from_event(key) == crate::keymap::Chord::from_event(chord)
            {
                self.confirming_shortcut = Some((cmd, chord, owners));
                return Ok(());
            }
            if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                self.write_shortcut(cmd, chord, &owners);
            } else {
                self.status = String::from("Shortcut unchanged");
            }
            return Ok(());
        }
        self.hover_popup = None;
        self.hover_diagnostic = None;
        self.hover_request_id = None;
        // `Cmd+K` leader: if armed, this keystroke is the second half of a
        // `Cmd+K`-prefixed chord. Consume the leader regardless; dispatch the
        // chord if it matches and the window has not lapsed, otherwise fall
        // through so the key keeps its normal meaning (e.g. Esc cancels).
        if let Some(started) = self.cmd_k_leader.take()
            && started.elapsed() <= CMD_K_LEADER_TIMEOUT
            && self.handle_cmd_k_chord(key)
        {
            self.screencast.note_chord_step();
            return Ok(());
        }
        // Modal layer: the unsupported-terminal nudge sits on top of everything
        // at startup. Any key dismisses it for this session; D also persists the
        // dismissal so it never shows again. All keys are swallowed so the user
        // can't accidentally type through into the UI below.
        if self.pending_terminal_warning {
            match key.code {
                KeyCode::Char('d') | KeyCode::Char('D') => self.dismiss_terminal_warning(true),
                _ => self.dismiss_terminal_warning(false),
            }
            return Ok(());
        }
        if self.approval_ui.is_some() {
            self.handle_approval_key(key);
            return Ok(());
        }
        if self.connect_dialog.is_some() {
            self.handle_connect_dialog_key(key);
            return Ok(());
        }
        if self.shortcuts_modal.is_some() {
            self.handle_shortcuts_modal_key(key);
            return Ok(());
        }
        if self.settings_editor.is_some() && self.input_prompt.is_none() {
            self.handle_settings_editor_key(key);
            return Ok(());
        }

        // The tour (#377): Esc leaves it at any time, as its first caption
        // and its keys row say, from a palette, Quick Open or picker its step
        // opened too (#863). Esc there closed only that, so at the theme
        // picker a user taking the caption at its word stayed in the tour.
        // A modal the user opens mid-tour is theirs: its Esc just closes it.
        if key.code == KeyCode::Esc
            && key.modifiers.is_empty()
            && key.kind == KeyEventKind::Press
            && self.tour.as_ref().is_some_and(|run| run.step_opened_modal)
            && (self.command_palette.is_some()
                || self.file_finder.is_some()
                || self.context_menu.is_some())
        {
            if self.command_palette.is_some() {
                self.close_command_palette();
            }
            if self.file_finder.is_some() {
                self.close_file_finder();
            }
            if self.context_menu.take().is_some() {
                self.overlays.activity.mark_dirty();
            }
            self.finish_tour();
            return Ok(());
        }
        if self.file_finder.is_some() {
            self.handle_file_finder_key(key);
            return Ok(());
        }
        if self.command_palette.is_some() {
            self.handle_command_palette_key(key);
            return Ok(());
        }
        if self.workspace_symbols.is_some() {
            self.handle_workspace_symbols_key(key);
            return Ok(());
        }
        if self.go_to_symbol.is_some() {
            self.handle_go_to_symbol_key(key);
            return Ok(());
        }
        if self.process_picker.is_some() {
            self.handle_process_picker_key(key);
            return Ok(());
        }
        if self.zoxide_jump.is_some() {
            self.handle_zoxide_jump_key(key);
            return Ok(());
        }
        if self.command_history_popup.is_some() {
            self.handle_command_history_key(key);
            return Ok(());
        }
        if self.branch_picker.is_some() {
            self.handle_branch_picker_key(key);
            return Ok(());
        }
        if self.input_prompt.is_some() {
            self.handle_input_prompt_key(key);
            return Ok(());
        }
        if self.list_picker.is_some() {
            self.handle_list_picker_key(key);
            return Ok(());
        }
        // The lossy-save prompt (#1053): only a plain S writes. Enter, N and
        // Esc keep the file as it is, and every other key, the Cmd+S that
        // used to be the consent included, does nothing. It is painted over
        // the unsaved-changes prompt, which a deferred format-on-save write
        // can leave up beneath it, so it answers first.
        if self.pending_lossy_save.is_some() {
            let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
            match key.code {
                KeyCode::Char('s' | 'S') if plain => self.confirm_lossy_save(),
                KeyCode::Char('n' | 'N') if plain => self.cancel_lossy_save(),
                KeyCode::Enter | KeyCode::Esc => self.cancel_lossy_save(),
                _ => {}
            }
            return Ok(());
        }
        // The unsaved-changes prompt (#862): S or Enter saves, D goes on
        // without saving, N or Esc cancels. N is a cancel, not "don't save":
        // every other croft confirm reads N as "stop", and a reflex N must
        // not throw edits away. Letters count only unmodified, so a stray
        // Ctrl+D (add next match) cannot discard either.
        if self.pending_unsaved.is_some() {
            let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
            match key.code {
                KeyCode::Char('s' | 'S') if plain => self.answer_unsaved(true),
                KeyCode::Enter => self.answer_unsaved(true),
                KeyCode::Char('d' | 'D') if plain => self.answer_unsaved(false),
                KeyCode::Char('n' | 'N') if plain => self.cancel_unsaved(),
                KeyCode::Esc => self.cancel_unsaved(),
                _ => {}
            }
            return Ok(());
        }
        if self.pending_replace_all.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.confirm_pending_replace_all();
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.cancel_pending_replace_all();
                }
                _ => {}
            }
            return Ok(());
        }
        if self.pending_unsaved_scm.is_some() {
            match key.code {
                KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Enter => {
                    self.save_all_then_pending_scm();
                }
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    self.continue_pending_scm_without_saving();
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.cancel_pending_unsaved_scm();
                }
                _ => {}
            }
            return Ok(());
        }
        // Letters count only unmodified (Shift aside), so a chord such as
        // Ctrl+Shift+Y (terminal copy mode) cannot answer for the user.
        if self.pending_stash_drop.is_some() {
            let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
            match key.code {
                KeyCode::Char('y' | 'Y') if plain => self.confirm_pending_stash_drop(),
                KeyCode::Char('n' | 'N') if plain => {
                    self.pending_stash_drop = None;
                    self.status = String::from("Kept the stash");
                }
                KeyCode::Esc => {
                    self.pending_stash_drop = None;
                    self.status = String::from("Kept the stash");
                }
                _ => {}
            }
            return Ok(());
        }
        if self.pending_discard_all {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.confirm_pending_discard_all();
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.cancel_pending_discard_all();
                }
                _ => {}
            }
            return Ok(());
        }
        // The confirms answer in the order they are drawn, topmost first
        // (#1584): the run block over a terminal paste over broadcast.
        if self.pending_run_block.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.confirm_pending_run_block();
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.cancel_pending_run_block();
                }
                _ => {}
            }
            return Ok(());
        }
        if self.pending_terminal_paste.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.confirm_pending_terminal_paste(false);
                }
                KeyCode::Char('l') | KeyCode::Char('L') => {
                    self.confirm_pending_terminal_paste(true);
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.cancel_pending_terminal_paste();
                }
                _ => {}
            }
            return Ok(());
        }
        if self.pending_broadcast_enable {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.confirm_pending_broadcast();
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.cancel_pending_broadcast();
                }
                _ => {}
            }
            return Ok(());
        }
        // History scrubber (#371): arrows step, Home returns to the working
        // tree, Esc closes.
        //
        // BELOW every modal guard above, deliberately. The scrubber is a view
        // of the editor, not a mode over the window, so a palette, prompt or
        // picker opened while scrubbing keeps its own arrows. Placing it here
        // gets that by construction — each of those guards returns
        // unconditionally, so a key only reaches this line when none of them
        // wanted it. The alternative, a list of "overlays that take
        // precedence", silently rots the next time someone adds a modal.
        if self.scrubber.is_some()
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && self.handle_scrubber_key(key.code)
        {
            return Ok(());
        }
        // While a commit is on screen, keys that would edit the hidden live
        // buffer are refused rather than typed blind into it (#371).
        if self.scrub_view.is_some()
            && self.focus == Pane::Editor
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            && matches!(
                key.code,
                KeyCode::Char(_)
                    | KeyCode::Enter
                    | KeyCode::Backspace
                    | KeyCode::Delete
                    | KeyCode::Tab
            )
        {
            self.status = String::from(
                "Looking at history - Esc returns to your working tree before editing",
            );
            return Ok(());
        }
        // The tour (#377): with no picker or palette open, Enter moves on and
        // Esc leaves. Below the modal guards, so a picker a step opened keeps
        // its own Enter and Esc; the context menu is checked here because its
        // guard comes later, and the theme picker is one.
        if self.tour.is_some()
            && self.context_menu.is_none()
            && key.kind == KeyEventKind::Press
            && key.modifiers.is_empty()
            && matches!(key.code, KeyCode::Enter | KeyCode::Esc)
        {
            if key.code == KeyCode::Enter {
                self.advance_tour();
            } else {
                self.finish_tour();
            }
            return Ok(());
        }
        if self.scm_menu.open && matches!(key.code, KeyCode::Esc) {
            self.scm_menu.close();
            return Ok(());
        }
        // Macro recording (#255) taps the key here: every modal above has
        // already returned, so what reaches this point is editor work — the
        // thing a macro is for. Capturing keys rather than resolved commands
        // is deliberate: only palette selections and user-rebound chords
        // reach `run_command`, while the built-in defaults (Cmd+S, Alt+Up,
        // the debugger keys) call their app methods straight from here, so a
        // command-level recorder would miss most of what a user does.
        //
        // Replay feeds keys back through `handle_key`, so it is re-entrant by
        // construction — `replaying` keeps a replayed key from being recorded
        // into the macro that is producing it.
        // User keybindings (keybindings.json) win over the built-in chords, but
        // only when the terminal is not focused (its raw control keys must pass
        // through) and the chord carries a real modifier or is a function key
        // (so plain typing and bare Enter/Tab/Esc keep their contextual meaning).
        // A row with `when` fires only while that pane has focus (#1436); an
        // unscoped row fires wherever the terminal does not.
        if self.focus != Pane::Terminal
            && !self.keymap.is_empty()
            && is_rebindable_chord(key)
            && let Some(cmd) = match self.key_context() {
                Some(ctx) => self.keymap.command_in(key, ctx),
                None => self.keymap.command_for(key),
            }
        {
            self.run_command(cmd);
            return Ok(());
        }
        // Ctrl+Shift+PageUp / PageDown: View: Move Editor Left / Right
        // (#1565), VS Code's Linux and Windows chords. Ahead of the editor's
        // Shift+PageUp / PageDown page selection, which would otherwise read
        // them as a select with the Ctrl ignored.
        if self.focus == Pane::Editor
            && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
            && key.modifiers == KeyModifiers::CONTROL | KeyModifiers::SHIFT
        {
            self.move_active_tab(key.code == KeyCode::PageDown);
            return Ok(());
        }
        // A user's chord for a terminal command is meant for the terminal
        // (#1562), so it is taken before the shell sees the key; every other
        // chord still passes through to the shell. Only the Terminal tab has
        // a shell: on Output, Problems, Ports or Captures the key is the
        // panel's own.
        if self.focus == Pane::Terminal
            && self.bottom_panel_tab == BottomPanelTab::Terminal
            && !self.keymap.is_empty()
            && is_rebindable_chord(key)
            && let Some(cmd) = self.keymap.command_for(key).filter(|c| c.skips_the_shell())
        {
            self.run_command(cmd);
            return Ok(());
        }
        // A focused comment-box reply field owns the keyboard: typing edits
        // the draft, Enter sends, Esc leaves, F4 hops on. Sits below the
        // real modals (palette, prompts) and above every editor key.
        if self.editor.comment_focus.is_some() && self.focus == Pane::Editor {
            self.handle_comment_focus_key(key);
            return Ok(());
        }
        if matches!(key.code, KeyCode::F(1)) {
            self.open_shortcuts_modal();
            return Ok(());
        }
        if key.code == KeyCode::F(8) && key.modifiers == KeyModifiers::SHIFT {
            self.toggle_screencast_mode();
            return Ok(());
        }
        if matches!(key.code, KeyCode::F(8)) {
            self.perf.toggle();
            return Ok(());
        }
        // Debugger chords (VS Code defaults). F5 start/continue, Shift+F5 stop,
        // Shift+Cmd+F5 restart, Ctrl+F5 attach to a Python process, F9 toggle
        // breakpoint, F10 step over, F11/Shift+F11 step in/out. Cmd is SUPER on
        // macOS, Ctrl on Linux/Termux.
        //
        // A bare F5/F9/F10/F11 pressed while the terminal pane is focused and
        // no debug session is live belongs to the app running in the shell
        // (process-compose's F10 Quit, htop's F9 kill), so it falls through to
        // the PTY instead of dying as a debugger no-op. Modified chords, the
        // updater's Ctrl+Shift+F9 among them, keep their croft meaning
        // everywhere.
        let terminal_owns_fkeys = self.focus == Pane::Terminal
            && matches!(self.bottom_panel_tab, BottomPanelTab::Terminal)
            && self.debug_sessions.is_empty()
            && key.modifiers.is_empty();
        if matches!(key.code, KeyCode::F(5)) && !terminal_owns_fkeys {
            let shift = key.modifiers.contains(KeyModifiers::SHIFT);
            let cmd = key.modifiers.contains(KeyModifiers::SUPER)
                || key.modifiers.contains(KeyModifiers::CONTROL);
            if shift && cmd {
                self.debug_restart();
            } else if key.modifiers.contains(KeyModifiers::CONTROL)
                && !shift
                && !key.modifiers.contains(KeyModifiers::SUPER)
                && !key.modifiers.contains(KeyModifiers::ALT)
            {
                self.open_attach_python_picker();
            } else if shift {
                self.debug_stop_by_user();
            } else {
                self.debug_start_or_continue();
            }
            return Ok(());
        }
        // F6 pauses a running program (VS Code's Pause key) ONLY while a debug
        // session is live; with no session it falls through to its long-standing
        // role as croft's cycle-focus key.
        if matches!(key.code, KeyCode::F(6)) && !self.debug_sessions.is_empty() {
            self.debug_pause();
            return Ok(());
        }
        // Debug: Switch Session (#567), only while debugging: with no session
        // the chord is not claimed at all.
        if is_switch_debug_session_key(key) && !self.debug_sessions.is_empty() {
            self.switch_debug_session();
            return Ok(());
        }
        // Checked before the F9 family below, whose Shift arm would take it.
        if is_update_croft_key(key) {
            self.update_croft();
            return Ok(());
        }
        if matches!(key.code, KeyCode::F(9)) && !terminal_owns_fkeys {
            if key.modifiers.contains(KeyModifiers::SHIFT)
                && key.modifiers.contains(KeyModifiers::ALT)
            {
                // Shift+Alt+F9: add or edit a logpoint at the cursor. Must
                // precede the bare-Shift arm, which also matches this chord.
                self.debug_edit_logpoint();
            } else if key.modifiers.contains(KeyModifiers::SHIFT) {
                // Shift+F9: add or edit a conditional breakpoint at the cursor.
                self.debug_edit_condition();
            } else if key.modifiers.contains(KeyModifiers::ALT) {
                // Alt+F9: toggle break-on-raised-exceptions.
                self.debug_toggle_raised_exceptions();
            } else {
                // Bare F9 is Toggle Breakpoint whatever the updater is
                // doing (#865): it used to start a rebuild of croft when
                // one was on offer, and was swallowed while it ran.
                self.debug_toggle_breakpoint();
            }
            return Ok(());
        }
        if is_run_to_cursor_key(key) && !terminal_owns_fkeys {
            self.debug_run_to_cursor();
            return Ok(());
        }
        if matches!(key.code, KeyCode::F(10)) && !terminal_owns_fkeys {
            self.debug_step("next");
            return Ok(());
        }
        if matches!(key.code, KeyCode::F(11)) && !terminal_owns_fkeys {
            let cmd = if key.modifiers.contains(KeyModifiers::SHIFT) {
                "stepOut"
            } else {
                "stepIn"
            };
            self.debug_step(cmd);
            return Ok(());
        }
        // Arm the `Cmd+K` leader (VS Code's two-key chord prefix). The next
        // keystroke is interpreted by `handle_cmd_k_chord`, checked at the top
        // of this fn. Cmd is SUPER everywhere; off macOS Ctrl+K arms it too,
        // except in a focused shell and the vim-mode editor (`ctrl_k_leads`).
        if is_cmd_k_leader_key(key, self.ctrl_k_leads_here()) {
            self.cmd_k_leader = Some(std::time::Instant::now());
            return Ok(());
        }
        if is_file_finder_key(key) {
            self.open_file_finder();
            return Ok(());
        }
        if is_command_palette_key(key) {
            self.open_command_palette();
            return Ok(());
        }
        if is_go_to_symbol_key(key) {
            self.open_go_to_symbol();
            return Ok(());
        }
        // Modal layer: prompt eats every key while it's open.
        if self.prompt.is_some() {
            self.handle_prompt_key(key);
            return Ok(());
        }
        // Modal layer: local-browser confirmation eats every key.
        if self.pending_local_open.is_some() {
            self.handle_local_open_confirm_key(key);
            return Ok(());
        }
        // Modal layer: discard-changes confirmation eats every key. Y/Enter
        // discards; N/Esc cancels. Anything else is swallowed so the user
        // can't accidentally type-through into the editor below.
        if self.pending_discard.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.confirm_pending_discard();
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.cancel_pending_discard();
                }
                _ => {}
            }
            return Ok(());
        }
        // Modal layer: revert-hunk confirmation, same contract as the
        // discard modal above.
        if self.pending_revert_hunk.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.confirm_pending_revert_hunk();
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.cancel_pending_revert_hunk();
                }
                _ => {}
            }
            return Ok(());
        }
        // Modal layer: open context menu eats keyboard navigation.
        if self.context_menu.is_some() {
            self.handle_menu_key(key);
            return Ok(());
        }
        // Cmd+E toggles native modal (vim) editing. Global (above the focus
        // dispatch) so it works from any pane and even with no file open: the
        // mode is a single app-wide flag that only affects the editor, but it
        // can be pre-armed from the tree or terminal and stays on as you open
        // and switch between files.
        if is_vim_toggle_key(key) {
            self.toggle_vim_mode();
            return Ok(());
        }
        // App-wide shortcuts (priority). Skip Cmd+S when the Source
        // Control pane is focused so the SC handler can use it as the
        // stage shortcut — saving an editor file with no editor focus
        // doesn't make sense anyway, and globally swallowing Cmd+S here
        // is what made the user's first attempt feel inert.
        let sc_focused =
            self.focus == Pane::Tree && self.sidebar_view == SidebarView::SourceControl;
        if is_save_key(key, self.kitty_keys) && !sc_focused {
            self.save();
            return Ok(());
        }
        // File: New Untitled Text File (#1580). Only from the editor or the
        // side bar: a terminal's `Ctrl`+`N` is the shell's next-history, and
        // in a vim-mode editor it is vim's own.
        if is_new_untitled_file_key(key)
            && matches!(self.focus, Pane::Editor | Pane::Tree)
            && !(self.vim.enabled
                && self.focus == Pane::Editor
                && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.new_untitled_file();
            return Ok(());
        }
        if is_terminal_toggle_key(key) {
            self.toggle_terminal();
            return Ok(());
        }
        if is_terminal_maximize_key(key) {
            self.toggle_terminal_maximize();
            return Ok(());
        }
        if is_editor_split_key(key) {
            self.split_editor();
            return Ok(());
        }
        if is_focus_group_left_key(key) {
            self.focus_editor_group(true);
            return Ok(());
        }
        if is_focus_group_right_key(key) {
            self.focus_editor_group(false);
            return Ok(());
        }
        if is_terminal_split_key(key) {
            match self.split_terminal() {
                Ok(()) => {
                    self.terminal_status(format!(
                        "Split terminal: {} active",
                        self.live_terminal_count()
                    ));
                }
                Err(e) => {
                    self.status = format!("Split terminal failed: {e}");
                }
            }
            return Ok(());
        }
        if is_terminal_focus_key(key) {
            // The TERMINAL tab, not whichever panel tab was last up (#852).
            self.set_bottom_panel_tab(BottomPanelTab::Terminal);
            return Ok(());
        }
        if is_run_build_task_key(key) {
            self.run_build_task();
            return Ok(());
        }
        // Explorer-pane shortcuts get first refusal so Cmd+Shift+F creates
        // a folder when the user is browsing the tree, rather than always
        // jumping to Search. Outside Explorer the global Search-jump wins.
        if self.is_explorer_focused() && self.handle_explorer_shortcut(key) {
            return Ok(());
        }
        if is_search_jump_key(key) {
            // Jumping to global Search supersedes the in-file find: tear the
            // local find bar down first, otherwise its overlay keeps painting
            // over the editor (render_editor_find only gates on editor_find
            // being Some, not on focus) and shadows the global search input.
            self.close_editor_find();
            self.set_sidebar_view(SidebarView::Search);
            return Ok(());
        }
        // Cmd+Shift+S jumps to Source Control from any pane — croft's Source
        // Control gesture. There is no editor Save-As to collide with (Cmd+S
        // alone saves), so it fires even while the editor is focused.
        if is_source_control_jump_key(key) {
            self.set_sidebar_view(SidebarView::SourceControl);
            return Ok(());
        }
        if is_explorer_jump_key(key) {
            self.show_tree = true;
            self.set_sidebar_view(SidebarView::Explorer);
            return Ok(());
        }
        if is_run_debug_jump_key(key) {
            self.show_tree = true;
            self.set_sidebar_view(SidebarView::RunDebug);
            return Ok(());
        }
        if is_remote_jump_key(key) {
            self.show_tree = true;
            self.set_sidebar_view(SidebarView::Remote);
            return Ok(());
        }
        if is_extensions_jump_key(key) {
            self.show_tree = true;
            self.set_sidebar_view(SidebarView::Extensions);
            return Ok(());
        }
        // The bottom panel's tabs (#852), claimed from any pane like the
        // side bar jumps above.
        if is_show_problems_key(key) {
            self.set_bottom_panel_tab(BottomPanelTab::Problems);
            return Ok(());
        }
        if is_show_output_key(key) {
            self.set_bottom_panel_tab(BottomPanelTab::Output);
            return Ok(());
        }
        if is_save_all_key(key) {
            self.save_all();
            return Ok(());
        }
        // VS Code "Focus and Select Breadcrumbs" (Cmd/Ctrl+Shift+., #1608),
        // claimed from any pane; the live shell keeps its Ctrl form.
        let terminal_owns_ctrl_shift_dot = self.focus == Pane::Terminal
            && self.bottom_panel_tab == BottomPanelTab::Terminal
            && key.modifiers.contains(KeyModifiers::CONTROL);
        if is_focus_breadcrumbs_key(key) && !terminal_owns_ctrl_shift_dot {
            self.focus_breadcrumbs();
            return Ok(());
        }
        // Ctrl+Shift+H in the live shell stays its command history; Cmd+Shift+H
        // and every other pane's Ctrl+Shift+H are Replace in Files (#860).
        let terminal_owns_ctrl_shift_h = self.focus == Pane::Terminal
            && self.bottom_panel_tab == BottomPanelTab::Terminal
            && key.modifiers.contains(KeyModifiers::CONTROL);
        if is_replace_in_files_key(key) && !terminal_owns_ctrl_shift_h {
            self.open_replace_in_files();
            return Ok(());
        }
        if self.is_remote && is_drop_to_local_key(key) {
            self.guard_unsaved(UnsavedExit::DropToLocal);
            return Ok(());
        }
        if self.sidebar_view == SidebarView::Search
            && self.focus == Pane::Tree
            && is_search_editing_shortcut(key)
        {
            self.handle_search_key(key);
            return Ok(());
        }
        if is_secondary_sidebar_toggle_key(key) {
            self.toggle_secondary_side_bar();
            return Ok(());
        }
        if is_minimap_toggle_key(key) {
            self.toggle_minimap();
            return Ok(());
        }
        // Ctrl+B while the terminal pane is focused belongs to the app in
        // the shell, not to croft (#304). Claude Code backgrounds a running
        // command with it and has no other route to that; croft's sidebar
        // toggle has three (Cmd+B, the palette, the menu). The asymmetry is
        // the argument: releasing costs a chord that is reachable three
        // other ways, holding costs a chord that is reachable no other way.
        //
        // Same conditional shape as the bare F5/F9/F10/F11 rule above, and
        // for the same reason -- it is a rule about when the chord is
        // UNAMBIGUOUS, not a user preference, so it needs no setting and
        // cannot drift out of step with a config. Cmd+B is untouched
        // everywhere, which is what keeps the sidebar reachable from the
        // terminal; only the Ctrl form is released, matching iTerm2 and
        // Ghostty, which reserve Cmd and pass Ctrl through.
        //
        // Ctrl+Q follows the same rule (#1290): it is vim's visual block,
        // nano's search backwards and the shell's XON, and quitting from
        // inside one of them killed the app, the shell and every unsaved
        // tab. Quit keeps Ctrl+Q from every other pane.
        let terminal_owns_ctrl = self.focus == Pane::Terminal
            && matches!(self.bottom_panel_tab, BottomPanelTab::Terminal)
            && key.modifiers == KeyModifiers::CONTROL;
        if is_sidebar_toggle_key(key) && !terminal_owns_ctrl {
            self.toggle_side_bar();
            return Ok(());
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), KeyModifiers::CONTROL) if !terminal_owns_ctrl => {
                self.guard_unsaved(UnsavedExit::Quit);
                return Ok(());
            }
            (KeyCode::F(6), _) => {
                self.cycle_focus();
                return Ok(());
            }
            _ => {}
        }

        match self.focus {
            Pane::Tree => match self.sidebar_view {
                SidebarView::Explorer => self.handle_tree_key(key),
                SidebarView::Search => self.handle_search_key(key),
                SidebarView::SourceControl => self.handle_source_control_key(key),
                SidebarView::Remote => self.handle_remote_key(key),
                SidebarView::RunDebug => self.handle_run_debug_key(key),
                SidebarView::Extensions => self.handle_extensions_key(key),
                SidebarView::Testing => self.handle_testing_key(key),
                SidebarView::CodeQL => self.handle_codeql_key(key),
            },
            Pane::Editor => {
                // A shared file is read-only until its bootstrap snapshot
                // lands (a keystroke now would race the owner's canonical
                // text). One gate at the dispatch covers every editor edit
                // path; the window is the bootstrap round-trip.
                if let Some(session) = self.collab.as_ref()
                    && let Some(file) = self
                        .editor
                        .path
                        .as_ref()
                        .and_then(|p| collab_file_key(&self.tree.root, p))
                {
                    if session.is_bootstrapping(&file) {
                        self.status = format!("{file}: joining the shared session, one moment");
                        return Ok(());
                    }
                    // Same gate for a not-yet-attached pane of a LIVE file
                    // (a fresh split or reopen holding stale disk text): a
                    // keystroke absorbed now would be silently wiped when
                    // the attach pass seeds the pane one tick later.
                    if session.is_live(&file) && self.editor.collab_doc_gen == 0 {
                        self.status = format!("{file}: syncing this pane, one moment");
                        return Ok(());
                    }
                }
                // Settle edits that landed since the last pass first (an
                // LSP rename or quick fix applied to the file's tab): with
                // two tabs changed at once, the keystroke's tab would be
                // copied over the other and erase that edit.
                self.sync_symbol_views();
                self.handle_editor_key(key);
                // Settle the clip on this edit before clamping the caret to
                // it: a line opened at the symbol's end grows the clip, so
                // the caret on it is inside, not past it (#369).
                self.sync_symbol_views();
                // A symbol tab's caret never leaves its symbol (#369).
                self.editor.clamp_to_symbol_view();
                self.poke_cursor();
            }
            Pane::Terminal => match self.bottom_panel_tab {
                // The PROBLEMS / OUTPUT views own the panel pane while their tab
                // is active, so keystrokes scroll the list instead of leaking
                // into the hidden shell.
                BottomPanelTab::Problems => self.handle_problems_key(key),
                BottomPanelTab::Output => self.handle_output_key(key),
                BottomPanelTab::Ports => self.handle_ports_key(key),
                BottomPanelTab::Captures => self.handle_captures_key(key),
                BottomPanelTab::Terminal => self.handle_terminal_key(key),
            },
        }
        Ok(())
    }
}
