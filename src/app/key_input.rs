//! `handle_key` and `handle_key_inner`, Cmd+K chords, every `is_*_key` predicate, the
//! key and mouse contexts for user keymaps, voice input and the on-screen keyboard.

use super::*;

/// How long an armed `Cmd+K` leader waits for its second key: VS Code's
/// five seconds, after which it leaves chord mode on its own.
pub(super) const CMD_K_LEADER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The status when a voice session's reader crashed: the session ended,
/// and the next tap starts a new one (#2602).
pub(super) const VOICE_READER_CRASHED: &str =
    "Voice input stopped: its reader crashed. Tap the mic to try again";

/// How long VS Code's "is not a command" note stays in the status bar.
pub(super) const CMD_K_MISS_NOTE_FOR: std::time::Duration = std::time::Duration::from_secs(10);

/// An armed `Cmd+K` leader: when it was pressed, and how it reads in the
/// status bar while croft waits for the second key (`Ctrl+K`, `Cmd+K`).
pub(super) struct CmdKLeader {
    pub(super) at: std::time::Instant,
    pub(super) label: String,
}

/// The status bar's line while a `Cmd+K` leader waits, VS Code's words.
pub(super) fn cmd_k_waiting_text(leader: &str) -> String {
    format!("({leader}) was pressed. Waiting for second key of chord...")
}

/// VS Code's note for a second key that completes no `Cmd+K` chord.
pub(super) fn cmd_k_miss_text(leader: &str, second: &str) -> String {
    format!("The key combination ({leader}, {second}) is not a command.")
}

impl App {
    /// Leave `Cmd+K` chord mode once its wait is up, and take VS Code's "is
    /// not a command" note off the bar once it has shown its ten seconds,
    /// unless another message has replaced it since. True when either went,
    /// so the frame without it is drawn.
    pub(super) fn expire_cmd_k(&mut self, now: std::time::Instant) -> bool {
        let mut changed = false;
        if self
            .cmd_k_leader
            .as_ref()
            .is_some_and(|leader| now.saturating_duration_since(leader.at) > CMD_K_LEADER_TIMEOUT)
        {
            self.cmd_k_leader = None;
            changed = true;
        }
        if let Some((until, note)) = &self.cmd_k_miss_note
            && now >= *until
        {
            if self.status == *note {
                self.status.clear();
            }
            self.cmd_k_miss_note = None;
            changed = true;
        }
        changed
    }

