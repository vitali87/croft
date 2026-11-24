//! Typing into terminals (key encoding, broadcast, paste confirm, redaction,
//! timestamps), find, quick select, copy mode, command history and decorations,
//! CAPTURES, links and inline suggestions.

use super::*;
use crate::widgets::confirm_frame::{
    ConfirmFrame, Inset, confirm_width, draw_confirm, yes_no_keys,
};

impl App {
    /// Settings / palette "Terminal: Toggle Secret Redaction" (#360): the
    /// built-in key and token rules leave or rejoin the trigger set; the
    /// user's own `redact` rows in triggers.json are unaffected.
    pub fn toggle_secret_redaction(&mut self) {
        self.secret_redaction = !self.secret_redaction;
        self.triggers = load_trigger_set(self.secret_redaction);
        self.status = String::from(if self.secret_redaction {
            "Secret redaction: on"
        } else {
            "Secret redaction: off"
        });
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_disable_secret_redaction(
                !self.secret_redaction,
            ));
        }
    }

    /// Palette "Terminal: Reveal Redacted Secrets for 10s": every mask on
    /// every pane paints as its real text for ten seconds, then masks
    /// again. Announced so a screen share sees it happen.
    pub fn reveal_redacted_secrets(&mut self) {
        self.redaction_reveal_until =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
        self.status = String::from("Redacted secrets revealed for 10s");
    }

    /// Whether a reveal window is open right now.
    pub(super) fn redactions_revealed(&self) -> bool {
        self.redaction_reveal_until
            .is_some_and(|until| std::time::Instant::now() < until)
    }

    /// Main-loop tick: when the reveal window closes, drop it and ask for
    /// a redraw so the masks come back without waiting for other input.
    pub fn tick_redaction_reveal(&mut self) -> bool {
        if self.redaction_reveal_until.is_some() && !self.redactions_revealed() {
            self.redaction_reveal_until = None;
            self.status = String::from("Redacted secrets masked again");
            return true;
        }
        false
    }

    /// Masks on screen across every pane, as painted last frame.
    pub(super) fn redacted_on_screen(&self) -> usize {
        self.terminals.iter().map(|t| t.redacted_on_screen).sum()
    }

    /// The current index of the pane a forwarded left press went to (#474),
    /// or `None` once that pane has been closed. Looked up by uid on every
    /// event of the gesture because a close or reorder while the button is
    /// held renumbers the vector under a stored index.
    pub(super) fn forwarded_pane_index(&self, fp: ForwardedPointer) -> Option<usize> {
        self.terminals.iter().position(|t| t.uid() == fp.pane)
    }

    /// The clipboard's view of terminal text: rules marked `copy: masked`
    /// keep their mask, everything else copies as typed (#360).
    pub(super) fn terminal_text_for_copy(&self, text: String) -> String {
        crate::triggers::mask_text(&text, &self.triggers, true)
    }

    /// Palette "Terminal: Toggle Timestamps": show / hide the per-row
    /// arrival-clock gutter on every pane.
    pub fn toggle_terminal_timestamps(&mut self) {
        self.show_terminal_timestamps = !self.show_terminal_timestamps;
        for t in self.terminals.iter_mut() {
            t.show_timestamps = self.show_terminal_timestamps;
        }
        self.status = String::from(if self.show_terminal_timestamps {
            "Terminal timestamps on"
        } else {
            "Terminal timestamps off"
        });
    }

    /// Cmd+K I: toggle broadcast input. Turning it ON goes through a confirm
    /// popup (iTerm2 warns the same way); turning it OFF is immediate.
    pub(super) fn toggle_broadcast_input(&mut self) {
        if self.broadcast_input {
            self.broadcast_input = false;
            self.status = String::from("Broadcast input off");
            return;
        }
        if self.terminals.len() < 2 {
            self.status =
                String::from("Broadcast input needs at least two terminal panes (Cmd+T splits)");
            return;
        }
        self.pending_broadcast_enable = true;
    }

    pub(super) fn confirm_pending_broadcast(&mut self) {
        self.pending_broadcast_enable = false;
        self.broadcast_input = true;
        let receiving = self
            .terminals
            .iter()
            .filter(|t| !t.broadcast_excluded)
            .count();
        self.status = format!(
            "Broadcast input ON: keystrokes go to {receiving} panes (Cmd+K I to stop, Cmd+K Shift+I excludes a pane)"
        );
    }

    pub(super) fn cancel_pending_broadcast(&mut self) {
        self.pending_broadcast_enable = false;
        self.status = String::from("Broadcast input cancelled");
    }

    /// Cmd+K Shift+I: toggle whether the active pane receives broadcast
    /// input. The focused pane always gets its own typing; exclusion only
    /// mutes the mirrored copy while it is unfocused.
    pub(super) fn toggle_broadcast_exclusion(&mut self) {
        let idx = self.active_terminal;
        let Some(t) = self.terminals.get_mut(idx) else {
            return;
        };
        t.broadcast_excluded = !t.broadcast_excluded;
        let label = t.label().to_string();
        self.status = if self.terminals[idx].broadcast_excluded {
            format!("{label} excluded from broadcast input")
        } else {
            format!("{label} receives broadcast input again")
        };
    }

    /// Terminal: Run Selected Text in Active Terminal (#1292): send the
    /// editor's selection, or with none the caret's line, to the active
    /// pane, then Enter. A block goes as one paste, bracketed when the
    /// program in the pane asked for it, and Enter follows outside the
    /// brackets so it runs. Without a selection the caret steps to the next
    /// non-blank line, so repeated runs walk through a script. Focus stays
    /// in the editor.
    pub(super) fn run_selected_text_in_terminal(&mut self) {
        if !self.editor_is_text() {
            self.status = String::from("Run Selected Text works on text tabs");
            return;
        }
        let selected = self.editor.selection_text();
        let stepping = selected.is_empty();
        let text = if stepping {
            self.editor
                .lines
                .get(self.editor.cursor_row)
                .cloned()
                .unwrap_or_default()
        } else {
            selected
        };
        let text = text.trim_end_matches(['\n', '\r']);
        if stepping {
            let next = (self.editor.cursor_row + 1..self.editor.lines.len())
                .find(|&r| !self.editor.lines[r].trim().is_empty());
            if let Some(row) = next {
                self.editor.cursor_row = row;
                self.editor.cursor_col = 0;
            }
        }
        if text.trim().is_empty() {
            self.status = String::from("Nothing to run on this line");
            return;
        }
        let lines = text.lines().count();
        let text = text.to_string();
        self.show_terminal = true;
        self.bottom_panel_tab = BottomPanelTab::Terminal;
        self.reveal_terminal_pane(self.active_terminal);
        // Running the selection is the ask, so it skips the multi-line paste
        // confirm (#1274); held there, the Enter below would reach the shell
        // without the text.
        self.paste_terminal_input_now(text.as_bytes());
        self.write_terminal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        self.status = format!(
            "Ran {lines} line{} in {}",
            if lines == 1 { "" } else { "s" },
            self.terminal().label()
        );
    }

    /// Paste counterpart of [`Self::write_terminal_key`] (bracketed-paste
    /// aware per pane): normally just the active pane, mirrored to every
    /// non-excluded pane while broadcast is on (the focused pane always
    /// receives its own input).
    ///
    /// A multi-line paste a pane would run line by line asks first (#1274).
    pub(super) fn paste_terminal_input(&mut self, payload: &[u8]) {
        // A paste is input, so on a pane whose shell exited it does what a
        // typed key does (#912): close the pane, or start a new shell in the
        // last one, instead of going to a shell that is gone.
        if self
            .terminals
            .get(self.active_terminal)
            .is_some_and(|t| t.has_exited())
        {
            self.dismiss_exited_terminal();
            return;
        }
        // A paste while the popup asks about another would replace the
        // held bytes, or (one line) slip into the shell behind the popup.
        if self.pending_terminal_paste.is_some() {
            self.status = String::from("Resolve the pending paste before pasting again");
            return;
        }
        if self.multiline_paste_needs_confirm(payload) {
            self.pending_terminal_paste = Some(payload.to_vec());
            // An armed Cmd+K chord would take the popup's next key (Cmd+K I
            // turning broadcast on behind it), so the popup disarms it.
            self.cmd_k_leader = None;
            return;
        }
        self.paste_terminal_input_now(payload);
    }

    /// Whether `payload` waits on the confirm popup: it holds a line break,
    /// and per `terminal_multiline_paste_warning` either always, or (`auto`)
    /// when a pane it goes to has not turned on bracketed paste, where every
    /// line break is an Enter (sh/dash, an old Python REPL, psql).
    pub(super) fn multiline_paste_needs_confirm(&self, payload: &[u8]) -> bool {
        use crate::prefs::MultilinePasteWarning;
        if !payload.iter().any(|&b| b == b'\n' || b == b'\r') {
            return false;
        }
        match self.multiline_paste_warning {
            MultilinePasteWarning::Never => false,
            MultilinePasteWarning::Always => true,
            MultilinePasteWarning::Auto => {
                let active = self.active_terminal;
                self.terminals.iter().enumerate().any(|(i, t)| {
                    let receives = i == active || (self.broadcast_input && !t.broadcast_excluded);
                    receives && !t.bracketed_paste()
                })
            }
        }
    }

    /// The confirm popup's Paste (`one_line` false) or Paste as one line.
    pub(super) fn confirm_pending_terminal_paste(&mut self, one_line: bool) {
        let Some(payload) = self.pending_terminal_paste.take() else {
            return;
        };
        let lines = pasted_lines(&payload);
        self.status = if one_line {
            format!("Pasted {} lines as one", lines.len())
        } else {
            format!("Pasted {} lines", lines.len())
        };
        let payload = if one_line { lines.join(&b' ') } else { payload };
        self.paste_terminal_input_now(&payload);
    }

    pub(super) fn cancel_pending_terminal_paste(&mut self) {
        self.pending_terminal_paste = None;
        self.status = String::from("Paste cancelled");
    }

    pub(super) fn paste_terminal_input_now(&mut self, payload: &[u8]) {
        if self.broadcast_input {
            let active = self.active_terminal;
            for (i, t) in self.terminals.iter_mut().enumerate() {
                if i == active || !t.broadcast_excluded {
                    t.paste_input(payload);
                }
            }
        } else {
            self.terminal_mut().paste_input(payload);
        }
    }

    /// Confirmation modal for enabling broadcast input: the classic footgun
    /// is typing a destructive command into panes you forgot were listening,
    /// so switching it on gets the same red Y/N gate iTerm2 shows.
    pub(super) fn render_broadcast_confirm(&self, frame: &mut ratatui::Frame) {
        if !self.pending_broadcast_enable {
            return;
        }
        let frame_spec = ConfirmFrame {
            title: " BROADCAST INPUT TO ALL PANES? ",
            accent: self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70)),
            title_fg: Color::White,
            height: 7,
            inset: Inset::Wide,
        };
        draw_confirm(frame, self.theme, frame_spec, |frame, inner| {
            let receiving = self
                .terminals
                .iter()
                .filter(|t| !t.broadcast_excluded)
                .count()
                .max(1);
            let body = ratatui::text::Text::from(vec![
                ratatui::text::Line::from(ratatui::text::Span::styled(
                    format!(
                        "Every keystroke and paste will go to {receiving} terminal panes at once (red \u{21f6} pills mark them). Cmd+K I stops it; Cmd+K Shift+I excludes a pane."
                    ),
                    Style::default().fg(self.theme.ui(Color::White)),
                )),
                ratatui::text::Line::from(""),
                yes_no_keys("es, broadcast"),
            ]);
            frame.render_widget(
                ratatui::widgets::Paragraph::new(body).wrap(ratatui::widgets::Wrap { trim: true }),
                inner,
            );
        });
    }

    /// The confirm popup for a multi-line terminal paste (#1274): how many
    /// lines would run, the first few of them, and the three choices.
    pub(super) fn render_terminal_paste_confirm(&self, frame: &mut ratatui::Frame) {
        use ratatui::text::{Line, Span};
        let Some(payload) = self.pending_terminal_paste.as_ref() else {
            return;
        };
        let lines = pasted_lines(payload);
        const PREVIEW: usize = 3;
        let preview = lines.len().min(PREVIEW);
        let more = lines.len() > PREVIEW;
        let n = lines.len();
        let title = format!(
            " PASTE {n} LINE{} INTO THE TERMINAL? ",
            if n == 1 { "" } else { "S" }
        );
        let frame_spec = ConfirmFrame {
            title: &title,
            accent: self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70)),
            title_fg: Color::White,
            height: 7 + preview as u16 + u16::from(more),
            inset: Inset::Wide,
        };
        draw_confirm(frame, self.theme, frame_spec, |frame, inner| {
            let white = Style::default().fg(self.theme.ui(Color::White));
            let dim = Style::default().fg(self.theme.ui(Color::Rgb(0x8b, 0x93, 0xa5)));
            let mut body = vec![
                Line::from(Span::styled(
                    "Each line break is an Enter here, so the lines run as they land:",
                    white,
                )),
                Line::from(""),
            ];
            for line in &lines[..preview] {
                body.push(Line::from(Span::styled(
                    format!("  {}", String::from_utf8_lossy(line)),
                    dim,
                )));
            }
            if more {
                body.push(Line::from(Span::styled(
                    format!("  … and {} more", lines.len() - PREVIEW),
                    dim,
                )));
            }
            let key = |k: &'static str, color: Color| {
                Span::styled(k, Style::default().fg(color).add_modifier(Modifier::BOLD))
            };
            body.push(Line::from(""));
            body.push(Line::from(vec![
                key("[Y]", Color::Red),
                Span::raw(" / Enter paste   "),
                key("[L]", Color::Yellow),
                Span::raw(" paste as one line   "),
                key("[N]", Color::Green),
                Span::raw(" / Esc cancel"),
            ]));
            frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
        });
    }

    /// The confirm popup for a runnable fence (#353): the whole block, the
    /// pane and directory it would run in, red when it looks destructive.
    pub(super) fn render_run_block_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(block) = self.pending_run_block.as_ref() else {
            return;
        };
        let area = frame.area();
        let inner_w = confirm_width(area).saturating_sub(4) as usize;
        let shown: Vec<String> = block
            .code
            .lines()
            .take(8)
            // A line wider than the popup is cut with a visible mark: what
            // the popup hides must never look complete.
            .map(|l| {
                if l.chars().count() > inner_w {
                    let mut cut: String = l.chars().take(inner_w.saturating_sub(1)).collect();
                    cut.push('\u{2026}');
                    cut
                } else {
                    l.to_string()
                }
            })
            .collect();
        let more = block.code.lines().count().saturating_sub(shown.len());
        let (title, accent) = if block.destructive {
            (
                " RUN THIS BLOCK? IT LOOKS DESTRUCTIVE ",
                self.theme.ui(Color::Rgb(0xe7, 0x70, 0x70)),
            )
        } else {
            (" RUN THIS BLOCK? ", self.theme.accent())
        };
        let frame_spec = ConfirmFrame {
            title,
            accent,
            title_fg: Color::White,
            height: (shown.len() as u16 + 6 + u16::from(more > 0))
                .min(area.height.saturating_sub(2)),
            inset: Inset::Wide,
        };
        draw_confirm(frame, self.theme, frame_spec, |frame, inner| {
            let mut lines: Vec<ratatui::text::Line> =
                vec![ratatui::text::Line::from(ratatui::text::Span::styled(
                    format!("in pane {} at {}", block.pane_name, block.cwd.display()),
                    Style::default().fg(self.theme.ui(Color::Rgb(0xCC, 0xCC, 0xCC))),
                ))];
            for l in &shown {
                lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                    l.clone(),
                    Style::default().fg(self.theme.ui(Color::White)),
                )));
            }
            if more > 0 {
                lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                    format!("… {more} more line(s)"),
                    Style::default().fg(self.theme.ui(Color::Rgb(0x88, 0x88, 0x88))),
                )));
            }
            lines.push(ratatui::text::Line::from(""));
            lines.push(yes_no_keys("es, run"));
            frame.render_widget(
                ratatui::widgets::Paragraph::new(ratatui::text::Text::from(lines)),
                inner,
            );
        });
    }

    pub(super) fn handle_captures_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.captures.move_selection(-1),
            KeyCode::Down => self.captures.move_selection(1),
            KeyCode::Esc => self.focus_pane(Pane::Editor),
            KeyCode::Enter => self.captures_open_selected(),
            KeyCode::Char('n') => self.ask_navigator_about_capture(),
            KeyCode::Char('x') => self.captures.remove_selected(),
            KeyCode::Char('c') => {
                self.captures.clear();
                self.status = String::from("Cleared captured output");
            }
            _ => {}
        }
    }

    /// Ctrl+Shift+H: open the durable command-history search over the
    /// active pane's context (atuin's enhanced Ctrl+R, embedded).
    pub(super) fn open_command_history(&mut self) {
        let mut pop = crate::widgets::history_popup::HistoryPopup::new();
        pop.cwd = self
            .terminal()
            .shell_cwd()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        pop.host = self.terminal().shell_host().unwrap_or_default();
        pop.set_results(
            self.command_history
                .search("", pop.scope, &pop.cwd, &pop.host),
        );
        self.command_history_popup = Some(pop);
    }

    /// Re-run the store search after any query / scope change.
    pub(super) fn refresh_command_history_results(&mut self) {
        let Some(pop) = self.command_history_popup.as_ref() else {
            return;
        };
        let (query, scope, cwd, host) = (
            pop.query.clone(),
            pop.scope,
            pop.cwd.clone(),
            pop.host.clone(),
        );
        let results = self.command_history.search(&query, scope, &cwd, &host);
        if let Some(pop) = self.command_history_popup.as_mut() {
            pop.set_results(results);
        }
    }

    pub(super) fn handle_command_history_key(&mut self, key: KeyEvent) {
        // Ctrl+R (and the opening chord itself) cycles the scope filter:
        // all → this directory → failed only.
        if matches!(key.code, KeyCode::Char('r' | 'R'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            if let Some(pop) = self.command_history_popup.as_mut() {
                pop.scope = pop.scope.next();
            }
            self.refresh_command_history_results();
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.command_history_popup = None;
            }
            KeyCode::Enter => {
                let cmd = self
                    .command_history_popup
                    .as_ref()
                    .and_then(|p| p.selected_entry())
                    .map(|e| e.cmd.clone());
                self.command_history_popup = None;
                if let Some(cmd) = cmd {
                    self.focus_pane(Pane::Terminal);
                    self.terminal_mut().paste_input(cmd.as_bytes());
                    self.status = String::from("Command typed at the prompt (Enter runs it)");
                }
            }
            KeyCode::Up => {
                if let Some(pop) = self.command_history_popup.as_mut() {
                    pop.select_prev();
                }
            }
            KeyCode::Down => {
                if let Some(pop) = self.command_history_popup.as_mut() {
                    pop.select_next();
                }
            }
            KeyCode::Left => {
                if let Some(pop) = self.command_history_popup.as_mut() {
                    pop.move_cursor_left();
                }
            }
            KeyCode::Right => {
                if let Some(pop) = self.command_history_popup.as_mut() {
                    pop.move_cursor_right();
                }
            }
            KeyCode::Backspace => {
                if let Some(pop) = self.command_history_popup.as_mut() {
                    pop.pop_char();
                }
                self.refresh_command_history_results();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(pop) = self.command_history_popup.as_mut() {
                    pop.push_char(c);
                }
                self.refresh_command_history_results();
            }
            _ => {}
        }
    }

    /// Jump the selected capture's pane back to the captured line: switch to
    /// the TERMINAL tab, focus the pane (found by shell pid; a closed pane
    /// falls back to the active one), locate the grid row matching the
    /// captured text nearest the row the line was printed on (so each run's
    /// copy of a repeated line is its own entry, #1450), scroll it into view
    /// and select it. A very long captured line wraps across grid rows, so
    /// the match is prefix-based on the first wrapped row.
    pub fn captures_open_selected(&mut self) {
        let Some(entry) = self.captures.selected_entry().cloned() else {
            return;
        };
        let (idx, anchor) = self.capture_pane(&entry);
        self.reveal_terminal_pane(idx);
        self.set_bottom_panel_tab(BottomPanelTab::Terminal);
        self.focus_pane(Pane::Terminal);
        let needle: String = entry.line.trim_end().chars().take(60).collect();
        let Some(line) = self.terminals[idx].find_captured_line(&needle, anchor) else {
            self.status = String::from("Captured line is no longer in the scrollback");
            return;
        };
        let (_, _, cols, _) = self.terminals[idx].grid_bounds();
        let t = &mut self.terminals[idx];
        t.set_selection(Some(crate::widgets::terminal::Selection {
            anchor: (line, 0),
            head: (line, cols.saturating_sub(1)),
            block: false,
        }));
        t.scroll_to_line(line);
        self.status = format!("Capture: {}", entry.message);
    }

    /// The pane a capture came from, found by shell pid, and the anchor to
    /// find its line by. A closed pane falls back to the active one without
    /// the anchor, which names a row only in the pane that recorded it: the
    /// newest match is all another pane can offer.
    pub(super) fn capture_pane(
        &self,
        entry: &crate::widgets::captures::CapturedLine,
    ) -> (usize, Option<u64>) {
        let found = entry.shell_pid.and_then(|pid| {
            self.terminals
                .iter()
                .position(|t| t.shell_pid() == Some(pid))
        });
        match found {
            Some(idx) => (idx, entry.anchor),
            None => (self.active_terminal, None),
        }
    }

    pub(super) fn handle_terminal_key(&mut self, key: KeyEvent) {
        // Quick-select owns every keystroke while its labels are up.
        if self.terminal_quick_select.is_some() {
            self.handle_terminal_quick_select_key(key);
            return;
        }
        // Copy mode owns every keystroke while it is on.
        if self.terminal_copy_mode.is_some() {
            self.handle_terminal_copy_mode_key(key);
            return;
        }
        // The find bar owns every keystroke while it is open.
        if self.terminal_find.is_some() {
            self.handle_terminal_find_key(key);
            return;
        }
        // Ctrl+Shift+Space (WezTerm's quick-select): label every URL / path /
        // hash / IP on screen; typing a label copies it, UPPERCASE pastes.
        if is_quick_select_key(key) {
            self.open_terminal_quick_select();
            return;
        }
        // Ctrl+Shift+Y (WezTerm's copy mode, Y for yank): keyboard-select
        // from the grid and scrollback with vi motions.
        if is_copy_mode_key(key) {
            self.open_terminal_copy_mode();
            return;
        }
        // Ctrl+Shift+H (H for history; atuin's enhanced Ctrl+R lives on the
        // shell side): search the durable cross-session command history.
        // Plain Ctrl+R still reaches the shell's own reverse search.
        if is_command_history_key(key) {
            self.open_command_history();
            return;
        }
        // Open find-in-terminal (scrollback search). Cmd+F everywhere;
        // Ctrl+F only off macOS, matching VS Code's terminal find defaults:
        // on macOS Ctrl+F is a control byte the child owns (vim page-forward,
        // emacs forward-char) and iTerm2/VS Code pass it through, while on
        // Linux/Windows Ctrl+F opening find IS the VS Code default.
        if is_terminal_find_key(key) {
            self.open_terminal_find();
            return;
        }
        // Ctrl+Shift+T: open another terminal next to the active one.
        if is_terminal_split_key(key) {
            self.split_terminal_reporting();
            return;
        }
        // Cmd+W (or legacy Ctrl+Shift+W): close the active terminal (no-op when
        // only one is left).
        if is_terminal_close_key(key) {
            self.close_terminal_reporting(self.active_terminal);
            return;
        }
        // Cmd+]: next terminal. Cmd+[: previous terminal.
        if is_terminal_cycle_key(key) {
            self.cycle_terminal();
            return;
        }
        if is_terminal_cycle_back_key(key) {
            self.cycle_terminal_back();
            return;
        }
        // Cmd+Opt+Up / Cmd+Opt+Down (Linux: Ctrl+Alt): jump to the previous /
        // next prompt via the OSC 133 marks shell integration records —
        // VS Code's command navigation. (VS Code's own Cmd+Up/Down chord is
        // impossible here: bare Cmd+arrows are reserved by the user, see
        // src/iterm2.rs. The chord is already forwarded globally for the
        // editor's Add Cursor Above/Below and is unused in the terminal.)
        if key
            .modifiers
            .intersects(KeyModifiers::SUPER | KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
        {
            self.terminal_prompt_jump(matches!(key.code, KeyCode::Down));
            return;
        }
        // Shift+PageUp / Shift+PageDown: page through the scrollback;
        // Shift+Home / Shift+End jump to the oldest line / the live bottom
        // (xterm's keyboard-scrollback convention, leaving the plain keys
        // for the running program). In the alternate screen there is no
        // scrollback, so the chord falls through to the program instead.
        if key.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(
                key.code,
                KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
            )
        {
            let page = self.terminal().last_inner.height.max(2) as usize - 1;
            let scrolled = match key.code {
                KeyCode::PageUp => self.terminal_mut().scroll_up(page),
                KeyCode::PageDown => self.terminal_mut().scroll_down(page),
                KeyCode::Home => self.terminal_mut().scroll_to_top(),
                _ => self.terminal_mut().reset_scrollback(),
            };
            if scrolled {
                return;
            }
        }
        // Ctrl+Shift+C / Cmd+C: copy current selection.
        if is_terminal_copy_key(key) {
            self.copy_terminal_selection();
            return;
        }
        // Cmd+V / Ctrl+Shift+V: paste local clipboard into the embedded
        // shell; Cmd+V, when not eaten by the host terminal's menu
        // shortcut, would otherwise do nothing useful. Plain Ctrl+V is the
        // app's (#1290): vim's visual block and nano's page down need the
        // 0x16 byte, as in VS Code's terminal on Linux.
        if is_clipboard_paste_key(key) && !is_plain_ctrl_v(key) {
            self.paste_clipboard_into_terminal();
            return;
        }
        // A pane whose shell exited takes no input (#912): a key that would
        // type closes it instead, or starts a new shell in the last pane.
        if self.terminal().has_exited() {
            if !key_to_bytes(key, false).is_empty() {
                self.dismiss_exited_terminal();
            }
            return;
        }
        // Any other keystroke clears the selection so the user's input is
        // sent without the previous highlight lingering on screen.
        if self.terminal().selection().is_some() {
            self.terminal_mut().clear_selection();
        }
        // Right arrow at the end of the typed line accepts the suggestion
        // painted after it (#614), as in fish; anywhere else it moves.
        if key.code == KeyCode::Right
            && key.modifiers.is_empty()
            && let Some((pane, typed, rest)) = self.term_suggestion.take()
            && pane == self.active_terminal
            && !self.terminal().awaiting_echo()
            && self.terminal().prompt_tail().is_some_and(|t| t.0 == typed)
        {
            self.terminal_mut().write_input(rest.as_bytes());
            return;
        }
        self.write_terminal_key(key);
    }

    /// Shell-bound keystroke from the focused terminal: normally just the
    /// active pane, mirrored to every non-excluded pane while broadcast is
    /// on. Encoded per pane because DECCKM is per-PTY state: every pane gets
    /// the form its own child asked for (a zsh pane wants SS3 arrows while a
    /// bash pane wants CSI), not the focused pane's.
    pub(super) fn write_terminal_key(&mut self, key: KeyEvent) {
        if self.broadcast_input {
            let active = self.active_terminal;
            for (i, t) in self.terminals.iter_mut().enumerate() {
                if i == active || !t.broadcast_excluded {
                    let bytes = key_to_bytes(key, t.app_cursor_keys());
                    if !bytes.is_empty() {
                        t.write_input(&bytes);
                    }
                }
            }
        } else {
            let bytes = key_to_bytes(key, self.terminal().app_cursor_keys());
            if !bytes.is_empty() {
                self.terminal_mut().write_input(&bytes);
            }
        }
    }

    /// Service edge auto-scroll while a terminal drag-selection is held
    /// past the top/bottom edge. Called every main-loop wakeup; advances
    /// the viewport by one row at ~30 ms intervals (a readable ~33 rows/s)
    /// and re-pins the selection head to the new edge. Returns true when a
    /// scroll happened so the caller knows it owes a redraw.
    pub fn tick_terminal_autoscroll(&mut self) -> bool {
        let Some(state) = self.terminal_select_autoscroll else {
            return false;
        };
        if state.last.elapsed() < std::time::Duration::from_millis(30) {
            return false;
        }
        // A forwarded click-only drag (#474) may be on a pane other than the
        // active one: Cmd+] moves the active pane mid-drag, and no further
        // Drag event arrives while the pointer rests past the edge to
        // re-check. This tick scrolls the ACTIVE pane, so it stands down
        // rather than walk the wrong pane's scrollback -- HELD, not dropped:
        // the parked pointer sends nothing to re-arm, so cycling back to the
        // origin pane must resume the scroll on its own (and the untouched
        // `last` stamp makes that first tick fire at once).
        if let Some(fp) = self.terminal_pointer_forwarded {
            let Some(idx) = self.forwarded_pane_index(fp) else {
                // The origin pane is gone (closed by its button while the
                // pointer was parked): nothing can ever resume this, so
                // drop the gesture rather than hold it to the release.
                self.terminal_pointer_forwarded = None;
                self.terminal_select_autoscroll = None;
                return false;
            };
            if idx != self.active_terminal {
                return false;
            }
        }
        self.terminal_mut().autoscroll_select(state.dir, state.col);
        self.terminal_select_autoscroll = Some(TerminalSelectAutoScroll {
            last: std::time::Instant::now(),
            ..state
        });
        true
    }

    /// Cmd+Opt+Up / Cmd+Opt+Down in the terminal: scroll the previous / next
    /// OSC 133 prompt mark to the top of the pane.
    pub(super) fn terminal_prompt_jump(&mut self, forward: bool) {
        let prompts = self.terminal().prompt_lines();
        let top = self.terminal().viewport_top_line();
        let Some(target) = crate::widgets::terminal::pick_prompt_jump(&prompts, top, forward)
        else {
            self.status = String::from(if prompts.is_empty() {
                "No command marks yet (shell integration needs a zsh prompt)"
            } else if forward {
                "No next command"
            } else {
                "No previous command"
            });
            return;
        };
        self.terminal_mut().scroll_line_to_top(target);
    }

    /// The most recent finished command in the active pane, for the
    /// `⌘K ⇧C`/`⇧S`/`⇧R` keyboard forms of the decoration-menu actions.
    pub(super) fn last_command_decoration(
        &self,
    ) -> Option<crate::widgets::terminal::CommandDecoration> {
        self.terminal().command_decorations().last().copied()
    }

    /// The command decoration dot under the pointer, when the panel shows
    /// the terminal. Used both to open the decoration menu and to exempt
    /// dot cells from the resize-seam grab zones they can overlap.
    pub(super) fn decoration_dot_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<(usize, crate::widgets::terminal::CommandDecoration)> {
        if !self.show_terminal || self.bottom_panel_tab != BottomPanelTab::Terminal {
            return None;
        }
        let idx = self.terminal_at_pos(col, row)?;
        self.terminals[idx]
            .decoration_at_screen(col, row)
            .map(|d| (idx, d))
    }

    pub(super) fn copy_command_output(
        &mut self,
        idx: usize,
        d: crate::widgets::terminal::CommandDecoration,
    ) {
        let Some(t) = self.terminals.get(idx) else {
            return;
        };
        // Same clipboard rule as a selection copy (#360): rules marked
        // copy=masked keep their mask.
        let text = self.terminal_text_for_copy(t.command_output_text(&d));
        if text.is_empty() {
            self.status = String::from("Command produced no output");
            return;
        }
        let lines = text.lines().count();
        let what = if lines == 1 {
            String::from("command output (1 line)")
        } else {
            format!("command output ({lines} lines)")
        };
        self.status = self.copy_where_pasted(&text).status(&what);
    }

    pub(super) fn select_command_output(
        &mut self,
        idx: usize,
        d: crate::widgets::terminal::CommandDecoration,
    ) {
        let Some(t) = self.terminals.get_mut(idx) else {
            return;
        };
        t.select_command_output(&d);
        self.status = String::from(if t.selection().is_some_and(|s| s.has_area()) {
            "Command output selected"
        } else {
            "Command produced no output"
        });
    }

    pub(super) fn rerun_command(
        &mut self,
        idx: usize,
        d: crate::widgets::terminal::CommandDecoration,
    ) {
        let Some(t) = self.terminals.get(idx) else {
            return;
        };
        let cmd = t.command_input_text(&d);
        if cmd.is_empty() {
            self.status = String::from("No command text recorded at that prompt");
            return;
        }
        self.terminals[idx].write_input(format!("{cmd}\r").as_bytes());
        self.status = format!("Re-running: {cmd}");
    }

    /// The context menu a click on a command decoration dot opens: a header
    /// with the outcome, then the VS Code decoration actions.
    pub(super) fn open_command_decoration_menu(
        &mut self,
        origin: (u16, u16),
        d: crate::widgets::terminal::CommandDecoration,
    ) {
        let idx = self.active_terminal;
        let code = d.exit.map_or_else(|| String::from("?"), |c| c.to_string());
        let header = match d.duration {
            Some(dur) => format!(
                "exit {code} in {}",
                crate::widgets::terminal::human_duration(dur)
            ),
            None => format!("exit {code}"),
        };
        self.context_menu = Some(ContextMenu {
            crumb: None,
            scroll: Default::default(),
            submenu_scroll: Default::default(),
            origin,
            items: vec![
                MenuEntry::header(header),
                MenuEntry::Item {
                    label: String::from("Copy Output"),
                    action: MenuAction::CopyCommandOutput(idx, d),
                },
                MenuEntry::Item {
                    label: String::from("Select Output"),
                    action: MenuAction::SelectCommandOutput(idx, d),
                },
                MenuEntry::Item {
                    label: String::from("Re-run Command"),
                    action: MenuAction::RerunCommand(idx, d),
                },
            ],
            selected: 1,
            open_submenu: None,
            submenu_selected: 0,
            target_dir: self.tree.root.clone(),
        });
    }

    /// Cmd/Ctrl+click in the active terminal: if a printed http(s) URL sits
    /// under the cursor, act on it (forward a remote dev-server port and open,
    /// or open the link) and return true so the click doesn't start a
    /// selection. Returns false when there's no URL there.
    /// Whether the built-in Ctrl/Cmd+click would open something at this cell.
    ///
    /// Pure: the lookups [`Self::terminal_url_click`] and
    /// [`Self::terminal_file_click`] perform, with the opening left out. The
    /// swallow branch needs to know whether declining a prefix click COSTS the
    /// user anything, and it cannot find out by calling the acting versions.
    ///
    /// BOTH arms of the built-in's `||` are covered deliberately: checking
    /// only the URL half would let a Ctrl+click on a file reference be
    /// swallowed and its jump-to-file lost, which is the same defect one
    /// branch over.
    ///
    /// Deliberately conservative on the file half. `terminal_file_click`
    /// resolves relative paths against the shell's cwd and touches the
    /// filesystem; reproducing that here would duplicate logic that then
    /// drifts. A file REFERENCE under the cursor is enough to defer to the
    /// built-in — at worst the prefix click is not swallowed, which costs a
    /// stray built-in action rather than a lost one.
    ///
    /// Takes the CLICKED pane's index rather than reading `self.terminal()`.
    /// The built-in activates the clicked pane before it looks for a link, so
    /// a predicate reading the ACTIVE pane answers about different content
    /// whenever the two differ — which is any split layout where the click
    /// lands outside the focused pane.
    pub(super) fn terminal_would_open_something_at(&self, idx: usize, col: u16, row: u16) -> bool {
        let Some(t) = self.terminals.get(idx) else {
            return false;
        };
        if t.hyperlink_at_screen(col, row).is_some() {
            return true;
        }
        t.line_text_at(col, row).is_some_and(|(text, c)| {
            crate::port_detect::url_at(&text, c).is_some()
                || crate::file_ref::file_ref_at(&text, c).is_some()
        })
    }

    /// Cmd/Ctrl+click in the active terminal: if a printed http(s) URL sits
    /// under the cursor, act on it (forward a remote dev-server port and open,
    /// or open the link) and return true so the click doesn't start a
    /// selection. Returns false when there's no URL there.
    pub(super) fn terminal_url_click(&mut self, col: u16, row: u16) -> bool {
        // A real OSC 8 hyperlink stored in the cell wins over text sniffing —
        // the visible text of a linked cell often isn't the URI at all.
        if let Some(uri) = self.terminal().hyperlink_at_screen(col, row) {
            self.open_detected_url(&uri);
            return true;
        }
        let Some((text, c)) = self.terminal().line_text_at(col, row) else {
            return false;
        };
        let Some(url) = crate::port_detect::url_at(&text, c) else {
            return false;
        };
        self.open_detected_url(&url);
        true
    }

    /// Cmd/Ctrl+click on a printed `path:line[:col]` reference (compiler
    /// errors, test failures, grep hits, tracebacks) jumps the editor
    /// there. Relative paths resolve against the pane shell's live cwd,
    /// then the workspace root; `~` against $HOME. The click only lands
    /// when the resolved file exists, which also filters lookalikes such
    /// as `host.com:443`.
    pub(super) fn terminal_file_click(&mut self, col: u16, row: u16) -> bool {
        let Some((text, c)) = self.terminal().line_text_at(col, row) else {
            return false;
        };
        let Some(fr) = crate::file_ref::file_ref_at(&text, c) else {
            return false;
        };
        let Some(abs) = self.resolve_pane_path(self.active_terminal, &fr.path) else {
            return false;
        };
        self.open_resolved_file_ref(&abs, fr.line, fr.column, fr.end_line)
    }

    /// A path printed in pane `idx`, made absolute: `~` against $HOME, a
    /// relative path against the pane shell's live cwd, then the workspace
    /// root. Shared by the file-ref click and the captures ask.
    pub(super) fn resolve_pane_path(&self, idx: usize, raw: &str) -> Option<std::path::PathBuf> {
        let path = std::path::PathBuf::from(raw);
        if let Some(rest) = raw.strip_prefix("~/") {
            return std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(rest));
        }
        if path.is_absolute() {
            return Some(path);
        }
        let cwd = self
            .terminals
            .get(idx)
            .and_then(|t| t.pid())
            .and_then(cwd_of_pid)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| self.workspace_root().to_path_buf());
        let joined = cwd.join(&path);
        Some(if joined.is_file() {
            joined
        } else {
            self.workspace_root().join(&path)
        })
    }

    /// Jump the editor to an absolute `path:line[:col]`, recording where the
    /// user was so Back returns to it, exactly like go-to-definition. With
    /// `end_line`, a `path:line-end` range, the lines `line..=end_line` are
    /// selected (#804). False when the file doesn't exist or won't open.
    pub(super) fn open_resolved_file_ref(
        &mut self,
        abs: &std::path::Path,
        line: u32,
        column: Option<u32>,
        end_line: Option<u32>,
    ) -> bool {
        self.open_resolved_file_ref_inner(abs, line, column, end_line, true)
    }

    /// [`Self::open_resolved_file_ref`], but `record_nav` false skips the jump
    /// history. A side-by-side open is ONE jump for the user even though it
    /// opens two files, so it records the origin itself and opens both halves
    /// without recording (otherwise the first Back lands on the left half).
    pub(super) fn open_resolved_file_ref_inner(
        &mut self,
        abs: &std::path::Path,
        line: u32,
        column: Option<u32>,
        end_line: Option<u32>,
        record_nav: bool,
    ) -> bool {
        if !abs.is_file() {
            return false;
        }
        if record_nav && let Some(from) = self.editor.path.clone() {
            self.nav.record(NavLoc {
                path: from,
                row: self.editor.cursor_row,
                col: self.editor.cursor_col,
            });
        }
        let line0 = (line as usize).saturating_sub(1);
        let col0 = column.map(|c| c as usize).unwrap_or(1).saturating_sub(1);
        match self.open_at(abs, line0, col0) {
            Ok(()) => {
                // `explorer.autoReveal` analogue, as quick-open does: sync
                // the tree to the landed file without stealing editor focus.
                self.tree.reveal_path(abs);
                let range = end_line.filter(|&e| e > line).and_then(|end| {
                    let last = self.editor.lines.len().checked_sub(1)?;
                    let end0 = (end as usize - 1).min(last);
                    (line0 <= end0).then_some((end0, end))
                });
                self.status = match range {
                    Some((end0, end)) => {
                        // The whole lines, caret at the top so the view
                        // opens on the start of the range.
                        let len = self.editor.lines[end0].chars().count();
                        self.editor.selection = Some(crate::widgets::editor::EditorSelection {
                            anchor: (end0, len),
                            head: (line0, 0),
                        });
                        self.editor.cursor_row = line0;
                        self.editor.cursor_col = 0;
                        format!("{}:{}-{}", self.status_path(abs), line, end)
                    }
                    None => format!("{}:{}", self.status_path(abs), line),
                };
                true
            }
            Err(_) => false,
        }
    }

    /// Open a `diff://` group link's two files side by side: left lands in
    /// the leftmost editor group, right in the rightmost — splitting first
    /// when the editor is a single group, reusing the existing pair when it
    /// is already split (so repeated clicks swap the comparison in place
    /// instead of accreting columns).
    pub(super) fn open_side_by_side(
        &mut self,
        left: &crate::file_ref::FileRef,
        right: &crate::file_ref::FileRef,
    ) -> bool {
        // Both sides absolute by construction (diff_uri refuses relative
        // paths); gated on is_file like every terminal file click.
        let lp = std::path::PathBuf::from(&left.path);
        let rp = std::path::PathBuf::from(&right.path);
        if !lp.is_file() || !rp.is_file() {
            return false;
        }
        // One click, one Back entry: record the origin once here, then open
        // both halves with recording off.
        if let Some(from) = self.editor.path.clone() {
            self.nav.record(NavLoc {
                path: from,
                row: self.editor.cursor_row,
                col: self.editor.cursor_col,
            });
        }
        self.focus_editor_group(true);
        if !self.open_resolved_file_ref_inner(&lp, left.line, left.column, None, false) {
            return false;
        }
        if self.editor_layout.is_split() {
            self.focus_editor_group(false);
        } else {
            // The split duplicates the left file into the new focused group
            // as a preview tab; opening the right file swaps into it.
            self.split_editor_dir(editor_layout::SplitDir::Horizontal, true);
        }
        if !self.open_resolved_file_ref_inner(&rp, right.line, right.column, None, false) {
            return false;
        }
        self.status = format!(
            "Side by side: {} | {}",
            self.status_path(&lp),
            self.status_path(&rp)
        );
        true
    }

    /// Open a URL discovered in the terminal. On a remote session a loopback
    /// dev-server URL is forwarded home and opened; any other link opens on the
    /// local Mac through the drop relay (with the usual confirm gate). On a
    /// local session everything opens directly.
    ///
    /// Editor deep links (`vscode://file/…:line`, and the same shape from
    /// Cursor/Windsurf/Zed, plus plain `file://`) name a file and line, and
    /// Croft is an editor: they open in the editor pane right here, never
    /// through the OS. Beyond those, web links only: an OSC 8 cell carries
    /// whatever URI the printing program chose while showing unrelated text,
    /// so any other custom-scheme URI would hand `open`/`xdg-open` an
    /// arbitrary target (app launch, protocol handler) off one disguised
    /// click. The refusal surfaces the real destination in the status line.
    pub(super) fn open_detected_url(&mut self, url: &str) {
        // A diff:// group link names two files; open them side by side.
        // Same trust story as single-file deep links: consumed internally,
        // gated on is_file, never handed to the OS.
        if let Some((left, right)) = crate::file_ref::diff_uri(url)
            && self.open_side_by_side(&left, &right)
        {
            return;
        }
        if let Some(fr) = crate::file_ref::editor_file_uri(url) {
            // Absolute by construction; gated on is_file like every terminal
            // file click. An unresolvable link falls through to the web-only
            // rule so its refusal still shows the real URI.
            let path = std::path::PathBuf::from(&fr.path);
            if self.open_resolved_file_ref(&path, fr.line, fr.column, fr.end_line) {
                return;
            }
        }
        let scheme_ok = {
            let l = url.trim_start().to_ascii_lowercase();
            l.starts_with("http://") || l.starts_with("https://")
        };
        if !scheme_ok {
            self.status = format!("Refused to open non-web link: {url}");
            return;
        }
        let loopback = crate::port_detect::loopback_url_port(url);
        if self.drop_relay_active() {
            if let Some(port) = loopback {
                self.request_remote_forward(port, true);
            } else if self.trust_local_browser {
                self.request_remote_url_open(url.to_string());
                self.status = format!("Opening {url} on your local Mac");
            } else {
                self.pending_local_open = Some(url.to_string());
            }
            return;
        }
        match open_url(url) {
            Ok(()) => self.status = format!("Opened {url}"),
            Err(e) => self.status = format!("Open link failed: {e}"),
        }
    }

    pub(super) fn render_command_history_popup(&mut self, frame: &mut ratatui::Frame) {
        let full = frame.area();
        let theme = self.theme;
        let Some(pop) = self.command_history_popup.as_mut() else {
            return;
        };
        // Centered box, the zoxide-jump fallback geometry: wide enough for
        // command lines, anchored in the upper half like Quick Open.
        let rect = fallback_popup_rect(full, 110);
        crate::widgets::history_popup::render_history_popup(pop, rect, frame.buffer_mut(), theme);
    }

    /// Cmd+F in the terminal: open a find bar that searches the active pane's
    /// grid + scrollback (iTerm2 / Ghostty scrollback search). Seeds from a
    /// single-line selection and lands on the most recent match.
    pub(super) fn open_terminal_find(&mut self) {
        if self.terminal_find.is_some() {
            return;
        }
        let opts = self.search.opts;
        let sel = self.terminal().selection_text();
        let initial = if !sel.is_empty() && !sel.contains('\n') {
            sel
        } else {
            String::new()
        };
        let lines = find_texts(self.terminal().find_lines());
        let mut state = crate::widgets::editor_find::EditorFind::new(initial.clone(), opts);
        state.set_match_count(
            crate::widgets::editor_find::count_matches(&lines, &initial, opts),
            false,
        );
        self.terminal_find = Some(state);
        self.terminal_find_pane = self.active_terminal;
        self.terminal_find_match = None;
        self.terminal_mut()
            .set_search((!initial.is_empty()).then(|| initial.clone()), opts);
        self.status = String::from(
            "Find in terminal: type to search, Enter next, Shift+Enter prev, Esc close",
        );
        if !initial.is_empty() {
            // Land on the bottom-most (most recent) match, like iTerm2.
            self.terminal_find_jump(false);
        }
    }

    /// Make pane `idx` the active terminal, tearing down the overlays that
    /// were bound to the pane being left.
    ///
    /// Shared by the built-in click path and the bound-gesture dispatch so
    /// the two cannot drift: every terminal helper reaches the grid through
    /// `self.terminal()`, which indexes `active_terminal`, so a caller that
    /// skips this reads a pane the user did not click. That is not
    /// hypothetical — a bound `mouse_open_link_at_click` reported "No link
    /// there" over a plainly visible URL in a split, because the dispatch
    /// returned above the assignment the built-in makes.
    pub(super) fn activate_terminal_pane(&mut self, idx: usize) {
        if self.active_terminal == idx {
            return;
        }
        // Copy mode is bound to the pane it opened on; a click on a sibling
        // pane leaves it, tmux-style, rather than hijacking the new pane's
        // grid.
        if self
            .terminal_copy_mode
            .as_ref()
            .is_some_and(|st| st.pane != idx)
        {
            self.close_terminal_copy_mode();
        }
        // Quick-select labels are pane-bound the same way.
        if self
            .terminal_quick_select
            .as_ref()
            .is_some_and(|st| st.pane != idx)
        {
            self.close_terminal_quick_select();
        }
        // As is the find bar and its highlight.
        if self.terminal_find.is_some() && self.terminal_find_pane != idx {
            self.close_terminal_find();
        }
        self.active_terminal = idx;
    }

    pub(super) fn close_terminal_find(&mut self) {
        if self.terminal_find.take().is_some() {
            self.terminal_find_match = None;
            let opts = self.search.opts;
            // Clear every pane, not just the active one: the close may be
            // the pane-binding invariant firing after `active_terminal`
            // already moved off the pane that owns the highlight.
            for t in &mut self.terminals {
                t.set_search(None, opts);
                t.set_current_match(None, 0);
            }
        }
    }

    /// Re-run the terminal search after the query changed: recount,
    /// re-highlight every occurrence, and re-anchor on the most recent match.
    pub(super) fn terminal_find_set_query(&mut self, new_query: String) {
        let opts = self.search.opts;
        let lines = find_texts(self.terminal().find_lines());
        let count = crate::widgets::editor_find::count_matches(&lines, &new_query, opts);
        if let Some(state) = self.terminal_find.as_mut() {
            state.query = new_query.clone();
            state.set_caret(new_query.chars().count());
            state.set_match_count(count, false);
            state.match_index = None;
        }
        self.terminal_find_match = None;
        self.terminal_mut()
            .set_search((!new_query.is_empty()).then(|| new_query.clone()), opts);
        self.terminal_mut().set_current_match(None, 0);
        if !new_query.is_empty() {
            self.terminal_find_jump(false);
        }
    }

    /// Move to the next (`forward`) or previous match, wrapping. Scrolls the
    /// pane so the match is visible, marks it active, and updates the "N of M"
    /// index. Navigation walks from the current match, or — on the first jump —
    /// from the bottom of the buffer so the newest occurrence wins.
    pub(super) fn terminal_find_jump(&mut self, forward: bool) {
        let Some(state) = self.terminal_find.as_ref() else {
            return;
        };
        let needle = state.query.clone();
        let opts = state.opts;
        if needle.is_empty() {
            return;
        }
        // Logical lines, soft-wrapped rows joined, so a match the pane's
        // width cut in two is found (#1275). A match is named by its line's
        // first row and a char index into the joined text.
        let (find_lines, now) = self.terminal().find_lines_and_clock();
        if find_lines.is_empty() {
            return;
        }
        let lines: Vec<String> = find_lines.iter().map(|l| l.text.clone()).collect();
        let anchor = self.terminal_find_match;
        let (from_row, from_col) = match anchor {
            Some((clock, abs, col, _len)) => {
                // Re-base the stored line by the rows scrolled since it was
                // anchored, so navigation walks from the match's TEXT, not
                // from the viewport row it occupied at anchor time.
                let abs = abs - (now - clock) as i32;
                let row = find_lines.iter().rposition(|l| l.start <= abs).unwrap_or(0);
                (row, col)
            }
            // No anchor: start past the last line's end so `prev` lands on the
            // bottom-most match and `next` wraps to the top-most.
            None => (lines.len().saturating_sub(1), usize::MAX),
        };
        let skip_current = anchor.is_some();
        let m = if forward {
            crate::widgets::editor_find::find_next_match(
                &lines,
                &needle,
                opts,
                from_row,
                from_col,
                skip_current,
            )
        } else {
            crate::widgets::editor_find::find_prev_match(
                &lines,
                &needle,
                opts,
                from_row,
                from_col,
                skip_current,
            )
        };
        let Some(m) = m else {
            return;
        };
        let abs_line = find_lines[m.row].start;
        self.terminal_find_match = Some((now, abs_line, m.col_chars, m.len_chars));
        self.terminal_mut()
            .set_current_match(Some((abs_line, m.col_chars, m.len_chars)), now);
        self.terminal_mut()
            .scroll_to_line(find_lines[m.row].line_of(m.col_chars));
        let idx =
            crate::widgets::editor_find::match_index_at(&lines, &needle, opts, m.row, m.col_chars);
        if let Some(state) = self.terminal_find.as_mut() {
            state.match_index = idx;
        }
    }

    /// Edit the terminal find query around its caret (#1729).
    pub(super) fn terminal_find_edit_at_caret(
        &mut self,
        edit: impl FnOnce(&str, usize) -> (String, usize),
    ) {
        let Some((text, caret)) = self.terminal_find.as_ref().map(|s| {
            let (text, caret) = edit(&s.query, s.caret());
            (text, caret)
        }) else {
            return;
        };
        self.terminal_find_set_query(text);
        if let Some(s) = self.terminal_find.as_mut() {
            s.set_caret(caret);
        }
    }

    /// Move the terminal find caret without touching the query.
    pub(super) fn terminal_find_move_caret(&mut self, step: impl FnOnce(&str, usize) -> usize) {
        if let Some(s) = self.terminal_find.as_mut() {
            let at = step(&s.query, s.caret());
            s.set_caret(at);
        }
    }

    pub(super) fn handle_terminal_find_key(&mut self, key: KeyEvent) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let by_word = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT);
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.close_terminal_find(),
            (KeyCode::Enter, _) | (KeyCode::F(3), _) => {
                self.terminal_find_jump(!shift);
            }
            // The caret keys (#1729), the same as the editor's find bar:
            // the terminal's query is a real input line too.
            (KeyCode::Left, _) => self.terminal_find_move_caret(|t, c| {
                crate::widgets::editor_find::caret_left(t, c, by_word)
            }),
            (KeyCode::Right, _) => self.terminal_find_move_caret(|t, c| {
                crate::widgets::editor_find::caret_right(t, c, by_word)
            }),
            (KeyCode::Home, _) => self.terminal_find_move_caret(|_, _| 0),
            (KeyCode::End, _) => self.terminal_find_move_caret(|t, _| t.chars().count()),
            (KeyCode::Delete, _) => self.terminal_find_edit_at_caret(|t, c| {
                crate::widgets::editor_find::delete_at(t, c, by_word)
            }),
            (KeyCode::Backspace, _) => self.terminal_find_edit_at_caret(|t, c| {
                crate::widgets::editor_find::delete_before(t, c, by_word)
            }),
            (KeyCode::Char('v'), m)
                if m.contains(KeyModifiers::CONTROL) || m.contains(KeyModifiers::SUPER) =>
            {
                if let Some(text) = self.clipboard_text() {
                    self.terminal_find_edit_at_caret(|t, c| {
                        crate::widgets::editor_find::insert_at(t, c, &text)
                    });
                }
            }
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                let typed = c.to_string();
                self.terminal_find_edit_at_caret(|t, caret| {
                    crate::widgets::editor_find::insert_at(t, caret, &typed)
                });
            }
            _ => {}
        }
    }

    /// Ctrl+Shift+Space: enter quick-select on the active pane. Scans the
    /// visible viewport for URLs / paths / hashes / IPs (see
    /// [`crate::quick_select`]), assigns home-row labels with the bottom-most
    /// match getting the cheapest one (WezTerm / tmux-thumbs), and overlays
    /// them for the render loop.
    pub(super) fn open_terminal_quick_select(&mut self) {
        let (lines, wraps, top, clock) = self.terminal_mut().visible_lines_and_clock();
        let mut matches = crate::quick_select::find_matches_wrapped(&lines, &wraps);
        // A hex run or a number inside a masked span would be a fragment of
        // the secret one keystroke from the clipboard (#360): no hint there.
        if self.triggers.has_redactions() && !self.redactions_revealed() {
            // Over wrapped rows joined, as the painter masks them.
            let masked = crate::triggers::redacted_row_ranges(&lines, &wraps, &self.triggers);
            let clear = |row: usize, start: usize, len: usize| {
                masked.get(row).is_none_or(|ranges| {
                    ranges
                        .iter()
                        .all(|&(s, l)| start + len <= s || start >= s + l)
                })
            };
            // A match that wraps the pane edge continues in `tail`; every
            // segment must be clear, or the fragment past the wrap leaks.
            matches.retain(|m| {
                clear(m.row, m.start, m.len) && m.tail.iter().all(|&(r, s, l)| clear(r, s, l))
            });
        }
        if matches.is_empty() {
            self.status = String::from("Quick select: nothing to match on screen");
            return;
        }
        let labels =
            crate::quick_select::assign_labels(matches.len(), crate::quick_select::ALPHABET);
        let dropped = matches.len().saturating_sub(labels.len());
        let hints: Vec<QuickHint> = crate::quick_select::label_matches(matches, &labels)
            .into_iter()
            .map(|(m, label)| QuickHint {
                line: top + m.row as i32,
                start: m.start,
                len: m.len,
                text: m.text,
                label,
                tail: m
                    .tail
                    .iter()
                    .map(|&(r, s, l)| (top + r as i32, s, l))
                    .collect(),
            })
            .collect();
        self.push_quick_select_hints(&hints, "", clock);
        self.terminal_quick_select = Some(QuickSelectState {
            pane: self.active_terminal,
            clock,
            hints,
            typed: String::new(),
            shifted: false,
        });
        self.status = if dropped > 0 {
            format!(
                "Quick select: type a label to copy, UPPERCASE pastes, Esc cancels ({dropped} top matches unlabelled)"
            )
        } else {
            String::from("Quick select: type a label to copy, UPPERCASE pastes, Esc cancels")
        };
    }

    /// Push the hint spans matching the typed label prefix down to the pane
    /// for painting. Called on open and after every typed / erased char.
    pub(super) fn push_quick_select_hints(&mut self, hints: &[QuickHint], typed: &str, clock: i64) {
        let spans: Vec<crate::widgets::terminal::HintSpan> = hints
            .iter()
            .filter(|h| h.label.starts_with(typed))
            .flat_map(|h| {
                // The head span carries the label glyph; a wrapped match's
                // tail segments highlight their continuation rows with the
                // label fully "typed" so no glyph ever paints on them.
                std::iter::once(crate::widgets::terminal::HintSpan {
                    line: h.line,
                    start: h.start,
                    len: h.len,
                    label: h.label.clone(),
                    typed: typed.len(),
                })
                .chain(h.tail.iter().map(|&(line, start, len)| {
                    crate::widgets::terminal::HintSpan {
                        line,
                        start,
                        len,
                        label: h.label.clone(),
                        typed: h.label.len(),
                    }
                }))
            })
            .collect();
        self.terminal_mut().set_hints(Some(spans), clock);
    }

    pub(super) fn close_terminal_quick_select(&mut self) {
        if self.terminal_quick_select.take().is_some() {
            // Clear every pane, not just the active one: a mouse click can
            // move `active_terminal` while the labels are still up.
            for t in &mut self.terminals {
                t.set_hints(None, 0);
            }
        }
    }

    /// Ctrl+Shift+Y: enter copy mode on the active pane. The cursor starts
    /// at the shell's cursor cell (WezTerm's convention) and the pane paints
    /// it as a green modal block.
    pub(super) fn open_terminal_copy_mode(&mut self) {
        // Cursor, bounds, and clock from ONE snapshot (see
        // `begin_annotate_terminal` for why the pairing must be atomic).
        let ((line, col), (top, bottom, cols, _), clock) =
            self.terminal_mut().cursor_bounds_and_clock();
        let line = line.clamp(top, bottom);
        let col = col.min(cols.saturating_sub(1));
        self.terminal_mut().set_copy_cursor(Some((line, col)));
        self.terminal_copy_mode = Some(CopyModeState {
            pane: self.active_terminal,
            line,
            col,
            anchor: None,
            kind: CopySelKind::Char,
            clock,
        });
        self.status = String::from(
            "Copy mode: h/j/k/l w/b/e 0/$ g/G move, v/V/^V select, y copies, Esc exits",
        );
    }

    pub(super) fn close_terminal_copy_mode(&mut self) {
        if self.terminal_copy_mode.take().is_some() {
            // Clear every pane, not just the active one: a mouse click can
            // move `active_terminal` while the mode is up.
            for t in &mut self.terminals {
                t.set_copy_cursor(None);
                t.clear_selection();
            }
        }
    }

    /// Push the copy-mode cursor + selection into the pane and keep the
    /// cursor on screen (centering only when it left the viewport).
    pub(super) fn apply_terminal_copy_mode(&mut self) {
        let Some(st) = self.terminal_copy_mode.as_ref() else {
            return;
        };
        // Bound to its own pane — never paint another pane's grid (see
        // `handle_terminal_copy_mode_key`).
        if st.pane != self.active_terminal {
            self.close_terminal_copy_mode();
            return;
        }
        let (line, col) = (st.line, st.col);
        let (_, _, cols, rows) = self.terminal().grid_bounds();
        let sel = st.anchor.map(|(al, ac)| match st.kind {
            CopySelKind::Char => crate::widgets::terminal::Selection {
                anchor: (al, ac),
                head: (line, col),
                block: false,
            },
            CopySelKind::Line => crate::widgets::terminal::Selection {
                anchor: (al.min(line), 0),
                head: (al.max(line), cols.saturating_sub(1)),
                block: false,
            },
            CopySelKind::Block => crate::widgets::terminal::Selection {
                anchor: (al, ac),
                head: (line, col),
                block: true,
            },
        });
        let off = self.terminal().scroll_display_offset();
        let visible = (-off, rows as i32 - 1 - off);
        let t = self.terminal_mut();
        t.set_selection(sel);
        t.set_copy_cursor(Some((line, col)));
        if line < visible.0 || line > visible.1 {
            t.scroll_to_line(line);
        }
    }

    /// The char index in `text` whose grid column covers `col` (the last
    /// char whose start column is <= col), for word motions over the
    /// spacer-skipped row text.
    pub(super) fn char_idx_at_col(colmap: &[usize], col: u16) -> usize {
        let col = col as usize;
        match colmap.binary_search(&col) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        }
    }

    pub(super) fn handle_terminal_copy_mode_key(&mut self, key: KeyEvent) {
        let Some(mut st) = self.terminal_copy_mode.take() else {
            return;
        };
        // The mode is bound to the pane it opened on: if a gesture moved
        // `active_terminal` since (a click on a sibling pane), the stored
        // coordinates and clock describe the wrong pane — close instead of
        // folding another pane's clock into them.
        if st.pane != self.active_terminal {
            self.terminal_copy_mode = Some(st);
            self.close_terminal_copy_mode();
            return;
        }
        // Content streamed since the last key: fold the scroll-clock
        // movement into the stored coordinates so this keystroke moves
        // from where the pane's re-anchored highlight actually is.
        let now = self.terminal_mut().scroll_clock();
        let drift = (now - st.clock) as i32;
        if drift != 0 {
            st.line -= drift;
            if let Some(a) = st.anchor.as_mut() {
                a.0 -= drift;
            }
            st.clock = now;
        }
        let (top, bottom, cols, rows) = self.terminal().grid_bounds();
        // Content that fell off the scrollback pins to the oldest line,
        // mirroring the pane's own rebase.
        st.line = st.line.clamp(top, bottom);
        if let Some(a) = st.anchor.as_mut() {
            a.0 = a.0.clamp(top, bottom);
        }
        let last_col = cols.saturating_sub(1);
        let half = (rows as i32 / 2).max(1);
        let page = (rows as i32 - 1).max(1);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let word = crate::widgets::terminal::is_terminal_word_char;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.terminal_copy_mode = Some(st);
                self.close_terminal_copy_mode();
                self.status = String::from("Copy mode off");
                return;
            }
            KeyCode::Char('y') | KeyCode::Enter => {
                let text = self.terminal_text_for_copy(self.terminal().selection_text());
                self.terminal_copy_mode = Some(st);
                self.close_terminal_copy_mode();
                if text.is_empty() {
                    self.status =
                        String::from("Copy mode: nothing selected (v/V/^V starts a selection)");
                } else {
                    self.deliver_copied_text(&text);
                }
                return;
            }
            // Selection arms: v again (same kind) drops the anchor, a
            // different kind reshapes the existing selection in place.
            KeyCode::Char('v') if ctrl => {
                if st.anchor.is_some() && st.kind == CopySelKind::Block {
                    st.anchor = None;
                } else {
                    st.anchor.get_or_insert((st.line, st.col));
                    st.kind = CopySelKind::Block;
                }
            }
            KeyCode::Char('v') => {
                if st.anchor.is_some() && st.kind == CopySelKind::Char {
                    st.anchor = None;
                } else {
                    st.anchor.get_or_insert((st.line, st.col));
                    st.kind = CopySelKind::Char;
                }
            }
            KeyCode::Char('V') => {
                if st.anchor.is_some() && st.kind == CopySelKind::Line {
                    st.anchor = None;
                } else {
                    st.anchor.get_or_insert((st.line, st.col));
                    st.kind = CopySelKind::Line;
                }
            }
            KeyCode::Char('h') | KeyCode::Left => st.col = st.col.saturating_sub(1),
            KeyCode::Char('l') | KeyCode::Right => st.col = (st.col + 1).min(last_col),
            KeyCode::Char('j') | KeyCode::Down => st.line = (st.line + 1).min(bottom),
            KeyCode::Char('k') | KeyCode::Up => st.line = (st.line - 1).max(top),
            KeyCode::Char('0') | KeyCode::Home => st.col = 0,
            KeyCode::Char('$') | KeyCode::End => {
                let (text, colmap) = self.terminal().row_text(st.line);
                let trimmed = text.trim_end();
                let chars = trimmed.chars().count();
                st.col = if chars == 0 {
                    0
                } else {
                    colmap.get(chars - 1).map_or(0, |&c| c as u16)
                };
            }
            KeyCode::Char('g') => st.line = top,
            KeyCode::Char('G') => st.line = bottom,
            KeyCode::Char('u') if ctrl => st.line = (st.line - half).max(top),
            KeyCode::Char('d') if ctrl => st.line = (st.line + half).min(bottom),
            KeyCode::PageUp => st.line = (st.line - page).max(top),
            KeyCode::PageDown => st.line = (st.line + page).min(bottom),
            KeyCode::Char('w') => {
                let (text, colmap) = self.terminal().row_text(st.line);
                let chars: Vec<char> = text.chars().collect();
                let mut i = Self::char_idx_at_col(&colmap, st.col);
                while i < chars.len() && word(chars[i]) {
                    i += 1;
                }
                while i < chars.len() && !word(chars[i]) {
                    i += 1;
                }
                // Trailing blanks reach the row's end: wrap to the next row
                // start like vim's w does at end of line.
                if i >= text.trim_end().chars().count() && st.line < bottom {
                    st.line += 1;
                    st.col = 0;
                } else {
                    st.col = colmap.get(i).map_or(last_col, |&c| c as u16);
                }
            }
            KeyCode::Char('b') => {
                let (text, colmap) = self.terminal().row_text(st.line);
                let chars: Vec<char> = text.chars().collect();
                let mut i = Self::char_idx_at_col(&colmap, st.col);
                if i == 0 {
                    if st.line > top {
                        st.line -= 1;
                        let (ptext, pcolmap) = self.terminal().row_text(st.line);
                        let pchars = ptext.trim_end().chars().count();
                        st.col = if pchars == 0 {
                            0
                        } else {
                            pcolmap.get(pchars - 1).map_or(0, |&c| c as u16)
                        };
                    }
                } else {
                    i -= 1;
                    while i > 0 && !word(chars[i]) {
                        i -= 1;
                    }
                    while i > 0 && word(chars[i - 1]) {
                        i -= 1;
                    }
                    st.col = colmap.get(i).map_or(0, |&c| c as u16);
                }
            }
            KeyCode::Char('e') => {
                let (text, colmap) = self.terminal().row_text(st.line);
                let chars: Vec<char> = text.chars().collect();
                let content = text.trim_end().chars().count();
                let mut i = Self::char_idx_at_col(&colmap, st.col);
                i += 1;
                while i < chars.len() && !word(chars[i]) {
                    i += 1;
                }
                // Past the last word on this row (the scan ran into the
                // trailing padding): hop to the next row's first word end,
                // like vim's `e` at end of line — `w` and `b` both wrap,
                // and without this `e` was a dead key at the row's last word.
                if i >= content && st.line < bottom {
                    // Word-less rows are skipped like vim: `e` lands on the
                    // end of the next word wherever it lives below, and only
                    // falls back to the next row's column 0 when nothing
                    // down to the bottom has a word.
                    let mut landed = None;
                    let mut line = st.line + 1;
                    while line <= bottom {
                        let (ntext, ncolmap) = self.terminal().row_text(line);
                        let nchars: Vec<char> = ntext.chars().collect();
                        let mut j = 0;
                        while j < nchars.len() && !word(nchars[j]) {
                            j += 1;
                        }
                        if j < nchars.len() && word(nchars[j]) {
                            while j + 1 < nchars.len() && word(nchars[j + 1]) {
                                j += 1;
                            }
                            landed = Some((line, ncolmap.get(j).map_or(0, |&c| c as u16)));
                            break;
                        }
                        line += 1;
                    }
                    (st.line, st.col) = landed.unwrap_or((st.line + 1, 0));
                } else {
                    while i + 1 < chars.len() && word(chars[i + 1]) {
                        i += 1;
                    }
                    if i < chars.len() {
                        st.col = colmap.get(i).map_or(last_col, |&c| c as u16);
                    }
                }
            }
            _ => {}
        }
        st.line = st.line.clamp(top, bottom);
        st.col = st.col.min(last_col);
        self.terminal_copy_mode = Some(st);
        self.apply_terminal_copy_mode();
    }

    pub(super) fn handle_terminal_quick_select_key(&mut self, key: KeyEvent) {
        // Bound to its origin pane: if anything moved the active pane while
        // the labels were up, close instead of pushing (or pasting) the old
        // pane's coordinates and text into the new one.
        if self
            .terminal_quick_select
            .as_ref()
            .is_some_and(|st| st.pane != self.active_terminal)
        {
            self.close_terminal_quick_select();
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.close_terminal_quick_select();
                self.status = String::from("Quick select cancelled");
            }
            KeyCode::Backspace => {
                let Some(state) = self.terminal_quick_select.as_mut() else {
                    return;
                };
                state.typed.pop();
                // Case history is not kept; retyping decides paste-vs-copy.
                state.shifted = false;
                let (hints, typed, clock) = (state.hints.clone(), state.typed.clone(), state.clock);
                self.push_quick_select_hints(&hints, &typed, clock);
            }
            KeyCode::Char(c) if c.is_ascii_alphabetic() => {
                let Some(state) = self.terminal_quick_select.as_ref() else {
                    return;
                };
                let candidate = format!("{}{}", state.typed, c.to_ascii_lowercase());
                let shifted = state.shifted || c.is_ascii_uppercase();
                if let Some(hit) = state.hints.iter().find(|h| h.label == candidate) {
                    // A hint can land inside a masked span (a hex run, a
                    // number); the clipboard rule applies here too (#360).
                    let text = self.terminal_text_for_copy(hit.text.clone());
                    self.close_terminal_quick_select();
                    let copied = self
                        .copy_where_pasted(&text)
                        .status(&format!("{} chars", text.chars().count()));
                    if shifted {
                        self.terminal_mut().paste_input(text.as_bytes());
                        self.status = format!("{copied} and pasted into the shell");
                    } else {
                        self.status = copied;
                    }
                } else if state.hints.iter().any(|h| h.label.starts_with(&candidate)) {
                    let Some(state) = self.terminal_quick_select.as_mut() else {
                        return;
                    };
                    state.typed = candidate.clone();
                    state.shifted = shifted;
                    let (hints, clock) = (state.hints.clone(), state.clock);
                    self.push_quick_select_hints(&hints, &candidate, clock);
                }
                // A char no label starts with is ignored, like WezTerm.
            }
            _ => {}
        }
    }

    /// Draw the find-in-terminal bar over the active terminal pane, reusing
    /// the editor find widget (query row only). Same top-right placement.
    pub(super) fn render_terminal_find(&mut self, frame: &mut ratatui::Frame) {
        if self.terminal_find.is_none() || !self.show_terminal {
            return;
        }
        let theme = self.theme;
        let area = self.terminal().last_area;
        if area.width == 0 || area.height == 0 {
            return;
        }
        let Some(state) = self.terminal_find.as_mut() else {
            return;
        };
        crate::widgets::editor_find::render_editor_find(state, area, frame.buffer_mut(), theme);
    }

    /// Paint the history suggestion after the focused terminal's typed line
    /// (#614) and remember it for Right arrow. Cleared whenever it does not
    /// apply, so a stale suggestion can never be accepted.
    pub(super) fn paint_terminal_suggestion(&mut self, frame: &mut ratatui::Frame) {
        self.term_suggestion = None;
        if !self.term_suggest_enabled
            || self.focus != Pane::Terminal
            || self.context_menu.is_some()
            || self.prompt.is_some()
        {
            return;
        }
        let idx = self.active_terminal;
        let Some(term) = self.terminals.get(idx) else {
            return;
        };
        // Typed keys not echoed yet: the line on screen is behind, and a
        // suggestion for it would be accepted after the keys that followed.
        if term.awaiting_echo() {
            return;
        }
        let Some((typed, x, y, right)) = term.prompt_tail() else {
            return;
        };
        let cwd = term
            .shell_cwd()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(cmd) = self.command_history.suggest(&typed, &cwd, "") else {
            return;
        };
        let rest = cmd[typed.len()..].to_string();
        if x >= right {
            return;
        }
        let shown: String = rest.chars().take((right - x) as usize).collect();
        frame
            .buffer_mut()
            .set_string(x, y, &shown, Style::default().fg(self.theme.ignored_fg()));
        self.term_suggestion = Some((idx, typed, rest));
    }

    /// Terminal: Toggle Command Suggestions (#614).
    pub(super) fn toggle_terminal_suggestions(&mut self) {
        self.term_suggest_enabled = !self.term_suggest_enabled;
        self.term_suggestion = None;
        self.status = format!(
            "Terminal suggestions: {}",
            if self.term_suggest_enabled {
                "on"
            } else {
                "off"
            }
        );
    }

    pub(super) fn toggle_copy_on_select(&mut self) {
        self.copy_on_select = !self.copy_on_select;
        self.status = if self.copy_on_select {
            String::from("Copy on Selection: on")
        } else {
            String::from("Copy on Selection: off")
        };
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_copy_on_select(self.copy_on_select));
        }
    }
}