    /// The status bar's line for an armed `Cmd+K` leader, if one waits.
    pub(super) fn cmd_k_waiting_line(&self) -> Option<String> {
        self.cmd_k_leader
            .as_ref()
            .map(|leader| cmd_k_waiting_text(&leader.label))
    }

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
            // Cmd+K Shift+C anywhere but the terminal's shell: Git: Show
            // Commit Actions (#871), the Commit button's dropdown with the
            // keyboard on it. Only the Terminal tab has a shell: on Output,
            // Problems, Ports or Captures the panel has focus but the chord
            // is this one. Case-insensitive for the CSI-u reason given at
            // Shift+W, and before the terminal's C/S/R arm and the plain C arm.
            KeyCode::Char(c)
                if shifted
                    && plain
                    && c.eq_ignore_ascii_case(&'c')
                    && !(self.focus == Pane::Terminal
                        && self.bottom_panel_tab == BottomPanelTab::Terminal) =>
            {
                self.open_commit_actions_menu_from_keyboard();
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
            // Cmd+K Shift+N: the user's menu (menu.json). Must precede any
            // plain N arm, which matches either case.
            KeyCode::Char(c) if shifted && plain && !cmd_held && c.eq_ignore_ascii_case(&'n') => {
                self.open_user_menu();
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
            // Cmd+K Shift+A: Git: Show Source Control Actions (#871), the
            // `⋯` menu with the keyboard on it. Matched case-insensitively
            // for the CSI-u reason given at Shift+W, and before the plain A
            // arm below.
            KeyCode::Char(c) if shifted && plain && c.eq_ignore_ascii_case(&'a') => {
                self.open_scm_actions_menu_from_keyboard();
                true
            }
            // Cmd+K A: who is attached to this multiplayer session
            // (Session: Participants).
            KeyCode::Char(c) if plain && c.eq_ignore_ascii_case(&'a') => {
                self.open_participants_picker();
                true
            }
            // Cmd+K L on a remote session: Drop to Local (#2071), its own
            // chord, so Cmd+Shift+L selects all occurrences there as it does
            // locally. Not Cmd+K Cmd+L, which carries Cmd on the second key.
            KeyCode::Char(c) if plain && !shifted && !cmd_held && c.eq_ignore_ascii_case(&'l') => {
                self.drop_to_local_if_remote();
                true
            }
            // Cmd+K Shift+X: close the other tabs of the focused group, as the
            // tab menu's Close Others and View: Close Other Editors in Group
            // do (#2072). The menu's old ⌥⌘T hint was iTerm2's New Tab and a
            // Linux desktop's new terminal, a chord croft never received.
            // Matched case-insensitively for the same CSI-u reason as Shift+W
            // above, and before the plain X arm below.
            KeyCode::Char(c) if shifted && plain && c.eq_ignore_ascii_case(&'x') => {
                self.run_command(crate::widgets::command_palette::Command::CloseOtherEditors);
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
            // Cmd+K Shift+arrow: move the active tab into the group on that
            // side, or into a new group there (View: Move Editor into Left
            // Group and its siblings, #871). VS Code binds none of the four;
            // its Cmd+K Shift+Cmd+← / → (move the tab within its group) keep
            // the Cmd, so they stay free. Before the Cmd+K → arm, which
            // ignores Shift.
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                if shifted && plain && !cmd_held =>
            {
                self.move_active_editor(match key.code {
                    KeyCode::Left => editor_layout::Dir::Left,
                    KeyCode::Right => editor_layout::Dir::Right,
                    KeyCode::Up => editor_layout::Dir::Up,
                    _ => editor_layout::Dir::Down,
                });
                true
            }
            // Cmd+K Opt+arrow: split the editor toward that side, as
            // Cmd+Opt+arrow focuses the group there (#871): Opt+↓ and Opt+←
            // are View: Split Editor Down / Left, Opt+↑ and Opt+→ the same
            // as Split Up and Split Right. Cmd may still be held from the
            // leader.
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down if !plain && !shifted => {
                match key.code {
                    KeyCode::Up => self.split_editor_dir(editor_layout::SplitDir::Vertical, false),
                    KeyCode::Down => self.split_editor_dir(editor_layout::SplitDir::Vertical, true),
                    KeyCode::Left => {
                        self.split_editor_dir(editor_layout::SplitDir::Horizontal, false)
                    }
                    _ => self.split_editor(),
                }
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
                self.reveal_active_file_in_explorer();
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
            // Cmd+K Shift+M: View: Toggle Minimap Side (#871), the strip to
            // the editor's other edge. Before the case-insensitive M arm.
            KeyCode::Char(c) if shifted && plain && !cmd_held && c.eq_ignore_ascii_case(&'m') => {
                self.toggle_minimap_side();
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
                .as_ref()
                .is_some_and(|leader| leader.at.elapsed() <= CMD_K_LEADER_TIMEOUT);
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
        // `Cmd+K`-prefixed chord, as in VS Code. A key that only goes down,
        // a modifier on its way to `Shift+X` (the kitty protocol reports
        // those), leaves the leader waiting. Any other key ends the wait:
        // it runs the chord it completes, and one that completes none, Esc
        // included, does nothing but say so. A leader older than
        // `CMD_K_LEADER_TIMEOUT` has lapsed and answers nothing.
        if let Some(leader) = self.cmd_k_leader.take()
            && leader.at.elapsed() <= CMD_K_LEADER_TIMEOUT
        {
            match screencast::key_label(key, cfg!(target_os = "macos")) {
                None => self.cmd_k_leader = Some(leader),
                Some(second) => {
                    // A character where typing is secret (a masked prompt,
                    // the shell) may be the first one of a password typed
                    // after a stray Cmd+K: neither the note nor Screencast
                    // Mode names it.
                    let secret =
                        screencast::types_a_character(key) && self.screencast_hides_typing();
                    let ran = self.handle_cmd_k_chord(key);
                    if !ran {
                        let second = if secret { "\u{2022}" } else { &second };
                        self.status = cmd_k_miss_text(&leader.label, second);
                        self.cmd_k_miss_note = Some((
                            std::time::Instant::now() + CMD_K_MISS_NOTE_FOR,
                            self.status.clone(),
                        ));
                    }
                    // Screencast Mode shows the key as the chord's second
                    // step, unless it is such a character that completed
                    // nothing.
                    if ran || !secret {
                        self.screencast.note_chord_step();
                    }
                    return Ok(());
                }
            }
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
        // Show Context Menu (#2739), checked before the F10 step-over below,
        // which took Shift+F10 too. Modified, so even a focused shell's app
        // does not get it, as in VS Code's terminal.
        if is_show_context_menu_key(key) {
            self.show_context_menu();
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
            self.cmd_k_leader = Some(CmdKLeader {
                at: std::time::Instant::now(),
                label: screencast::key_label(key, cfg!(target_os = "macos"))
                    .unwrap_or_else(|| String::from("Cmd+K")),
            });
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

impl App {
    /// A tap inside the on-screen keyboard band. Layer/modifier keys mutate
    /// the keyboard; character/named keys synthesize a `KeyEvent` and feed
    /// it through `handle_key`, so it reaches whatever currently owns the
    /// keyboard focus (editor, terminal, search, or an open modal).
    /// Begin a push-to-talk session on a mic press. Gated on availability:
    /// only Termux with `termux-speech-to-text` can listen; a missing
    /// `termux-api` package kicks off a background install and asks the user to
    /// retry, and a non-Termux host just explains it is Termux-only.
    /// Tap the mic: cancel a live session, or start a new one. The success path
    /// is not this tap but going quiet - the system speech dialog finalizes on
    /// silence and the transcript is injected then. A second tap is the escape
    /// hatch (cancel) because killing the recognizer preempts its result, so it
    /// can never be the way to "finish".
    pub(super) fn toggle_voice_input(&mut self) {
        if self.voice_handle.is_some() {
            self.cancel_voice_input();
        } else {
            self.start_voice_input();
        }
    }

    pub(super) fn start_voice_input(&mut self) {
        if self.voice_handle.is_some() {
            return;
        }
        match crate::voice::availability() {
            crate::voice::Availability::Ready => {
                self.voice_handle = Some(crate::voice::start());
                self.voice_canceling = false;
                self.status = String::from("Listening, speak then pause to insert");
            }
            crate::voice::Availability::NeedsApi => {
                crate::voice::ensure_api_installed_in_background();
                self.status = String::from(
                    "Installing Termux:API for voice input, tap the mic again once it finishes",
                );
            }
            crate::voice::Availability::NeedsTermux => {
                self.status = String::from("Voice input is available only on Termux");
            }
        }
    }

    /// Cancel the active session (a second mic tap): kill the recognizer and
    /// discard. Recognition normally ends on its own when you stop talking;
    /// this is only for abandoning a session that is stuck or unwanted. The
    /// killed worker reports `Empty`, which `drain_voice` turns into a "canceled"
    /// message via this flag; a transcript the session already sent is dropped.
    pub(super) fn cancel_voice_input(&mut self) {
        if let Some(handle) = self.voice_handle.as_ref() {
            self.voice_canceling = true;
            handle.stop();
        }
    }

    /// Per-tick: deliver a finished voice session's result. Returns whether the
    /// next frame should redraw (transcript injected or a status message set).
    ///
    /// A cancel tap can reach croft after the dialog has sent its transcript,
    /// and killing a recognizer that has exited cannot take the result back,
    /// so a transcript is typed only if no cancel is pending. The frame loop
    /// drains before it reads the input queued meanwhile, so a tap that came
    /// in before the transcript can still be unread here. The transcript
    /// therefore waits in `voice_ready` for the next drain, after that input
    /// has been read.
    pub(super) fn drain_voice(&mut self) -> bool {
        let mut changed = false;
        if let Some(text) = self.voice_ready.take() {
            changed = true;
            self.voice_handle = None;
            if std::mem::take(&mut self.voice_canceling) {
                self.status = String::from("Voice input canceled");
            } else {
                self.inject_voice_text(&text);
            }
        }
        let Some(handle) = self.voice_handle.as_mut() else {
            return changed;
        };
        let msg = match handle.poll() {
            crate::jobs::Poll::Pending => return changed,
            crate::jobs::Poll::Done(msg) => msg,
            // Its reader crashed, and the crash notice said so: the session
            // ends, so the next tap starts one instead of cancelling a dead
            // one (#2602).
            crate::jobs::Poll::Died => {
                self.voice_handle = None;
                self.voice_canceling = false;
                self.status = String::from(VOICE_READER_CRASHED);
                // This says what the crash cost: the generic notice stays away.
                claim_background_panic(crate::voice::THREAD);
                return true;
            }
        };
        match msg {
            crate::voice::VoiceMsg::Transcript(text) if !self.voice_canceling => {
                self.voice_ready = Some(text);
            }
            crate::voice::VoiceMsg::Transcript(_) => {
                changed = true;
                self.voice_handle = None;
                self.voice_canceling = false;
                self.status = String::from("Voice input canceled");
            }
            crate::voice::VoiceMsg::Empty => {
                changed = true;
                self.voice_handle = None;
                self.status = if std::mem::take(&mut self.voice_canceling) {
                    String::from("Voice input canceled")
                } else {
                    String::from("No speech detected")
                };
            }
            crate::voice::VoiceMsg::Failed(e) => {
                changed = true;
                self.voice_handle = None;
                self.voice_canceling = false;
                self.status = e;
            }
        }
        changed
    }

    /// Type a recognized transcript into whatever owns keyboard focus by
    /// replaying it as synthetic key events through `handle_key` - the same
    /// path an OSK key tap takes - so it reaches the editor, terminal, or an
    /// open modal without pane-specific wiring.
    pub(super) fn inject_voice_text(&mut self, text: &str) {
        for ch in text.chars() {
            let code = if ch == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(ch)
            };
            if let Err(e) = self.handle_key(KeyEvent::new(code, KeyModifiers::NONE)) {
                self.status = format!("Voice input failed: {e}");
                return;
            }
        }
        self.status = String::new();
    }

    pub(super) fn handle_osk_mouse(&mut self, m: MouseEvent) {
        if !matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            return;
        }
        let Some(osk) = self.osk.as_mut() else {
            return;
        };
        let Some(key) = osk.key_at(m.column, m.row) else {
            return;
        };
        if key == crate::widgets::osk::OskKey::Hide {
            self.osk = None;
            return;
        }
        let ev = osk.tap(key);
        // Layer/modifier taps reshape the band; refresh the hit rects
        // immediately so a follow-up tap that lands before the next frame
        // hits the new layer, not the stale one.
        let band = osk.last_area;
        osk.layout(band);
        // The split choice outlives the session (said when it cannot, like
        // themes).
        if key == crate::widgets::osk::OskKey::SplitToggle {
            let saved = crate::prefs::save_osk_split(osk.split);
            self.report_unsaved_setting(saved);
        }
        // Mic is a tap, not push-to-talk: Termux turns any finger hold into its
        // own text-selection gesture (hardcoded in TerminalView, not
        // suppressible from a TUI), so a hold can never reach croft as a
        // sustained press. A quick tap is the only touch Termux forwards as a
        // click. A tap starts recognition; the transcript lands when the user
        // pauses (Android finalizes on silence), and a second tap cancels.
        // Deferred until the `osk` borrow ends so the toggle can take `&mut
        // self`. `tap` returned None for it, so nothing reaches handle_key.
        let is_voice = key == crate::widgets::osk::OskKey::Voice;
        if let Some(ev) = ev
            && let Err(e) = self.handle_key(ev)
        {
            self.status = format!("On-screen key failed: {e}");
        }
        if is_voice {
            self.toggle_voice_input();
        }
    }
}

/// True when the "command" modifier is effectively held. That is `Super`
/// (Cmd) everywhere; on Termux (Android, no Cmd key) `Ctrl` stands in for it
/// too, mirroring VS Code's Linux keymap so the Super-only chords stay
/// reachable. `termux` is threaded in explicitly to keep the decision pure
/// and testable.
pub(super) fn cmd_active(mods: KeyModifiers, termux: bool) -> bool {
    mods.contains(KeyModifiers::SUPER) || (termux && mods.contains(KeyModifiers::CONTROL))
}

/// `cmd_active` over the live environment (cached Termux detection).
pub(super) fn has_cmd(mods: KeyModifiers) -> bool {
    cmd_active(mods, crate::iterm2_inline::detect_termux())
}

/// [`has_cmd`], or off macOS a `Ctrl`: the command modifier LINUX.md
/// promises there (#843), for the chords whose `Ctrl` spelling is free.
pub(super) fn has_cmd_or_linux_ctrl(mods: KeyModifiers) -> bool {
    has_cmd(mods) || (cfg!(not(target_os = "macos")) && mods.contains(KeyModifiers::CONTROL))
}

/// Returns true if the given key event should copy the terminal pane's
/// current selection to the clipboard.  Recognises:
///   * `Ctrl+Shift+C` — doesn't collide with `Ctrl+C` (SIGINT). It is also
///     the host terminal's own Copy in GNOME Terminal and Konsole, which
///     keep it, and without the kitty protocol it arrives as `Ctrl+C`; the
///     pane menu's Copy and "Copy on Selection" reach croft there (#1598).
///   * `Cmd+C`        — for terminals that deliver Super via the kitty
///     keyboard protocol.
pub(super) fn is_terminal_copy_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'c') {
        return false;
    }
    let ctrl_shift = key.modifiers.contains(KeyModifiers::CONTROL)
        && key.modifiers.contains(KeyModifiers::SHIFT);
    let super_only = key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::CONTROL);
    ctrl_shift || super_only
}

/// Returns true if the given key event should jump to the Search sidebar
/// view (VS Code's Ctrl/Cmd+Shift+F "Find in Files" gesture).
/// `Ctrl+D` / `Cmd+D` (no Shift). Used in the Explorer to drive the
/// "compare two files" flow:
///   * no anchor yet → stash the highlighted file as the anchor;
///   * anchor + different file → open a side-by-side diff;
///   * anchor + same file → clear the anchor (toggle off).
pub(super) fn is_compare_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'d') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// `Cmd+Shift+F` / `Ctrl+Shift+F`: jump to the Search view. Opt is
/// rejected: `Cmd+Opt+Shift+F` is Format Document (#2647).
pub(super) fn is_search_jump_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'f') || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    let has_shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let has_ctrl_or_super = key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER);
    has_shift && has_ctrl_or_super
}

/// VS Code-style `Cmd+F` / `Ctrl+F` inside the editor: open the inline
/// Find bar. Same chord as the Explorer's "New File" — the dispatch in
/// `handle_key` gives the Explorer handler first refusal so this only
/// fires when the editor is the focused pane.
pub(super) fn is_editor_find_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'f') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// The terminal pane's find chord: Cmd+F everywhere, plus Ctrl+F off macOS
/// only (VS Code's terminal find defaults). On macOS Ctrl+F must reach the
/// child as 0x06 — it is a control byte programs bind — so the editor's
/// looser Ctrl-or-Cmd predicate must not gate the terminal pane there.
pub(super) fn is_terminal_find_key(key: KeyEvent) -> bool {
    if !is_editor_find_key(key) {
        return false;
    }
    cfg!(not(target_os = "macos")) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// VS Code-style `Cmd+Opt+F` / `Ctrl+Alt+F` inside the editor: open the
/// find bar expanded with the replace row (VS Code's
/// `editor.action.startFindReplaceAction`). The iTerm2 / Ghostty
/// forwarders deliver the macOS chord as CSI-u decoding to ALT|SUPER;
/// Linux kitty-protocol terminals report Ctrl+Alt+F as ALT|CONTROL.
/// Shift excluded: `Cmd+Opt+Shift+F` is Format Document.
pub(super) fn is_editor_replace_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'f') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || !key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// VS Code-style `Cmd+P` / `Ctrl+P`: open the Quick Open file finder.
/// Plain `p` with no modifier (or with Shift / Alt only) falls through so
/// the editor still receives literal `p` keystrokes.
pub(super) fn is_file_finder_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'p') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// `Ctrl/Cmd+Shift+P`: open the Command Palette from any pane, mirroring VS
/// Code. Shares the `is_cmd_shift_letter` helper with the sidebar-view jumps
/// so the SHIFT / case handling is identical, and sits after
/// `is_file_finder_key` in the dispatch (which rejects SHIFT, so `Cmd+P` and
/// `Cmd+Shift+P` never collide).
pub(super) fn is_command_palette_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'p')
}

/// `Cmd+Shift+O` (macOS) / `Ctrl+Shift+O` (Linux): VS Code's "Go to Symbol in
/// Editor". Opens the symbol picker (also Go to Line via a `:` prefix).
pub(super) fn is_go_to_symbol_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'o')
}

/// `Cmd+K` (macOS) / `Ctrl+K` (Linux / Termux): the leader for croft's
/// `Cmd+K`-prefixed chords (Color Theme, Close to the Right, Select for /
/// Compare with Selected, Close All). Rejects Shift and Alt so it never
/// shadows a future `Cmd+K`+modifier chord. `ctrl_leads` is
/// [`ctrl_k_leads`] for the focused pane.
pub(super) fn is_cmd_k_leader_key(key: KeyEvent, ctrl_leads: bool) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'k') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    has_cmd(key.modifiers) || (ctrl_leads && key.modifiers.contains(KeyModifiers::CONTROL))
}

/// Whether a bare `Ctrl+K` arms the `Cmd+K` leader (#843). Off macOS it
/// does, as in VS Code's Linux keymap: Super reaches croft only over the
/// kitty keyboard protocol, so in GNOME Terminal, Konsole, xterm or tmux no
/// leader chord was reachable at all. Two places keep `Ctrl+K` for their
/// own job: a focused shell (readline's kill-line) and the editor in vim
/// mode (kill to end of line, also the palette's "Kill to End of Line").
/// On macOS `Ctrl+K` stays kill-line and Cmd leads; on Termux `Ctrl` is
/// the command key outright ([`cmd_active`]), so this is not consulted.
pub(super) fn ctrl_k_leads(macos: bool, shell_focused: bool, vim_editing: bool) -> bool {
    !macos && !shell_focused && !vim_editing
}

/// `Cmd+/` (macOS) / `Ctrl+/` (Linux / Termux): Toggle Line Comment. Rejects
/// Shift and Alt so it never collides with a future chord on `/`.
pub(super) fn is_toggle_line_comment_key(key: KeyEvent) -> bool {
    if key.code != KeyCode::Char('/') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// `Shift+Alt+A`: Toggle Block Comment (the VS Code default). Requires both
/// Shift and Alt and rejects the command modifier. Also reachable from the
/// Command Palette, which is the parity path on terminals that swallow the
/// Alt+letter chord.
pub(super) fn is_toggle_block_comment_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'a') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SHIFT)
        && key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
}

/// `Alt+O`: C/C++: Switch Header/Source (#1360), the clangd extension's
/// chord in VS Code. Rejects Shift and the other modifiers.
pub(super) fn is_switch_header_source_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    c.eq_ignore_ascii_case(&'o')
        && key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
}

/// `Alt+Z`: View: Toggle Word Wrap (the VS Code default). Rejects Shift and
/// the command modifier. Also reachable from the Command Palette.
pub(super) fn is_toggle_wrap_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'z') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Explorer-pane shortcut: `Cmd+Z` / `Ctrl+Z` — open the zoxide jump popup.
/// `z` for **z**oxide; the user's shell still uses `j` for the same jump.
/// `Ctrl+Z` counts on every platform, as the Make Root chords do (#1294):
/// xterm, GNOME Terminal, Konsole and tmux never deliver Super, and this
/// predicate only runs while the Explorer is focused, so a `Ctrl+Z` here is
/// never a shell's suspend or the editor's undo. The Explorer has no undo
/// of its own to clash with.
/// SHIFT is rejected because `Cmd+Shift+Z` is the reserved redo chord.
/// Editor-pane `Cmd+Z` is untouched: this predicate is only consulted from
/// `handle_explorer_shortcut`, which runs solely when the Explorer is
/// focused (and gets first refusal in the dispatch chain).
pub(super) fn is_tree_zoxide_jump_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'z') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Explorer-pane shortcut: `Cmd+F` / `Ctrl+F` (no Shift, no Alt) - "New File".
pub(super) fn is_tree_new_file_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'f') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// File: New Untitled Text File (#1580): `Cmd+N`, and `Ctrl+N` off macOS,
/// as in VS Code. Shift belongs to New Folder.
pub(super) fn is_new_untitled_file_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('n' | 'N'))
        && has_cmd_or_linux_ctrl(key.modifiers)
        && !key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
}

/// Explorer-pane shortcut: `Cmd+Shift+N` / `Ctrl+Shift+N` - "New Folder".
/// The global dispatcher gives the Explorer-context handler first refusal
/// so this only fires when the tree is the focused pane; outside Explorer
/// the chord is unbound and falls through to the focused pane.
pub(super) fn is_tree_new_folder_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'n') {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Explorer-pane shortcut: `Cmd+R` / `Ctrl+R` (no Shift) OR plain `F2` -
/// "Rename". F2 is the cross-IDE convention; Cmd+R matches the user-
/// requested chord (overrides iTerm's "Clear buffer" via setup-iterm2).
pub(super) fn is_tree_rename_key(key: KeyEvent) -> bool {
    if matches!(key.code, KeyCode::F(2)) {
        return true;
    }
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'r') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// `Cmd+Opt+R` / `Ctrl+Opt+R` - "Reveal in Finder". Used by both the Explorer
/// (reveals the selected entry) and the editor (reveals the active file's path).
/// The Alt requirement keeps this disjoint from `Cmd+R` (Rename), which rejects
/// Alt, so the two R chords never collide. The caller additionally gates this to
/// a local macOS session, matching the context-menu entry.
pub(super) fn is_reveal_in_finder_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'r') {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// `Cmd+Opt+C` / `Ctrl+Opt+C` - "Copy Path": put the active file's absolute
/// path on the system clipboard. The Alt requirement keeps it disjoint from
/// plain `Cmd+C` (Copy selection), and rejecting Shift keeps it disjoint from
/// `Cmd+Opt+Shift+C` (Copy Relative Path). Mirrors VS Code's binding.
pub(super) fn is_copy_path_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'c') {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Explorer-pane shortcut: `Cmd+/` / `Ctrl+/` - "Make root". Re-roots the
/// workspace at the selected folder.
pub(super) fn is_tree_make_root_key(key: KeyEvent) -> bool {
    if !matches!(key.code, KeyCode::Char('/')) {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Explorer-pane shortcut: `Cmd+Shift+/` / `Ctrl+Shift+/` - "Make root at
/// parent". Re-roots the workspace at the parent of the selected node
/// (file or folder). The selected node itself remains on disk and the
/// new root is one filesystem level up. Terminals that strip Shift on
/// shifted punctuation report `?`; we accept both `/+Shift` and `?` to
/// cover the spread.
pub(super) fn is_tree_make_parent_root_key(key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    let has_ctrl_or_super = key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER);
    if !has_ctrl_or_super {
        return false;
    }
    match key.code {
        KeyCode::Char('/') => key.modifiers.contains(KeyModifiers::SHIFT),
        KeyCode::Char('?') => true,
        _ => false,
    }
}

/// `Ctrl/Cmd+Shift+E`: jump to the Explorer sidebar view from any pane.
/// Matches VS Code's "View: Show Explorer" command.
pub(super) fn is_explorer_jump_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'e')
}