/// `app_cursor` is the pane's DECCKM state: cursor keys switch from the
/// normal CSI form (`\e[A`) to SS3 (`\eOA`), the form terminfo advertises —
/// apps that bind keys from terminfo (Python's REPL, less) only match SS3.
pub(super) fn key_to_bytes(key: KeyEvent, app_cursor: bool) -> Vec<u8> {
    use KeyCode::*;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // xterm's modifier parameter: 1 + Shift 1 + Alt 2 + Ctrl 4. A modified
    // cursor or editing key carries it (`Ctrl+Left` is `\e[1;5D`); sending
    // the plain sequence dropped the modifier, so word motion and selection
    // bindings in shells and editors never fired.
    let m = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let cursor = |app: &'static [u8], normal: &'static [u8], fin: char| {
        if m > 1 {
            format!("\x1b[1;{m}{fin}").into_bytes()
        } else if app_cursor {
            app.to_vec()
        } else {
            normal.to_vec()
        }
    };
    let tilde = |n: u8| {
        if m > 1 {
            format!("\x1b[{n};{m}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let esc_if_alt = |mut v: Vec<u8>| {
        if alt && !v.is_empty() {
            v.insert(0, 0x1b);
        }
        v
    };
    match key.code {
        Enter => esc_if_alt(vec![b'\r']),
        Tab => vec![b'\t'],
        BackTab => b"\x1b[Z".to_vec(),
        // Alt+Backspace deletes a word in readline; Ctrl+Backspace sends ^H.
        Backspace => esc_if_alt(vec![if ctrl { 0x08 } else { 0x7f }]),
        Esc => vec![0x1b],
        // Alt alone on Left/Right keeps readline's word motion (`\eb`/`\ef`),
        // which every shell binds; other modifiers use the xterm form.
        Right if alt && !ctrl && !shift => b"\x1bf".to_vec(),
        Left if alt && !ctrl && !shift => b"\x1bb".to_vec(),
        Up => cursor(b"\x1bOA", b"\x1b[A", 'A'),
        Down => cursor(b"\x1bOB", b"\x1b[B", 'B'),
        Right => cursor(b"\x1bOC", b"\x1b[C", 'C'),
        Left => cursor(b"\x1bOD", b"\x1b[D", 'D'),
        Home => cursor(b"\x1bOH", b"\x1b[H", 'H'),
        End => cursor(b"\x1bOF", b"\x1b[F", 'F'),
        PageUp => tilde(5),
        PageDown => tilde(6),
        Insert => tilde(2),
        Delete => tilde(3),
        F(n) => match n {
            1..=4 => {
                let fin = b"PQRS"[usize::from(n - 1)] as char;
                if m > 1 {
                    format!("\x1b[1;{m}{fin}").into_bytes()
                } else {
                    format!("\x1bO{fin}").into_bytes()
                }
            }
            5 => tilde(15),
            6 => tilde(17),
            7 => tilde(18),
            8 => tilde(19),
            9 => tilde(20),
            10 => tilde(21),
            11 => tilde(23),
            12 => tilde(24),
            _ => Vec::new(),
        },
        Char(c) => {
            if ctrl {
                let lc = c.to_ascii_lowercase();
                let byte = if lc.is_ascii_lowercase() {
                    Some((lc as u8) - b'a' + 1)
                } else {
                    match c {
                        ' ' | '@' | '2' => Some(0x00),
                        '[' | '3' => Some(0x1b),
                        '\\' | '4' => Some(0x1c),
                        ']' | '5' => Some(0x1d),
                        '^' | '6' => Some(0x1e),
                        '/' | '_' | '7' => Some(0x1f),
                        '8' | '?' => Some(0x7f),
                        _ => None,
                    }
                };
                // Ctrl+Alt+letter is ESC then the control byte.
                byte.map_or_else(Vec::new, |b| esc_if_alt(vec![b]))
            } else {
                let mut buf = [0u8; 4];
                esc_if_alt(c.encode_utf8(&mut buf).as_bytes().to_vec())
            }
        }
        _ => Vec::new(),
    }
}