/// `Ctrl/Cmd+Shift+S`: jump to the Source Control sidebar view from any pane.
/// This is croft's Source Control gesture. Croft has no editor Save-As (Cmd+S
/// alone saves), so claiming Cmd+Shift+S is collision-free even while editing.
/// `Ctrl+Shift+S` gets here only when [`is_save_key`] let it go, which needs
/// the kitty keyboard protocol to have reported the Shift (#857).
pub(super) fn is_source_control_jump_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 's')
}

/// `Ctrl/Cmd+Shift+D`: jump to the Run and Debug sidebar view from any pane.
/// Matches VS Code's "View: Show Run and Debug" command. iTerm2 binds the
/// same chord to "Split Horizontally with Same Profile" — `setup-iterm2`
/// relocates that menu item to Cmd+Opt+Shift+D so this chord reaches croft.
pub(super) fn is_run_debug_jump_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'd')
}

/// `Ctrl/Cmd+Shift+R`: jump to the Remote (SSH) sidebar view from any pane.
/// VS Code uses this for "Remote Explorer" in the Remote Development pack —
/// same intent.
pub(super) fn is_remote_jump_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'r')
}

/// `Ctrl/Cmd+Shift+X`: jump to the Extensions sidebar view from any pane.
/// Matches VS Code's "View: Show Extensions". Cmd+Shift+X is unbound by any
/// default macOS / iTerm2 menu item, so `setup-iterm2` only needs the
/// GlobalKeyMap forwarder (no menu relocation) to deliver it here.
pub(super) fn is_extensions_jump_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'x')
}

/// Whether a caret or delete key walks by a word rather than a character:
/// Ctrl, Cmd or Alt held (#1729).
pub(super) fn key_walks_by_word(key: KeyEvent) -> bool {
    key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT)
}

/// `Ctrl/Cmd+Shift+M`: show the PROBLEMS tab from any pane, VS Code's
/// "View: Focus Problems" (#852).
pub(super) fn is_show_problems_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'm')
}

/// `Ctrl/Cmd+Shift+U`: show the OUTPUT tab from any pane, VS Code's
/// "View: Toggle Output" chord (#852).
pub(super) fn is_show_output_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'u')
}

/// `Ctrl/Cmd+Shift+H`: Search: Replace in Files, VS Code's chord (#860). In
/// the live terminal `Ctrl+Shift+H` stays `is_command_history_key`'s; the
/// dispatcher checks that before this.
pub(super) fn is_replace_in_files_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'h')
}

/// `Alt+C` / `Alt+W` / `Alt+R` / `Alt+P` while a Search side bar input has
/// focus: the `Aa` / `ab` / `.*` / `AB` toggle each flips, VS Code's
/// search-input keys (#860, #1612).
/// Only a bare Alt: a macOS terminal that sends Option+C as `ç` is typing,
/// and taking that glyph would eat it on the keyboards that have it.
pub(super) fn search_toggle_for_key(key: KeyEvent) -> Option<crate::widgets::search::SearchToggle> {
    use crate::widgets::search::SearchToggle;
    if key.modifiers != KeyModifiers::ALT {
        return None;
    }
    match key.code {
        KeyCode::Char('c' | 'C') => Some(SearchToggle::CaseSensitive),
        KeyCode::Char('w' | 'W') => Some(SearchToggle::WholeWord),
        KeyCode::Char('r' | 'R') => Some(SearchToggle::UseRegex),
        KeyCode::Char('p' | 'P') => Some(SearchToggle::PreserveCase),
        _ => None,
    }
}

/// `Alt+D` while a Search side bar input has focus: show or hide the "files
/// to include / exclude" rows (#860). VS Code's own chord, `Ctrl+Shift+J`, is
/// croft's maximize-terminal, so D for details.
pub(super) fn is_search_details_key(key: KeyEvent) -> bool {
    key.modifiers == KeyModifiers::ALT && matches!(key.code, KeyCode::Char('d' | 'D'))
}

/// `Cmd+Opt+S` / `Ctrl+Alt+S`: File: Save All (#852). VS Code's macOS chord;
/// its Linux `Ctrl+K S` is croft's Select for Compare. Shift is rejected:
/// `Cmd+Opt+Shift+S` converts indentation to spaces.
pub(super) fn is_save_all_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'s') {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// `Ctrl/Cmd+Shift+B`: run the project's auto-detected build task, VS
/// Code's "Tasks: Run Build Task" chord. iTerm2 binds the same chord to
/// "Show Toolbelt" — `setup-iterm2` relocates that menu item to
/// Cmd+Ctrl+B so this chord reaches croft.
pub(super) fn is_run_build_task_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'b')
}

/// `Cmd/Ctrl+Shift+V`: Markdown: Toggle Preview (the VS Code default chord).
/// iTerm2 binds the same chord to Paste Special > "Paste Selection" —
/// `setup-iterm2` relocates that menu item so this chord reaches croft.
pub(super) fn is_markdown_preview_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'v')
}

/// Cmd+Enter (#353): run the shell fence under the caret in a Markdown
/// source buffer. Off macOS `Ctrl`+`Enter` too (#843): there it only
/// duplicated `Enter`, and Super reaches croft only over the kitty keyboard
/// protocol. Where a terminal sends `Ctrl`+`Enter` as a bare `Enter`, the
/// palette's "Markdown: Run Code Block at Cursor" runs it.
pub(super) fn is_run_fence_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Enter)
        && has_cmd_or_linux_ctrl(key.modifiers)
        // Cmd+Shift+Enter is "insert line above"; Alt chords stay the
        // editor's.
        && !key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
}

pub(super) fn is_cmd_shift_letter(key: KeyEvent, letter: char) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&letter) {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    let has_shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let has_ctrl_or_super = key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER);
    has_shift && has_ctrl_or_super
}

/// `Cmd+B` (macOS) / `Ctrl+B` (Linux): toggle the primary side bar (left
/// pane) visibility, mirroring VS Code's "View: Toggle Primary Side Bar".
/// Accepts SUPER *or* CONTROL so the chord behaves identically whether
/// iTerm2 forwards `Cmd+B` as the CSI-u Super sequence (see `src/iterm2.rs`)
/// or a raw `Ctrl+B` control byte arrives on a remote Linux session. SHIFT
/// and ALT must not be held, so it never collides with a future bracketed
/// or word-motion chord on `b`.
pub(super) fn is_sidebar_toggle_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'b') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// `⌥⌘B` (macOS) / `Ctrl+Alt+B` (Linux): toggle the secondary side bar (the
/// Outline), the edge opposite the primary side bar. ALT distinguishes it from
/// the plain ⌘B/Ctrl+B primary-side-bar toggle.
pub(super) fn is_secondary_sidebar_toggle_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'b') {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Returns true for `Cmd+Opt+M` (the CSI-u forwarders decode it as ALT|SUPER):
/// VS Code's "View: Toggle Minimap". SHIFT must not be held.
pub(super) fn is_minimap_toggle_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'m') {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Returns true if the given key event should toggle the terminal pane.
/// VS Code uses `Ctrl+`` ` `` (backtick); we use `Ctrl+J` to match its
/// "Toggle Terminal Panel" shortcut, which is more reliable to type on
/// non-US keyboards. Case-insensitive on the letter. SHIFT must NOT be
/// held — Ctrl+Shift+J is reserved for the maximize-terminal chord and
/// must not double-fire as a toggle.
pub(super) fn is_terminal_toggle_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'j') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Ctrl+Shift+J`: toggle "maximize terminal" — the editor / welcome pane
/// collapses and the terminal fills the right column next to the Explorer.
/// Pressing it again restores the previous editor↔terminal split.
pub(super) fn is_terminal_maximize_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'j') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SUPER)
}

/// `Ctrl+Shift+Space`: WezTerm's quick-select chord. No Cmd involved, so it
/// needs no host-terminal forwarder — it arrives disambiguated through the
/// kitty keyboard protocol exactly like the Ctrl+Shift+T split chord.
pub(super) fn is_quick_select_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(' '))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Ctrl+Shift+Y` (Y for yank; WezTerm's Ctrl+Shift+X is croft's
/// Extensions jump). Arrives via the kitty keyboard protocol, so no
/// iTerm2/Ghostty forwarder is needed.
pub(super) fn is_copy_mode_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('y' | 'Y'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Ctrl+Shift+H` (H for history; atuin's Ctrl+R belongs to the shell and
/// Ctrl+Shift+R is croft's Remote jump). Kitty-protocol chord, no
/// forwarder needed.
pub(super) fn is_command_history_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('h' | 'H'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Cmd+T` (or legacy `Ctrl+Shift+T`): spawn an additional terminal next
/// to the active one. iTerm2's GlobalKeyMap forwards `Cmd+T` as
/// `ESC [ 116 ; 9 u`, which crossterm decodes to `Char('t') + SUPER`.
/// In the SUPER branch we reject SHIFT so the `Cmd+Shift+T` focus chord
/// (see `is_terminal_focus_key`) does not double-fire as a split, and
/// reject ALT so `Cmd+Opt+T` (iTerm2's relocated New Tab) is left alone.
/// Both forms reject ALT: `Ctrl+Alt+Shift+T` converts indentation to tabs
/// off macOS (#2647).
pub(super) fn is_terminal_split_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'t') || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    if has_cmd(key.modifiers) && !key.modifiers.contains(KeyModifiers::SHIFT) {
        return true;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) && key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Cmd+\`: split the editor into two side-by-side columns (VS Code's
/// `workbench.action.splitEditor`). iTerm2's GlobalKeyMap forwards it as
/// `ESC [ 92 ; 9 u`, which crossterm decodes to `Char('\\') + SUPER`.
/// SUPER-only; reject SHIFT/ALT so it can't double-fire with a future
/// modified backslash chord.
pub(super) fn is_editor_split_key(key: KeyEvent) -> bool {
    let KeyCode::Char('\\') = key.code else {
        return false;
    };
    // SUPER off Termux; on Termux `Ctrl+\` is the command surrogate (note this
    // shadows the shell's SIGQUIT there, the accepted cost of Ctrl parity).
    has_cmd(key.modifiers)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

/// `Cmd+Shift+\`: Go to Bracket (VS Code's `editor.action.jumpToBracket`). The
/// same `\` key as the split chord but with Shift, so the two are disjoint.
pub(super) fn is_goto_bracket_key(key: KeyEvent) -> bool {
    let KeyCode::Char('\\') = key.code else {
        return false;
    };
    has_cmd(key.modifiers)
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

/// `Cmd+Opt+\`: Select to Bracket (VS Code's `editor.action.selectToBracket`).
/// The `\` bracket-family key with Opt (not Shift), disjoint from the split
/// chord (`Cmd+\`) and Go to Bracket (`Cmd+Shift+\`).
pub(super) fn is_select_to_bracket_key(key: KeyEvent) -> bool {
    let KeyCode::Char('\\') = key.code else {
        return false;
    };
    has_cmd(key.modifiers)
        && key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// VS Code "Go Back" (Ctrl+-). The kitty keyboard protocol delivers the
/// chord as `-` + CONTROL, and `_` + CONTROL is accepted too. The legacy
/// encoding folds Ctrl+- into 0x1F, Ctrl+/'s byte, which croft reads as
/// Toggle Line Comment there (`legacy_ctrl_punct`, #1007).
pub(super) fn is_navigate_back_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    (c == '-' || c == '_')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// VS Code "Go Forward" (Ctrl+Shift+-). Only the explicitly SHIFT-tagged
/// spellings count: the kitty protocol reports the chord as `-` (or the
/// shifted `_`) with CONTROL|SHIFT, while the legacy 0x1F byte decodes as
/// `_` + CONTROL with no SHIFT and must stay Go Back (a legacy host cannot
/// distinguish Ctrl+- from Ctrl+Shift+-, and Back is the safer meaning).
pub(super) fn is_navigate_forward_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    (c == '-' || c == '_')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

/// `Ctrl+T`: Transpose Characters around the Cursor (VS Code's
/// `editor.action.transpose`). A raw control byte, so it needs no iTerm2 /
/// Ghostty forwarder; the editor intercepts it only when focused, leaving the
/// shell's own `Ctrl+T` intact when the terminal is focused.
pub(super) fn is_transpose_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'t') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Cmd+Opt+Shift+S`: Convert Indentation to Spaces
/// (`editor.action.indentationToSpaces`).
pub(super) fn is_indentation_to_spaces_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 's')
}

/// `Cmd+Opt+Shift+T`: Convert Indentation to Tabs
/// (`editor.action.indentationToTabs`).
pub(super) fn is_indentation_to_tabs_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 't')
}

/// `Cmd+Opt+Shift+N`: Trim Final Newlines (VS Code's `files.trimFinalNewlines`
/// surfaced as a command).
pub(super) fn is_trim_final_newlines_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 'n')
}

/// `Cmd+Opt+Shift+K`: toggle a bookmark on the cursor's line (VS Code's
/// Bookmarks extension binds `Ctrl+Alt+K`; nvim spells it `m<letter>`).
pub(super) fn is_toggle_bookmark_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 'k')
}

/// `Cmd+Opt+Shift+G`: focus the next member of a compound debug session
/// (#567), so one stopped in the background can be inspected.
pub(super) fn is_switch_debug_session_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 'g')
}

/// `Cmd+Opt+Shift+B`: clear every bookmark in the open file. `Cmd+Shift+B`
/// (Run Build Task) and `Cmd+Opt+B` (secondary side bar) each exclude the
/// other modifier, so the three chords on `b` never collide.
pub(super) fn is_clear_bookmarks_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 'b')
}

/// `Cmd+Opt+.`: jump to the next bookmark below the cursor, wrapping to the
/// top of the file. `Cmd+.` alone is Quick Fix, which excludes Alt, so the
/// two never collide.
pub(super) fn is_next_bookmark_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('.') | KeyCode::Char('>'))
        && (key.modifiers.contains(KeyModifiers::SUPER)
            || key.modifiers.contains(KeyModifiers::CONTROL))
        && key.modifiers.contains(KeyModifiers::ALT)
}

/// `Cmd+Opt+,`: mirror of [`is_next_bookmark_key`] walking upwards. The
/// shifted glyph `<` is accepted for terminals that pre-apply the shift.
pub(super) fn is_prev_bookmark_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(',') | KeyCode::Char('<'))
        && (key.modifiers.contains(KeyModifiers::SUPER)
            || key.modifiers.contains(KeyModifiers::CONTROL))
        && key.modifiers.contains(KeyModifiers::ALT)
}

/// `Cmd+Opt+Shift+J`: Join Lines (`editor.action.joinLines`).
pub(super) fn is_join_lines_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 'j')
}

/// `Cmd+Opt+Shift+U`: Transform to Uppercase (`editor.action.transformToUppercase`).
pub(super) fn is_transform_upper_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 'u')
}

/// `Cmd+Opt+Shift+L`: Transform to Lowercase (`editor.action.transformToLowercase`).
pub(super) fn is_transform_lower_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 'l')
}

/// `Cmd+Opt+Shift+C` / `Ctrl+Opt+Shift+C` - "Copy Relative Path": put the
/// active file's path, relative to the workspace root, on the clipboard. The
/// Shift distinguishes it from plain `Cmd+Opt+C` (Copy Path, absolute). Mirrors
/// VS Code's binding; Transform to Title Case keeps its Command Palette entry
/// but gives up this chord (VS Code assigns Title Case no default key).
pub(super) fn is_copy_relative_path_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 'c')
}

/// `Cmd+Opt+Shift+A`: Sort Lines Ascending (`editor.action.sortLinesAscending`).
pub(super) fn is_sort_lines_asc_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 'a')
}

/// `Cmd+Opt+Shift+D`: Sort Lines Descending (`editor.action.sortLinesDescending`).
pub(super) fn is_sort_lines_desc_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 'd')
}

/// `Cmd+Opt+Shift+W`: Trim Trailing Whitespace (`editor.action.trimTrailingWhitespace`).
pub(super) fn is_trim_trailing_whitespace_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 'w')
}

/// `Cmd+Opt+=`: Increment the number under the cursor.
///
/// vim's own chord is `Ctrl-A`, which cannot be taken here — it is Select
/// All, and on a terminal it is also the tmux/screen prefix. `Cmd+Opt` with
/// the `=`/`-` pair keeps the two halves adjacent on the keyboard and free
/// of every existing binding. `+` is accepted alongside `=` so a shifted
/// press works on layouts that report it.
pub(super) fn is_increment_number_key(key: KeyEvent) -> bool {
    is_cmd_alt_char(key, &['=', '+'])
}

/// `Cmd+Opt+-`: Decrement the number under the cursor (vim's `Ctrl-X`).
pub(super) fn is_decrement_number_key(key: KeyEvent) -> bool {
    is_cmd_alt_char(key, &['-', '_'])
}

pub(super) fn is_cmd_alt_char(key: KeyEvent, chars: &[char]) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !chars.contains(&c) {
        return false;
    }
    let has_alt = key.modifiers.contains(KeyModifiers::ALT);
    let has_cmd_or_ctrl = key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER);
    has_alt && has_cmd_or_ctrl
}

/// `Cmd+Opt+Shift+F`: Format Document (`editor.action.formatDocument`, VS Code's
/// `Shift+Alt+F`). Reformats the whole buffer through the language server.
pub(super) fn is_format_document_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 'f')
}

/// `Cmd+Opt+Shift+E`: Emmet Expand Abbreviation
/// (`editor.emmet.action.expandAbbreviation`). VS Code binds this to Tab,
/// which croft cannot reuse — Tab is indentation here, and an abbreviation
/// that only sometimes expands on Tab is worse than one that always expands
/// on its own chord.
pub(super) fn is_emmet_expand_key(key: KeyEvent) -> bool {
    is_cmd_alt_shift_letter(key, 'e')
}

/// VS Code "Focus and Select Breadcrumbs": `Cmd+Shift+.` / `Ctrl+Shift+.`
/// (#1608). A terminal that applies the shift itself sends `>`.
pub(super) fn is_focus_breadcrumbs_key(key: KeyEvent) -> bool {
    let shifted = match key.code {
        KeyCode::Char('.') => key.modifiers.contains(KeyModifiers::SHIFT),
        KeyCode::Char('>') => true,
        _ => false,
    };
    shifted
        && (key.modifiers.contains(KeyModifiers::SUPER)
            || key.modifiers.contains(KeyModifiers::CONTROL))
        && !key.modifiers.contains(KeyModifiers::ALT)
}

/// VS Code "Quick Fix": `Cmd+.` on macOS (delivered as `Char('.')` + SUPER by
/// the iTerm2 / Ghostty CSI-u forwarder) and `Ctrl+.` on Linux / Termux. Plain
/// `.`, `Shift+.` ('>'), and `Alt+.` (the shell's last-argument) are excluded.
pub(super) fn is_quick_fix_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('.'))
        && (key.modifiers.contains(KeyModifiers::SUPER)
            || key.modifiers.contains(KeyModifiers::CONTROL))
        && !key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// True for `Cmd+Opt+Shift+<letter>` (or `Ctrl+Opt+Shift+<letter>` on Termux):
/// the three-modifier sibling of [`is_cmd_shift_letter`], used for editor
/// commands VS Code leaves unbound so croft can still honor "everything has a
/// shortcut" without colliding with the two-modifier `Cmd+Shift+<letter>` set.
pub(super) fn is_cmd_alt_shift_letter(key: KeyEvent, letter: char) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&letter) {
        return false;
    }
    let has_shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let has_alt = key.modifiers.contains(KeyModifiers::ALT);
    let has_cmd_or_ctrl = key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER);
    has_shift && has_alt && has_cmd_or_ctrl
}

/// `Cmd+Opt+Left`: move keyboard focus to the LEFT editor group while
/// split. Distinct from the plain `Opt+Left` word-left motion (no SUPER)
/// by requiring both SUPER and ALT. iTerm2 forwards it as `ESC [ 1 ; 12 D`.
pub(super) fn is_focus_group_left_key(key: KeyEvent) -> bool {
    let KeyCode::Left = key.code else {
        return false;
    };
    // With Shift it is column selection (#1560), not a group switch.
    has_cmd(key.modifiers)
        && key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Cmd+Opt+Right`: move keyboard focus to the RIGHT editor group while
/// split. Mirror of `is_focus_group_left_key`. iTerm2 forwards it as
/// `ESC [ 1 ; 12 C`.
pub(super) fn is_focus_group_right_key(key: KeyEvent) -> bool {
    let KeyCode::Right = key.code else {
        return false;
    };
    // With Shift it is column selection (#1560), not a group switch.
    has_cmd(key.modifiers)
        && key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Cmd+Shift+T` (Mac SUPER+SHIFT+T): focus the Terminal pane from any
/// pane, un-hiding it if it was collapsed via Ctrl+J and bringing its
/// TERMINAL tab forward over PROBLEMS / OUTPUT / PORTS / CAPTURES (#852).
/// SUPER-only so the cross-platform `Ctrl+Shift+T` (split-terminal) chord
/// keeps its historical meaning - the two chords share the same letter but
/// are disambiguated by the modifier.
pub(super) fn is_terminal_focus_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'t') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SUPER) && key.modifiers.contains(KeyModifiers::SHIFT)
}

/// `Cmd+W` (or legacy `Ctrl+Shift+W`): close the active terminal (no-op if
/// it's the only one). Plain `Ctrl+W` is intentionally excluded so the shell's
/// delete-word-backward keeps working; the close action requires either the
/// Super (Cmd) modifier or the Ctrl+Shift pair.
pub(super) fn is_terminal_close_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'w') {
        return false;
    }
    let cmd_w =
        key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::META);
    let ctrl_shift_w = key.modifiers.contains(KeyModifiers::CONTROL)
        && key.modifiers.contains(KeyModifiers::SHIFT);
    cmd_w || ctrl_shift_w
}

/// `Cmd+]`: cycle to the next (right) terminal in the pane. iTerm2's
/// GlobalKeyMap forwards it as `ESC [ 93 ; 9 u` (modifier 9 = base +
/// Super), which crossterm decodes to `Char(']') + SUPER`. Brackets are
/// used because every arrow option is blocked: Ctrl+arrows by macOS
/// Spaces, Option+arrows by shell word-motion, Cmd+arrows by user choice.
/// Accepts the shifted glyph `}` defensively in case a terminal pre-applies
/// the shift.
pub(super) fn is_terminal_cycle_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    (c == ']' || c == '}') && has_cmd(key.modifiers)
}

/// `Cmd+[`: cycle to the previous (left) terminal in the pane. Mirror of
/// `is_terminal_cycle_key` for the reverse direction.
pub(super) fn is_terminal_cycle_back_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    (c == '[' || c == '{') && has_cmd(key.modifiers)
}

/// Returns true if the given key event should trash the currently-selected
/// node in the file tree. Recognises:
///
/// * `KeyCode::Delete` — the Forward-Delete key on full-size keyboards
///   (and `fn+Delete` on Mac laptops).
/// * `KeyCode::Backspace` — the key labeled "delete" on every Mac keyboard.
///   The tree pane has no text input that needs Backspace, so plain
///   Backspace is safe here.
/// * `Cmd+Backspace` — the macOS Finder gesture for trashing.
///
/// This helper is only called from `handle_tree_key`, so plain Backspace is
/// only swallowed when the tree pane has focus; the editor and terminal panes
/// continue to consume Backspace for their own purposes.
pub(super) fn is_delete_node_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Delete | KeyCode::Backspace)
}

impl App {
    /// The `when` context a key chord is asked about (#1436): the editor, or
    /// the file tree while the sidebar shows it, as for the mouse. `None`
    /// elsewhere (another sidebar view, the bottom panel), where only
    /// unscoped rows fire.
    pub(super) fn key_context(&self) -> Option<crate::keymap::MouseContext> {
        use crate::keymap::MouseContext;
        match self.focus {
            Pane::Editor => Some(MouseContext::Editor),
            Pane::Tree if self.sidebar_view == SidebarView::Explorer => {
                Some(MouseContext::FileTree)
            }
            _ => None,
        }
    }
}

/// Which mouse region a binding's `when` context refers to, or `None` when
/// the pointer is somewhere no context covers (a panel, a seam, the status
/// bar) — a user binding must not fire there.
pub(super) fn mouse_context_for(
    m: &MouseEvent,
    in_editor: bool,
    in_terminal: bool,
    in_tree: bool,
    app: &App,
) -> Option<crate::keymap::MouseContext> {
    use crate::keymap::MouseContext;
    // The tab strip has no focus notion, so it is recognised positionally and
    // checked first: it overlaps the editor pane's full area.
    if app.editor.tab_at(m.column, m.row).is_some() {
        return Some(MouseContext::TabStrip);
    }
    if in_terminal {
        return Some(MouseContext::Terminal);
    }
    // `in_tree` is true anywhere in the ACTIVE SIDEBAR, whichever view it is
    // showing, so it alone would make a `file_tree` binding fire over Search
    // results, the Remote list, the Testing panel and the commit graph. The
    // context is named for the file tree and documented as the file tree, so
    // it is narrowed to the view that actually is one; the other sidebar
    // views have their own row semantics and no binding context yet, and
    // firing a tree binding over them would act on a row it never meant.
    if in_tree && app.sidebar_view == SidebarView::Explorer {
        return Some(MouseContext::FileTree);
    }
    if in_editor {
        return Some(MouseContext::Editor);
    }
    None
}

/// The gesture a mouse event spells, or `None` for events that are not
/// bindable (motion, drags, button releases).
///
/// A left press reports `DoubleClick` when croft's own click tracker already
/// considers it one, so a binding agrees with the built-in select-word
/// behaviour rather than disagreeing about the same physical click. Triple
/// click is NOT reported: only the editor's tracker counts a third click,
/// for its built-in line selection (#1570), and the others distinguish
/// single from double only, so a `triple_click` binding would fire on the
/// second click there. `Gesture::parse` therefore rejects it with a load
/// warning rather than accepting a binding that could never behave.
pub(super) fn gesture_for(
    m: &MouseEvent,
    app: &App,
    ctx: crate::keymap::MouseContext,
    now: std::time::Instant,
) -> Option<crate::keymap::Gesture> {
    use crate::keymap::{Gesture, GestureKind, MouseContext};
    // Only the three modifiers a terminal actually reports for mouse; Super
    // is never present (see `Gesture::parse`).
    let mods = m.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
    let kind = match m.kind {
        MouseEventKind::Down(MouseButton::Left) if !mods.is_empty() => {
            // A modified click reads its OWN tracker. The unmodified ones are
            // shared with built-ins that ignore modifiers, so the dispatch
            // deliberately never records a modified click into them — which
            // left `ctrl+double_click` unable to fire at all, since nothing
            // remembered the first click of the pair.
            if app.modified_click.is_double(now, m.column, m.row, mods) {
                GestureKind::DoubleClick
            } else {
                GestureKind::Click
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // The tracker has to match the region: each pane records into its
            // own, so consulting `editor_click` everywhere left `double_click`
            // permanently false in the tree, tab strip, and terminal — a
            // binding there would silently never match.
            let tracker = match ctx {
                MouseContext::Terminal => &app.terminal_click,
                MouseContext::FileTree => &app.tree_click,
                MouseContext::Editor => &app.editor_click,
                // The tab strip writes to no tracker of its own, so a double
                // there can only be seen because the dispatch itself records
                // into `editor_click` above. Sharing it with the editor body
                // is acceptable: a double needs both clicks within 500ms at
                // the SAME cell, and the strip and the body do not overlap.
                MouseContext::TabStrip => &app.editor_click,
            };
            // `mods` is empty in this arm by construction (the modified case
            // is matched above and reads its own tracker), and passing it
            // rather than a literal keeps that true if the guard ever moves:
            // #317 makes the built-in tracker refuse a modified pair, which is
            // exactly what should happen if a modified click ever reached here.
            if tracker.is_double(now, m.column, m.row, mods) {
                GestureKind::DoubleClick
            } else {
                GestureKind::Click
            }
        }
        MouseEventKind::Down(MouseButton::Middle) => GestureKind::MiddleClick,
        MouseEventKind::Down(MouseButton::Right) => GestureKind::RightClick,
        MouseEventKind::ScrollUp => GestureKind::WheelUp,
        MouseEventKind::ScrollDown => GestureKind::WheelDown,
        _ => return None,
    };
    Some(Gesture { kind, mods })
}

/// Command titles in curly quotes, joined for a status line (#1482):
/// `“A”, “B” and “C”`.
pub(super) fn quoted_titles(cmds: &[crate::widgets::command_palette::Command]) -> String {
    let quoted: Vec<String> = cmds
        .iter()
        .map(|c| format!("\u{201c}{}\u{201d}", c.title()))
        .collect();
    match quoted.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => quoted.concat(),
    }
}

/// A chord eligible for a user rebind: it carries a real modifier
/// (Ctrl/Alt/Super) or is a function key. Bare keys, unmodified letters, and
/// Shift-only chords (which are just typing) are excluded so the user keymap
/// can never shadow plain text entry. Intentionally not named `is_*_key`: it is
/// a gate, not a shortcut the F1 overlay documents.
pub(super) fn is_rebindable_chord(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(_))
        || key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
}

/// Whether a key should keep an active snippet tab-stop session alive: plain
/// typing (an unmodified char), Backspace, and Tab (which advances the session).
/// Every other key ends it so Tab reverts to indenting.
pub(super) fn snippet_key_continues(key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Tab | KeyCode::Backspace => true,
        KeyCode::Char(_) => !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER),
        _ => false,
    }
}

pub(super) fn is_completion_trigger_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if c != ' ' {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// `Cmd+S` / `Ctrl+S`: save the open file. `kitty_keys` is
/// [`App::kitty_keys`]: whether `Shift` held with `Ctrl` is reported
/// explicitly.
pub(super) fn is_save_key(key: KeyEvent, kitty_keys: bool) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'s') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    let has_super = key.modifiers.contains(KeyModifiers::SUPER);
    let has_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let has_shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // Cmd+Shift+S is the Source Control jump (`is_source_control_jump_key`),
    // and so is Ctrl+Shift+S where the kitty keyboard protocol reports the
    // Shift explicitly (#857). Without it, a Shift beside Ctrl may be a
    // terminal capitalising a plain Ctrl+S, so there Ctrl+Shift+S still
    // saves; an uppercase S with Ctrl and no Shift reported saves everywhere.
    if has_shift && (has_super || (has_ctrl && kitty_keys)) {
        return false;
    }
    has_super || has_ctrl
}

/// Returns true if the given key event should copy the editor's current
/// selection to the system clipboard. Recognises plain `Ctrl+C` and `Cmd+C`
/// — there's no SIGINT collision since the editor pane is not a shell.
pub(super) fn is_editor_copy_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'c') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Cut: `Ctrl+X` / `Cmd+X`.
pub(super) fn is_editor_cut_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'x') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Paste in the search input. Normal terminal paste is handled via
/// `Event::Paste`; `setup-iterm2` maps Cmd+V to a CSI-u Cmd+V sequence, and
/// the raw Ctrl+V byte is accepted as a fallback for older setups.
pub(super) fn is_search_paste_key(key: KeyEvent) -> bool {
    is_clipboard_paste_key(key)
}

pub(super) fn is_editor_paste_key(key: KeyEvent) -> bool {
    is_clipboard_paste_key(key)
}

pub(super) fn is_clipboard_paste_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if c == '\u{16}' {
        return true;
    }
    if !c.eq_ignore_ascii_case(&'v') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Ctrl+V with no other modifier (or its raw 0x16 form), which the
/// terminal hands to the app rather than pasting (#1290).
pub(super) fn is_plain_ctrl_v(key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('\u{16}') => true,
        KeyCode::Char(c) => c.eq_ignore_ascii_case(&'v') && key.modifiers == KeyModifiers::CONTROL,
        _ => false,
    }
}

pub(super) fn is_search_editing_shortcut(key: KeyEvent) -> bool {
    is_search_paste_key(key)
        || is_editor_select_all_key(key)
        || is_editor_copy_key(key)
        || is_editor_cut_key(key)
}

/// Select-all: `Ctrl+A` / `Cmd+A`.
pub(super) fn is_editor_select_all_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'a') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Undo: `Ctrl+Z` / `Cmd+Z`. Plain Shift is rejected so it stays distinct from
/// the redo chord `Shift+Cmd+Z` / `Shift+Ctrl+Z` (see `is_editor_redo_key`).
pub(super) fn is_editor_undo_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'z') {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Redo: `Shift+Cmd+Z` / `Shift+Ctrl+Z` (VS Code's default). `Cmd+Y` is not a
/// redo alias here because it is already the `Cmd+y y` yank-line leader.
pub(super) fn is_editor_redo_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'z') {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::SUPER)
}

/// Editor-pane Rename Symbol: plain `F2` (VS Code's binding). In the Explorer
/// tree the same key renames the selected file (`is_tree_rename_key`); the
/// pane in focus decides which fires.
pub(super) fn is_rename_symbol_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(2))
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

/// `Cmd+Shift+L` (`Ctrl+Shift+L` off macOS): Select All Occurrences of Find
/// Match (#1558), VS Code's chord, the same on a remote session: Drop to
/// Local, which took it there, is `Cmd+K L` (#2071).
pub(super) fn is_select_all_occurrences_key(key: KeyEvent) -> bool {
    is_cmd_shift_letter(key, 'l')
}

/// `Shift+Alt+I`: Add Cursors to Line Ends (#1558), VS Code's chord.
pub(super) fn is_add_cursors_to_line_ends_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('i' | 'I'))
        && key.modifiers.contains(KeyModifiers::ALT)
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
}

/// `Cmd+L` (`Ctrl+L` too, as `Cmd+D` takes `Ctrl+D`): Expand Line Selection
/// (#1570), VS Code's chord. Editor-only, so a focused shell keeps `Ctrl+L`
/// to clear the screen; Shift belongs to Select All Occurrences.
pub(super) fn is_expand_line_selection_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('l' | 'L'))
        && key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
        && !key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
}

/// Editor-pane Change All Occurrences: `Cmd+F2` (macOS) or `Ctrl+F2` (Linux),
/// matching VS Code on each platform so local and remote behave identically.
/// iTerm2 may deliver Cmd folded onto the Meta/ALT bit (the same reason
/// Cmd+click arrives as ALT for go-to-definition), so any of SUPER / CONTROL /
/// ALT alongside F2 counts. Plain F2 (no modifiers) stays Rename Symbol.
pub(super) fn is_change_all_occurrences_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(2))
        && (key.modifiers.contains(KeyModifiers::SUPER)
            || key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT))
}

/// VS Code "Add Selection to Next Find Match" (`Cmd+D` on macOS, `Ctrl+D`
/// elsewhere). Neither Shift nor Alt may be held, so this never collides with
/// `Cmd+Shift+D` (Run and Debug) or `Cmd+Opt+Shift+D` (Sort Lines Descending).
pub(super) fn is_select_next_occurrence_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('d') | KeyCode::Char('D'))
        && (key.modifiers.contains(KeyModifiers::SUPER)
            || key.modifiers.contains(KeyModifiers::CONTROL))
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

/// VS Code "Delete Line" (`Cmd+Shift+K` on macOS, `Ctrl+Shift+K` elsewhere).
/// Requires Shift (to distinguish it from the `Cmd+K` chord leader) and no Alt.
pub(super) fn is_delete_line_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('k') | KeyCode::Char('K'))
        && (key.modifiers.contains(KeyModifiers::SUPER)
            || key.modifiers.contains(KeyModifiers::CONTROL))
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

/// Editor-pane Go to Definition: plain `F12` (VS Code's binding). The F12
/// navigation family splits on the modifier, matching VS Code: bare F12 is
/// Definition, `Shift+F12` is Declaration, `Ctrl+F12` is Type Definition, and
/// `Cmd+F12` (SUPER) is Implementations. Definition therefore requires F12 with
/// none of Shift / Ctrl / Super held. (Alt is still accepted: iTerm2 can fold a
/// held Cmd onto the Meta/Alt bit for some events, and Option+F12 is otherwise
/// unused, so leaving Alt on Definition is harmless.)
pub(super) fn is_go_to_definition_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(12))
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

/// Editor-pane Peek Definition: `Alt+F12`, VS Code's default (#115). This
/// claims the ALT bit Definition used to tolerate as iTerm2 Cmd-fold noise —
/// on those rare folded events the user now gets a peek popup instead of a
/// jump, strictly less disruptive than the old mis-jump.
pub(super) fn is_peek_definition_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(12))
        && key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
}

/// Editor-pane Go to References: `Shift+F12`, VS Code's real binding for this
/// action. This is the chord croft previously gave to Go to Declaration; with
/// References taking its rightful VS Code default, Declaration moved one
/// modifier over to `Ctrl+Shift+F12`. References therefore requires F12 with
/// Shift held but neither Ctrl (that is Declaration) nor Super (Implementations).
pub(super) fn is_go_to_references_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(12))
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
}

/// Show Context Menu (#2739): `Shift+F10`, or the Menu key a terminal reports
/// on the modern keyboard protocol, opens the right-click menu of what has
/// the keys, as in VS Code. Shift+F10 was Step Over, which bare F10 still is.
pub(super) fn is_show_context_menu_key(key: KeyEvent) -> bool {
    match key.code {
        KeyCode::F(10) => key.modifiers == KeyModifiers::SHIFT,
        KeyCode::Menu => !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER),
        _ => false,
    }
}

/// Debug: Run to Cursor, `Ctrl+F10` (#611), Visual Studio's binding. Checked
/// before the bare F10 step-over, which accepts any modifier.
pub(super) fn is_run_to_cursor_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(10))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// croft's own updater, `Ctrl+Shift+F9` (`Cmd+Shift+F9` too) (#865):
/// relaunch into a landed update, or rebuild a croft older than its source
/// checkout. It once shared bare F9 with Toggle Breakpoint, and won.
pub(super) fn is_update_croft_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(9))
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && (key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::SUPER))
        && !key.modifiers.contains(KeyModifiers::ALT)
}

/// Editor-pane Peek References: `Alt+Shift+F12` (#616). Alt is croft's
/// "peek" modifier on the F12 family (Alt+F12 peeks the definition), so
/// this must be checked before Go to References, which ignores Alt.
pub(super) fn is_peek_references_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(12))
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
}

/// Editor-pane Go to Declaration: `Ctrl+Shift+F12`. VS Code leaves Declaration
/// unbound, so croft assigns it within the F12 navigation family. It moved off
/// the bare `Shift+F12` it once held when Go to References (added later) claimed
/// that VS Code default, so Declaration now needs both Shift and Ctrl (Super
/// would be Implementations). iTerm2 emits `CSI 24 ; 6 ~` for Ctrl+Shift+F12,
/// which crossterm decodes to `F(12) + SHIFT + CONTROL`; `iterm2.rs` also
/// installs a defensive GlobalKeyMap forwarder emitting exactly those bytes so a
/// future iTerm2 default cannot swallow the chord.
pub(super) fn is_go_to_declaration_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(12))
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
}

/// Editor-pane Go to Type Definition: `Ctrl+F12`. VS Code leaves Type
/// Definition unbound, so croft assigns it within the F12 navigation family.
/// iTerm2 emits `CSI 24 ; 5 ~` for Ctrl+F12, which crossterm decodes to
/// `F(12) + CONTROL` (the same legacy-tilde path that delivers plain F12 and
/// Shift+F12), so no iTerm2 GlobalKeyMap forwarder is needed.
pub(super) fn is_go_to_type_definition_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(12))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// Editor-pane Go to Implementations: `Cmd+F12` (SUPER), VS Code's real macOS
/// binding for this action. Unlike its F12-family siblings, Cmd+F12 is captured
/// by macOS, so `iterm2.rs` installs a GlobalKeyMap forwarder emitting
/// `CSI 24 ; 9 ~`; crossterm decodes that to `F(12) + SUPER`. This must not
/// collide with the three siblings: bare F12 is Definition, `Shift+F12` is
/// Declaration, `Ctrl+F12` is Type Definition.
pub(super) fn is_go_to_implementation_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::F(12))
        && key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Cmd+E toggles native modal (vim) editing in the editor pane. A plain
/// Cmd+letter reaches croft reliably as SUPER (the same path as Cmd+S / Cmd+C),
/// unlike Option+letter which emits a glyph on macOS; and it stays out of the
/// Cmd+Shift+letter space reserved for activity-bar view jumps.
pub(super) fn is_vim_toggle_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('e') | KeyCode::Char('E'))
        && key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

pub(super) fn is_editor_line_home_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'a') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

pub(super) fn is_editor_line_end_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'e') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

pub(super) fn is_editor_kill_to_eol_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'k') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

pub(super) fn is_editor_kill_to_bol_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'u') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

pub(super) fn is_editor_open_line_below_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if c != 'o' {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Insert line above: `Cmd+Shift+Enter` (macOS) / `Ctrl+Shift+Enter` (Linux),
/// matching VS Code's `editor.action.insertLineBefore`. (Previously bound to
/// `Cmd+Shift+O`, which VS Code reserves for Go to Symbol; relocated to its
/// real chord so `Cmd+Shift+O` can open the symbol picker.)
pub(super) fn is_editor_open_line_above_key(key: KeyEvent) -> bool {
    if !matches!(key.code, KeyCode::Enter) {
        return false;
    }
    if !key.modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Close active tab: `Ctrl+W` / `Cmd+W`.
pub(super) fn is_close_tab_key(key: KeyEvent) -> bool {
    let KeyCode::Char(c) = key.code else {
        return false;
    };
    if !c.eq_ignore_ascii_case(&'w') {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::SUPER)
        || key.modifiers.contains(KeyModifiers::META)
}

/// Cmd+1..9 / Ctrl+1..9 — jump straight to that tab (1-based; returns the
/// 0-based index). Anything outside that range returns `None`.
pub(super) fn jump_to_tab_index(key: KeyEvent) -> Option<usize> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    if !key.modifiers.contains(KeyModifiers::SUPER)
        && !key.modifiers.contains(KeyModifiers::META)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
    {
        return None;
    }
    let d = c.to_digit(10)?;
    if !(1..=9).contains(&d) {
        return None;
    }
    Some((d - 1) as usize)
}

/// `key` as VS Code means it on a terminal without the kitty keyboard
/// protocol. There Ctrl+\, Ctrl+] and Ctrl+/ (and Ctrl+-) send the bytes
/// 0x1C, 0x1D and 0x1F, which crossterm reads as Ctrl+4, Ctrl+5 and
/// Ctrl+7, so Toggle Line Comment switched to tab 7 instead (#1007).
/// Ctrl+4, Ctrl+5 and Ctrl+7 send the same bytes, so those tabs give way:
/// the punctuation chords are the ones used every few minutes. Ctrl+6's
/// 0x1E stays tab 6, as nothing else claims Ctrl+^.
pub(super) fn legacy_ctrl_punct(key: KeyEvent) -> KeyEvent {
    if key.modifiers != KeyModifiers::CONTROL {
        return key;
    }
    let code = match key.code {
        KeyCode::Char('4') => KeyCode::Char('\\'),
        KeyCode::Char('5') => KeyCode::Char(']'),
        KeyCode::Char('7') => KeyCode::Char('/'),
        _ => return key,
    };
    KeyEvent { code, ..key }
}

/// How long a Keyboard Shortcuts prompt waits for its chord.
pub(super) const SHORTCUT_RECORD_WINDOW: std::time::Duration = std::time::Duration::from_secs(15);
