//! Terminal panes: split, layout, collapse, maximize and rail, reorder, profiles,
//! rename, notes, close and undo, saving and restoring the panel, redraw tracking,
//! recordings, bells and exits, labels and pane buttons.

use super::*;

/// Which shape the copy-mode selection takes: vim's v / V / Ctrl+V.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CopySelKind {
    Char,
    Line,
    Block,
}

/// How long a closed terminal pane stays parked (child alive, hidden)
/// before the tick drops it for real. Ghostty and iTerm2 default their
/// undo-close window to 5s; croft doubles it because the hint lives in a
/// TUI status line that is easier to miss than a native toast.
pub const UNDO_CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

impl RunningStatus {
    /// The bar shows `shown`, just set by the run of `label`.
    pub(super) fn new(shown: &str, label: String) -> Self {
        Self {
            shown: shown.to_string(),
            label,
            started: std::time::Instant::now(),
        }
    }

    /// The run is over: put `outcome(label, time since it started)` in
    /// `status`, only if that still shows what the run set. Whether it did.
    pub(super) fn settle(
        self,
        status: &mut String,
        outcome: impl FnOnce(&str, std::time::Duration) -> String,
    ) -> bool {
        if *status != self.shown {
            return false;
        }
        *status = outcome(&self.label, self.started.elapsed());
        true
    }
}

/// How a command a terminal pane ran ended, for the status bar (#856), from
/// the shell's own command-finished report: `label finished (exit 0, 0.4
/// s)`, or `label failed (exit 2, 1.3 s)`. A shell that reported no exit
/// code gets `label finished (0.4 s)`.
pub(super) fn pane_run_outcome(label: &str, exit: Option<i32>, dur: std::time::Duration) -> String {
    let took = format!("{:.1} s", dur.as_secs_f64());
    match exit {
        Some(0) => format!("{label} finished (exit 0, {took})"),
        Some(code) => format!("{label} failed (exit {code}, {took})"),
        None => format!("{label} finished ({took})"),
    }
}

/// How often terminal pane labels are refreshed from their foreground process.
pub(super) const TERMINAL_LABEL_INTERVAL: Duration = Duration::from_millis(1500);

impl App {
    /// Spawn a new PTY-backed terminal next to the existing ones and make
    /// it the active one. The pane becomes visible if it was hidden. The
    /// new terminal's cwd is the *live* cwd of the active terminal's shell
    /// (so a `cd somewhere` inside the user's prompt is reflected), with
    /// the workspace root as a fallback if we can't resolve it.
    ///
    /// The new terminal lands immediately to the right of the currently
    /// active one, not at the far end of the strip, so the user's mental
    /// model of "the new tab sits next to where I was" matches what they
    /// see. Inserting at the end would scatter related splits across the
    /// row whenever the active terminal wasn't already on the right edge.
    pub fn split_terminal(&mut self) -> Result<()> {
        self.insert_terminal(
            PtyTerminal::new(&self.next_terminal_cwd()).context("spawning terminal")?,
        );
        Ok(())
    }

    /// Open a new pane running a specific shell (a terminal profile) instead of
    /// `$SHELL`. Same placement and focus behaviour as [`split_terminal`].
    pub fn split_terminal_with_shell(&mut self, shell_path: &str) -> Result<()> {
        let cwd = self.next_terminal_cwd();
        self.insert_terminal(
            PtyTerminal::new_shell(shell_path, &cwd).context("spawning terminal")?,
        );
        Ok(())
    }

    /// The cwd a new terminal should inherit: the active terminal's live
    /// directory if it has one, else the workspace root.
    pub(super) fn next_terminal_cwd(&self) -> PathBuf {
        // The shell's own OSC 7 report (shell integration) beats sampling
        // the process table — it is exact and free of proc-inspection
        // races — but only when the REPORTING host is this machine: an
        // in-pane ssh session names a remote path that would silently
        // resolve to the wrong local directory whenever it exists here.
        self.terminal()
            .local_shell_cwd()
            .filter(|p| p.is_dir())
            .or_else(|| {
                self.terminal()
                    .pid()
                    .and_then(cwd_of_pid)
                    .filter(|p| p.is_dir())
            })
            .unwrap_or_else(|| self.workspace_root().to_path_buf())
    }

    /// Insert `term` just after the active pane, make it active, and reveal the
    /// terminal pane.
    pub(super) fn insert_terminal(&mut self, term: PtyTerminal) {
        let target = self.active_terminal + 1;
        self.terminals.insert(target, term);
        self.active_terminal = target;
        if !self.show_terminal {
            self.show_terminal = true;
        }
        self.focus_pane(Pane::Terminal);
        self.save_terminal_session();
    }

    /// Set a structural-change status, keeping any latched session-store
    /// failure visible beside it (#158 review: plain assignments right
    /// after a failed save silently hid the persistence error).
    pub(super) fn terminal_status(&mut self, msg: String) {
        self.status = match &self.terminal_store_error {
            Some(e) => format!("{msg} — session not saved: {e}"),
            None => msg,
        };
    }

    /// Snapshot the terminal panel (pane order, cwds, names, focus) into
    /// the per-workspace session store. Called on structural changes and at
    /// quit; the default single-pane layout prunes the record instead.
    pub fn save_terminal_session(&mut self) {
        let root = self.workspace_root().display().to_string();
        let triggers = self.triggers.clone();
        let panes = self
            .terminals
            .iter()
            .map(|t| crate::terminal_session::PaneRecord {
                cwd: t
                    .local_shell_cwd()
                    .filter(|p| p.is_dir())
                    .or_else(|| t.pid().and_then(cwd_of_pid).filter(|p| p.is_dir()))
                    .unwrap_or_else(|| self.workspace_root().to_path_buf())
                    .display()
                    .to_string(),
                name: t.manual_name().map(str::to_string),
                lane: self.lane_panes.get(&t.uid()).cloned(),
                // The pane's own output, so a restored pane shows what you
                // were doing instead of a blank grid (#249). Trailing blank
                // rows are dropped: a mostly-empty screen would otherwise
                // restore as a wall of nothing.
                transcript: if t.alt_screen() {
                    // A full-screen program's frame is not scrollback: the
                    // alternate screen exists precisely so its contents
                    // vanish on exit. Persisting vim's tildes or htop's
                    // meters and replaying them as inert text above a fresh
                    // prompt would be worse than restoring nothing.
                    // `grid_lines` is the one grid reader here without an
                    // ALT_SCREEN guard of its own, so the check lives here.
                    Vec::new()
                } else {
                    let (lines, wraps, _) = t.grid_lines_wrapped();
                    let end = lines
                        .iter()
                        .rposition(|l| !l.trim().is_empty())
                        .map_or(0, |i| i + 1);
                    // Masked before the cut, over wrapped rows joined, so a
                    // secret straddling a wrap (or the cut) is still masked.
                    // Persisted to disk and replayed next launch: every
                    // redact rule applies (#360), as for the dump.
                    let masked = crate::triggers::mask_rows(&lines[..end], &wraps, &triggers);
                    let start = end.saturating_sub(crate::terminal_session::TRANSCRIPT_LINES);
                    masked[start..end].to_vec()
                },
                host: t.host_socket().map(|s| s.display().to_string()),
            })
            .collect();
        let record = crate::terminal_session::SessionRecord {
            panes,
            active: self.active_terminal,
        };
        // A corrupt store is refused, never replaced (#157). The error is
        // LATCHED, not just put in the status line: the structural-change
        // call sites set their own success statuses right after this
        // returns, which silently clobbered a transient message (#158
        // review) — terminal_status() re-attaches it to whatever they say.
        self.terminal_store_error =
            crate::terminal_session::save_for_root(&self.terminal_session_path, &root, record)
                .err();
    }

    /// Resurrect this workspace's terminal panel from the session store.
    /// Called once at startup, replacing the default lone pane.
    ///
    /// What comes back depends on where croft runs (#1458). A local launch
    /// brings back only panes with something still in them: a shell still
    /// running in its pane host, and an agent lane, whose agent is seated
    /// again. A plain pane whose shell has ended would come back as an
    /// empty fresh shell, so it is left out, and a launch with nothing to
    /// bring back keeps the one default terminal. A remote croft (over
    /// SSH, as `croft remote` runs it) restores every pane, so a dropped
    /// connection comes back to the panel it had.
    pub fn restore_terminal_session(&mut self) {
        self.restore_terminal_session_with(self.terminal_restore_respawns);
    }

    /// [`Self::restore_terminal_session`], with `respawn` saying whether a
    /// plain pane whose shell has ended comes back as a fresh shell in its
    /// directory (with its name and transcript). A recorded directory that
    /// no longer exists falls back to the workspace root; a pane whose
    /// shell fails to spawn is skipped. Focus returns to the pane that had
    /// it, or the first pane when that one was not brought back.
    pub(super) fn restore_terminal_session_with(&mut self, respawn: bool) {
        let map = crate::terminal_session::load(&self.terminal_session_path);
        let Some(rec) = map.get(&self.workspace_root().display().to_string()) else {
            return;
        };
        let mut terms = Vec::new();
        let mut lanes = std::collections::BTreeMap::new();
        let mut active = 0;
        for (i, p) in rec.panes.iter().enumerate() {
            // A lane pane belongs in its worktree, whatever cwd its shell
            // had drifted to at save time (#348): the launch line below is
            // typed into THIS shell, so the directory and the lane are
            // decided together. Deciding them apart seated the agent where
            // the shell had wandered, or, when that cwd was gone, in the
            // PRIMARY repo the fallback names, which is the one directory a
            // lane exists to keep the agent out of. A lane whose worktree
            // is gone is an ordinary pane now.
            let lane = p.lane.as_ref().filter(|l| Path::new(&l.path).is_dir());
            let dir = match lane {
                Some(l) => PathBuf::from(&l.path),
                None => {
                    let dir = PathBuf::from(&p.cwd);
                    if dir.is_dir() {
                        dir
                    } else {
                        self.workspace_root().to_path_buf()
                    }
                }
            };
            // A shell still running in its pane host (croft was killed, not
            // quit) is reattached as it is: its screen is redrawn by the
            // host, and a lane's agent is still running in it (#694).
            let reattached = p
                .host
                .as_deref()
                .map(Path::new)
                .filter(|s| crate::session::is_alive(s))
                .and_then(|s| PtyTerminal::attach_hosted(s).ok());
            if let Some(mut t) = reattached {
                t.set_manual_name(p.name.clone());
                if let Some(lane) = lane {
                    lanes.insert(t.uid(), lane.clone());
                }
                if i == rec.active {
                    active = terms.len();
                }
                terms.push(t);
                continue;
            }
            if !respawn && lane.is_none() {
                continue;
            }
            // The transcript is painted during the spawn, not after it: a
            // replay that follows the constructor races the new shell's
            // first prompt for the same grid (#249).
            if let Ok(mut t) = PtyTerminal::new_with_transcript(&dir, &p.transcript) {
                t.set_manual_name(p.name.clone());
                // The agent is seated again, from the row's CURRENT launch
                // line, so an edited row applies.
                if let Some(lane) = lane {
                    if let Some(cmd) = lane
                        .agent
                        .as_deref()
                        .and_then(|a| self.agents.launch_line(a))
                    {
                        t.write_input(lane_launch_input(cmd).as_bytes());
                    }
                    lanes.insert(t.uid(), lane.clone());
                }
                // Focus follows the saved active pane only when it is
                // actually brought back; one left out or failing to spawn
                // keeps the first-pane fallback, not a neighbour.
                if i == rec.active {
                    active = terms.len();
                }
                terms.push(t);
            }
        }
        if terms.is_empty() {
            return;
        }
        self.lane_panes = lanes;
        // The restored panes are brand-new PtyTerminals, but everything
        // bound to a pane of the OUTGOING panel is keyed by pane index —
        // copy mode, find, quick-select — which an identical index in the
        // restored panel would wrongly satisfy, applying stale grid
        // coordinates to a grid that never produced them. The undo-close
        // park would likewise resurrect an outgoing workspace's PTY into
        // the restored panel. Drop them all before the swap.
        self.close_terminal_copy_mode();
        self.close_terminal_find();
        self.close_terminal_quick_select();
        self.closed_terminals.clear();
        self.terminals = terms;
        self.active_terminal = active.min(self.terminals.len() - 1);
        self.sync_focus_flags();
        self.status = format!("Restored terminal session ({} panes)", self.terminals.len());
        self.save_terminal_session_if_hosted();
    }

    /// Record the panel now when a pane's shell is in a pane host (#694):
    /// the record is how a croft that crashes finds its shells again, so it
    /// must not wait for the next split or a clean quit.
    pub fn save_terminal_session_if_hosted(&mut self) {
        if self.terminals.iter().any(|t| t.host_socket().is_some()) {
            self.save_terminal_session();
        }
    }

    /// Available terminal profiles as `(shell_path, label)`: the shells in
    /// `/etc/shells`, with the user's current `$SHELL` ensured present
    /// (prepended) so the default is always an option even on a box with no
    /// `/etc/shells`.
    pub(super) fn terminal_profiles(&self) -> Vec<(String, String)> {
        let contents = std::fs::read_to_string("/etc/shells").unwrap_or_default();
        let mut shells = crate::widgets::terminal::parse_shells(&contents);
        if let Ok(current) = std::env::var("SHELL")
            && !current.is_empty()
        {
            let base = std::path::Path::new(&current)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&current)
                .to_string();
            if !shells.iter().any(|(_, b)| *b == base) {
                shells.insert(0, (current, base));
            }
        }
        shells
    }

    /// Where the profile dropdown hangs: under the `⌄` caret of the pane it was
    /// opened from, with the menu auto-clamping up/left to stay on screen.
    /// Anchoring on the first caret in the vec is wrong twice over — hidden
    /// panes hold `Rect::default()` placeholders so a button's position names
    /// its terminal, and side by side every pane has a live caret, so every
    /// pane's dropdown would hang under the leftmost one. Falls back to the
    /// toolbar's top-right corner before the caret has been painted.
    pub(super) fn terminal_caret_origin(&self, pane: usize) -> (u16, u16) {
        self.terminal_profile_buttons
            .get(pane)
            .filter(|c| c.width > 0)
            .map(|c| (c.x, c.y.saturating_add(1)))
            .unwrap_or((self.last_frame_area.width.saturating_sub(20), 1))
    }

    /// Open the terminal-profile dropdown (VS Code's `⌄` next to `+`): a compact
    /// context menu anchored under the caret, one row per shell. Picking a row
    /// opens a new pane running that shell.
    pub(super) fn open_terminal_profile_picker(&mut self, pane: usize) {
        let items: Vec<MenuEntry> = self
            .terminal_profiles()
            .into_iter()
            .map(|(path, label)| MenuEntry::Item {
                label,
                action: MenuAction::NewTerminalWithProfile(path),
            })
            .collect();
        if items.is_empty() {
            self.status = String::from("No terminal profiles found");
            return;
        }
        let origin = self.terminal_caret_origin(pane);
        // The dropdown is anchored under the `⌄` caret in the terminal
        // toolbar and grows downward into the terminal panel — it never
        // overlaps the editor's welcome logo, so the cached OSC-1337 image
        // is left intact rather than evicted (evicting it left the logo
        // blank, since the welcome emit is gated off while a menu is open).
        self.context_menu = Some(ContextMenu {
            crumb: None,
            scroll: Default::default(),
            submenu_scroll: Default::default(),
            origin,
            items,
            selected: 0,
            open_submenu: None,
            submenu_selected: 0,
            target_dir: self.tree.root.clone(),
        });
    }

    /// A terminal action run as a command (#1562): from the palette, a
    /// `keybindings.json` chord or an imported VS Code one. Each acts on the
    /// active pane, showing the panel when it was hidden, as its chord and
    /// menu row do; those that take the keyboard (rename, copy mode, quick
    /// select, history) also focus the terminal and bring its tab to the
    /// front, since another bottom-panel tab would hide the mode and take
    /// its keys.
    pub(super) fn run_terminal_command(&mut self, cmd: crate::widgets::command_palette::Command) {
        use crate::widgets::command_palette::Command as Cmd;
        self.show_terminal = true;
        let idx = self.active_terminal;
        match cmd {
            Cmd::TerminalClear => self.clear_terminal_at(idx),
            Cmd::TerminalRename => self.begin_rename_terminal(idx),
            Cmd::TerminalKill => {
                if self.close_terminal_at(idx) {
                    self.terminal_status(format!(
                        "Closed terminal: {} remaining",
                        self.terminals.len()
                    ));
                } else {
                    self.status =
                        String::from("Cannot close the last terminal; press Ctrl+J to hide");
                }
            }
            Cmd::TerminalSplit => match self.split_terminal() {
                Ok(()) => {
                    self.terminal_status(format!(
                        "Split terminal: {} active",
                        self.live_terminal_count()
                    ));
                }
                Err(e) => {
                    self.status = format!("Split terminal failed: {e}");
                }
            },
            Cmd::TerminalNewWithProfile => self.open_terminal_profile_picker(idx),
            Cmd::TerminalQuickSelect => {
                self.set_bottom_panel_tab(BottomPanelTab::Terminal);
                self.open_terminal_quick_select();
            }
            Cmd::TerminalCopyMode => {
                self.set_bottom_panel_tab(BottomPanelTab::Terminal);
                self.open_terminal_copy_mode();
            }
            Cmd::TerminalCommandHistory => {
                self.set_bottom_panel_tab(BottomPanelTab::Terminal);
                self.open_command_history();
            }
            Cmd::TerminalOpenScrollback => self.open_scrollback_in_editor(),
            _ => {}
        }
    }

    /// Clear the terminal at `idx`'s screen and scrollback (Cmd+K K / menu).
    pub(super) fn clear_terminal_at(&mut self, idx: usize) {
        if let Some(t) = self.terminals.get_mut(idx) {
            t.clear_screen_and_scrollback();
            // The cleared grid invalidates find's state on this pane: the
            // anchored match names content that no longer exists (and the
            // clear can kill the scroll clock's tracer), so navigation
            // restarts from scratch and the count reflects the empty grid.
            if self.terminal_find.is_some() && self.terminal_find_pane == idx {
                self.terminal_find_match = None;
                self.terminals[idx].set_current_match(None, 0);
                let lines = find_texts(self.terminals[idx].find_lines());
                if let Some(state) = self.terminal_find.as_mut() {
                    state.set_match_count(
                        crate::widgets::editor_find::count_matches(
                            &lines,
                            &state.query,
                            state.opts,
                        ),
                        false,
                    );
                    state.match_index = None;
                }
            }
            self.status = String::from("Cleared terminal");
        }
    }

    /// Open a prompt to rename the terminal pane at `idx` (Cmd+K R / menu),
    /// pre-filled with its current label.
    pub(super) fn begin_rename_terminal(&mut self, idx: usize) {
        let Some(t) = self.terminals.get(idx) else {
            return;
        };
        let current = t.label().to_string();
        self.prompt = Some(Prompt {
            label: String::from("Rename Terminal"),
            buffer: current,
            caret: usize::MAX,
            kind: PromptKind::RenameTerminal(idx),
            target_dir: PathBuf::new(),
            error: None,
        });
    }

    /// Cmd+K N: open the annotation prompt for the active pane's selection
    /// (its first line's span). When the span overlaps an existing
    /// annotation, the prompt edits that note instead.
    pub(super) fn begin_annotate_terminal(&mut self) {
        let pane = self.active_terminal;
        // Selection and clock from ONE snapshot: separate reads let PTY
        // output land in between, pairing a pre-output line with a
        // post-output clock and pinning the note to the wrong row.
        let (sel, clock) = self.terminals[pane].selection_and_clock();
        let Some(sel) = sel.filter(|s| s.has_area()) else {
            self.status = String::from("Annotate: select some output first (drag or copy mode)");
            return;
        };
        let (sr, sc, er, ec) = sel.normalised();
        let (_, _, cols, _) = self.terminals[pane].grid_bounds();
        let end = if er > sr { cols.saturating_sub(1) } else { ec };
        let (start, len) = (sc, end.saturating_sub(sc) + 1);
        // Translate the existing annotations at the SAME captured clock as
        // the selection, or output scrolling in between shifts them off the
        // fixed `sr` and the same span duplicates instead of editing.
        let current = self.terminals[pane].annotations_at_clock(clock);
        let existing = current
            .iter()
            .position(|&(l, s0, ln, _)| l == sr && s0 < start + len && start < s0 + ln);
        let buffer = existing
            .and_then(|i| current.get(i).map(|a| a.3.clone()))
            .unwrap_or_default();
        self.prompt = Some(Prompt {
            label: String::from("Annotate Selection"),
            buffer,
            caret: usize::MAX,
            kind: PromptKind::AnnotateTerminal {
                pane,
                line: sr,
                clock,
                start,
                len,
                existing,
            },
            target_dir: PathBuf::new(),
            error: None,
        });
    }

    /// Cmd+K Shift+N: drop every annotation whose span intersects the
    /// selection's first line span.
    pub(super) fn delete_terminal_annotations_in_selection(&mut self) {
        let pane = self.active_terminal;
        let Some(sel) = self.terminals[pane].selection().filter(|s| s.has_area()) else {
            self.status = String::from("Delete annotation: select over the note first");
            return;
        };
        let (sr, sc, er, ec) = sel.normalised();
        let (_, _, cols, _) = self.terminals[pane].grid_bounds();
        let end = if er > sr { cols.saturating_sub(1) } else { ec };
        let hits: Vec<usize> = self.terminals[pane]
            .annotations_current()
            .iter()
            .enumerate()
            .filter(|&(_, &(l, s0, ln, _))| l == sr && s0 <= end && sc < s0 + ln)
            .map(|(i, _)| i)
            .collect();
        if hits.is_empty() {
            self.status = String::from("No annotation under the selection");
            return;
        }
        for i in hits.into_iter().rev() {
            self.terminals[pane].remove_annotation(i);
        }
        self.status = String::from("Deleted annotation");
    }

    /// Drop the terminal at `idx`. Returns false (and does nothing) when
    /// only one terminal is left or `idx` is out of range. Hiding the pane
    /// is the user's job (Ctrl+J), not ours, so the last terminal stays.
    /// The active index shifts so the same logical terminal remains active
    /// when an inactive pane is closed; closing the active pane reseats the
    /// focus on the neighbour to the left (or the new last if we removed
    /// the right edge).
    ///
    /// The pane is not dropped (which would kill its child): it parks in
    /// `closed_terminals` for [`UNDO_CLOSE_GRACE`] so Cmd+K ⇧T can bring it
    /// back, process and scrollback intact. The tick reaps expired entries.
    pub fn close_terminal_at(&mut self, idx: usize) -> bool {
        if self.terminals.len() <= 1 || idx >= self.terminals.len() {
            return false;
        }
        let term = self.terminals.remove(idx);
        // A lane pane closed by any route stops being the lane's (#348): the
        // map is keyed by uid, and an undo-close brings the pane back as an
        // ordinary one rather than re-seating an agent nobody asked for.
        self.lane_panes.remove(&term.uid());
        self.closed_terminals.push(ClosedTerminal {
            term,
            idx,
            closed_at: std::time::Instant::now(),
        });
        if self.active_terminal == idx {
            if self.active_terminal >= self.terminals.len() {
                self.active_terminal = self.terminals.len() - 1;
            }
        } else if self.active_terminal > idx {
            self.active_terminal -= 1;
        }
        // Closing the only expanded pane would leave the row nothing but
        // strips; the collapse gesture refuses that from its own side and
        // this closes the other route to it.
        self.ensure_a_terminal_pane_is_expanded();
        // A lone terminal has nothing to maximize against; the rail would be
        // empty, so the split view is the honest state.
        if self.terminals.len() <= 1 {
            self.terminal_pane_maximized = false;
            // Safety: never keep broadcast armed against a single pane —
            // the next split would silently start receiving mirrored input.
            if self.broadcast_input {
                self.broadcast_input = false;
                self.status = String::from("Broadcast input off (one pane left)");
            }
        }
        self.sync_focus_flags();
        self.save_terminal_session();
        true
    }

    /// Toggle maximize mode for the active terminal pane (Cmd+K M / the
    /// pane's `⛶` button): one pane spans the panel's width and the other
    /// terminals move to the right rail, where clicking a row hands it the
    /// maximized pane. Orthogonal to `toggle_terminal_maximize`
    /// (Ctrl+Shift+J), which grows the panel over the editor: this
    /// reapportions the panel's width, not the window's rows.
    pub fn toggle_terminal_pane_maximize(&mut self) {
        if self.terminal_pane_maximized {
            self.terminal_pane_maximized = false;
            // The rail lists folded panes too, so the pane that held the
            // maximized view may be a strip once the split comes back. The
            // invariant lives in `sync_focus_flags` and this is the gesture
            // that re-enters its scope.
            self.sync_focus_flags();
            self.terminal_status(String::from("Restored terminal split"));
        } else if self.terminals.len() > 1 {
            self.terminal_pane_maximized = true;
            self.status = String::from("Maximized terminal");
        } else {
            self.status = String::from("Only one terminal open");
        }
    }

    /// Paint a collapsed pane's one-column strip (#313) and return its hit
    /// rect: the expand chevron on the header row, then the pane's name set
    /// vertically down the column.
    ///
    /// A strip rather than hiding the pane outright. A pane that vanishes is
    /// indistinguishable from one that was closed, and croft already has
    /// closing, with its own undo window; the strip keeps the pane counted,
    /// named and clickable, which is what makes folding it obviously
    /// reversible.
    pub(super) fn paint_terminal_collapsed_strip(
        &mut self,
        frame: &mut ratatui::Frame,
        area: Rect,
        idx: usize,
    ) -> Rect {
        if area.width == 0 || area.height == 0 {
            return Rect::default();
        }
        let hovered = self.pointer_cell.is_some_and(|(px, py)| {
            px >= area.x && px < area.x + area.width && py >= area.y && py < area.y + area.height
        });
        frame.render_widget(
            ratatui::widgets::Block::default().style(Style::default().bg(self.theme.editor_bg())),
            area,
        );
        let chevron = crate::widgets::header_pill::action_style(self.theme, hovered);
        let name = crate::widgets::header_pill::action_style(self.theme, false);
        let label = self.terminals[idx].label().to_string();
        let buf = frame.buffer_mut();
        buf.set_string(area.x, area.y, TERMINAL_EXPAND_GLYPH.to_string(), chevron);
        // The name runs down the strip so a folded pane is still identifiable
        // without unfolding it. Only single-width characters are placed: a CJK
        // glyph or an emoji is two cells wide and would paint over the pane
        // next door, which is one column away.
        for (row, ch) in label
            .chars()
            .filter_map(|c| match unicode_width::UnicodeWidthChar::width(c) {
                Some(1) => Some(c),
                // Wide: keep the position, lose the glyph.
                Some(w) if w > 1 => Some(TERMINAL_STRIP_WIDE_STANDIN),
                // Zero-width — a combining mark or a control character —
                // has nothing to stand in for and no cell of its own.
                _ => None,
            })
            .take(area.height.saturating_sub(1) as usize)
            .enumerate()
        {
            buf.set_string(area.x, area.y + 1 + row as u16, ch.to_string(), name);
        }
        area
    }

    /// Collapse the pane at `idx` to a strip, or give a collapsed one its
    /// width back (#313). The `‹` header button, the strip, the right-click
    /// entry and the palette command all land here.
    ///
    /// Collapsing the LAST expanded pane is refused: folding every pane away
    /// leaves a row of strips with nothing in it, and croft already has
    /// Ctrl+J for putting the panel away entirely (Ctrl+Shift+J is the
    /// opposite gesture, growing it over the editor). The refusal says so
    /// on the status line rather than swallowing the click, so the button
    /// never reads as broken.
    pub fn toggle_terminal_collapse(&mut self, idx: usize) {
        let Some(pane) = self.terminals.get(idx) else {
            return;
        };
        if pane.collapsed {
            self.terminals[idx].collapsed = false;
            self.active_terminal = idx;
            self.focus_pane(Pane::Terminal);
            let msg = format!("Expanded terminal {}", self.terminals[idx].label());
            self.terminal_status(msg);
            return;
        }
        if self.terminals.iter().filter(|t| !t.collapsed).count() <= 1 {
            self.status = String::from("Cannot collapse the last expanded terminal");
            return;
        }
        self.terminals[idx].collapsed = true;
        // Focus cannot stay on a pane that is now one column wide: it shows no
        // cursor, so the keystrokes would go somewhere the user cannot see
        // them arrive. `sync_focus_flags` owns that rule for every path, this
        // one included - a second copy of the nearest-expanded walk here would
        // be dead weight AND a tie-break that can drift out of step with the
        // one that matters.
        self.sync_focus_flags();
        let msg = format!("Collapsed terminal {}", self.terminals[idx].label());
        self.terminal_status(msg);
    }

    /// Fold the active pane, unless maximize owns the panel (#313).
    ///
    /// The `‹` button is not painted while maximized and the right-click entry
    /// is suppressed there, but the chord and the palette command reached
    /// `toggle_terminal_collapse` directly and so had no such guard. Because
    /// collapsing reassigns `active_terminal` and the maximize branch hands
    /// the panel to whatever that is, the effect was to swap the maximized
    /// pane for a neighbour while the status line announced a collapse that
    /// was invisible until you left maximize.
    pub fn collapse_active_terminal_pane(&mut self) {
        if self.terminal_pane_maximized {
            self.status = String::from("Collapse is unavailable while a pane is maximized");
            return;
        }
        self.toggle_terminal_collapse(self.active_terminal);
    }

    /// Make the pane at `idx` the active one AND visible (#313).
    ///
    /// For callers that deliberately TARGET a pane by index - jump to a
    /// captured line, run a task in its own pane, run a fenced block, focus an
    /// agent's pane - rather than for a gesture that merely moves focus.
    ///
    /// The distinction is load-bearing. `sync_focus_flags` walks focus OFF a
    /// folded pane, which is right for a drifting gesture and wrong here: it
    /// silently redirected these callers to a neighbour, so a task command was
    /// written into a pane that is never rendered while the status line
    /// reported it running. Reaching for a pane by index is a request to see
    /// it, which is the rationale `undo_close_terminal` already applies when
    /// it force-expands a reopened pane.
    pub(super) fn reveal_terminal_pane(&mut self, idx: usize) {
        if let Some(t) = self.terminals.get_mut(idx) {
            t.collapsed = false;
        }
        self.active_terminal = idx;
    }

    /// Give every collapsed pane its width back (Cmd+K `]`): the issue's
    /// "maximise the terminals back to the same equal-sized terminal
    /// windows", which has to be one gesture rather than one per pane.
    pub fn restore_all_terminal_panes(&mut self) {
        // The mirror of the guard on `collapse_active_terminal_pane`. Without
        // it, Cmd+K ] while maximized cleared every flag and announced it, for
        // a change the user cannot see until they leave maximize: the same
        // silent-state-change this pair already refuses in the other
        // direction.
        if self.terminal_pane_maximized {
            self.terminal_status(String::from(
                "Restore is unavailable while a pane is maximized",
            ));
            return;
        }
        let folded = self.terminals.iter().filter(|t| t.collapsed).count();
        if folded == 0 {
            self.terminal_status(String::from("No collapsed terminal panes"));
            return;
        }
        for t in self.terminals.iter_mut() {
            t.collapsed = false;
        }
        self.terminal_status(format!(
            "Restored {folded} terminal pane{}",
            if folded == 1 { "" } else { "s" }
        ));
    }

    /// Keep at least one pane expanded after a pane leaves the row (#313).
    ///
    /// [`Self::toggle_terminal_collapse`] refuses to fold the last expanded
    /// pane, but CLOSING reaches the same state from the other side: two
    /// panes with one collapsed, close the expanded one, and the panel is a
    /// single strip with nothing behind it. Expanding the pane that is now
    /// active, rather than all of them, keeps the rest of the user's
    /// folding intact.
    pub(super) fn ensure_a_terminal_pane_is_expanded(&mut self) {
        if self.terminals.is_empty() || self.terminals.iter().any(|t| !t.collapsed) {
            return;
        }
        let idx = self.active_terminal.min(self.terminals.len() - 1);
        self.terminals[idx].collapsed = false;
    }

    /// Drop the rail's hit rects and its column. Both must go together: a
    /// stale area would keep swallowing the wheel after the rail left screen.
    pub(super) fn clear_terminal_rail(&mut self) {
        self.terminal_rail_rects.clear();
        self.terminal_rail_area = Rect::default();
    }

    /// Paint the maximize-mode rail: one row per terminal down the panel's
    /// right edge — the active one highlighted, the rest hover-lit — so the
    /// user can shuffle between maximized terminals. Records one hit rect
    /// per row in `terminal_rail_rects`, index-aligned with `terminals`.
    pub(super) fn paint_terminal_rail(&mut self, frame: &mut ratatui::Frame, rail: Rect) {
        self.terminal_rail_area = rail;
        if rail.width == 0 || rail.height == 0 {
            return;
        }
        let brand = self.theme.gradient();
        let tones = crate::widgets::surface::tones(self.theme);
        // Same selected-row treatment as the OPEN EDITORS list, so the rail
        // reads as the same family of chrome in both themes.
        let sel_bg = if brand {
            crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG)
        } else {
            self.theme.ui(Color::Rgb(0x09, 0x4d, 0x77))
        };
        // With more terminals than rows the rail is a window over the list,
        // scrolled by the wheel — the panel floors at 3 rows, so four panes
        // are enough to reach this. Switching panes scrolls the new one into
        // view so its highlight is never off-screen, but only on the frame the
        // active pane CHANGES: revealing every frame would pin the window
        // whenever the active pane sits at either end and make the wheel inert.
        let rows = rail.height as usize;
        let max_scroll = self.terminals.len().saturating_sub(rows);
        let mut scroll = self.terminal_rail_scroll.min(max_scroll);
        if self.terminal_rail_revealed != Some(self.active_terminal) {
            if self.active_terminal < scroll {
                scroll = self.active_terminal;
            } else if self.active_terminal >= scroll + rows {
                scroll = self.active_terminal + 1 - rows;
            }
            self.terminal_rail_revealed = Some(self.active_terminal);
        }
        self.terminal_rail_scroll = scroll;
        for i in 0..self.terminals.len() {
            // Rows outside the window still push a placeholder, so a rect's
            // position in the vec keeps naming its terminal. `rect_contains`
            // rejects empty rects, so they can never phantom-match a click.
            if i < scroll || i >= scroll + rows {
                self.terminal_rail_rects.push(Rect::default());
                continue;
            }
            let y = rail.y + (i - scroll) as u16;
            let row = Rect {
                x: rail.x,
                y,
                width: rail.width,
                height: 1,
            };
            let active = i == self.active_terminal;
            let bg = if active && tones.is_none() {
                Some(sel_bg)
            } else if active {
                None
            } else {
                crate::widgets::hover::row_hover_bg(row, self.pointer_cell, self.theme)
            };
            if let Some(bg) = bg {
                frame.buffer_mut().set_style(row, Style::default().bg(bg));
            }
            let label = self.terminals[i].label().to_string();
            let room = TERMINAL_RAIL_LABEL_W as usize;
            let shown: String = label.chars().take(room).collect();
            let fg = match tones {
                // Croft Loch: the active terminal's name in body text on the
                // selected-row band, the rest muted.
                Some(t) => Style::default().fg(if active { t.text() } else { t.muted() }),
                None if active => Style::default()
                    .fg(self.theme.ui(Color::White))
                    .add_modifier(Modifier::BOLD),
                None => Style::default(),
            };
            // `set_string` clips at the buffer edge, not the rail's, so a
            // label wider than a clamped rail would paint its tail over the
            // panel to the right. Clip to the rail's own width.
            frame.buffer_mut().set_stringn(
                row.x,
                y,
                format!(" {TERMINAL_RAIL_ICON} {shown}"),
                row.width as usize,
                fg,
            );
            // The one selected-row anatomy: band plus the accent bar in the
            // row's leading space, the bar only while the panel has the
            // keyboard.
            if active && let Some(t) = tones {
                crate::widgets::surface::paint_selected_row(
                    frame.buffer_mut(),
                    row,
                    t,
                    t.canvas,
                    self.focus == Pane::Terminal,
                );
            }
            self.terminal_rail_rects.push(row);
        }
    }

    /// Move the terminal at `from` so it sits at slot `to`, keeping
    /// `active_terminal` pointing at the same terminal it did before. Drives
    /// the drag-reorder of side-by-side panes and maximize-mode rail rows.
    pub fn move_terminal(&mut self, from: usize, to: usize) {
        if from == to || from >= self.terminals.len() || to >= self.terminals.len() {
            return;
        }
        // `last_area` is a SLOT's geometry, but it lives on the terminal
        // object and so travels with it. The panes do not actually move on
        // screen until the next frame, and the drag hit-tests `last_area`
        // between frames — the main loop drains the whole queued mouse burst
        // before drawing. Re-seat the rects in slot order so the next motion
        // report over the same cell resolves to the pane now sitting there
        // instead of swapping the two straight back, which made the final
        // order the parity of however many motion reports arrived.
        let slot_rects: Vec<Rect> = self.terminals.iter().map(|t| t.last_area).collect();
        let t = self.terminals.remove(from);
        self.terminals.insert(to, t);
        self.active_terminal = match self.active_terminal {
            a if a == from => to,
            a if from < a && a <= to => a - 1,
            a if to <= a && a < from => a + 1,
            a => a,
        };
        if self.terminal_pane_maximized {
            // Maximize mode has exactly one live rect, and it belongs to
            // whichever pane was active at the LAST PAINT — not to the dragged
            // terminal, since the press and the drag are drained together. So
            // neither positional rule works here: find the live rect and hand
            // it to the pane the drag leaves active, which is the one that will
            // own the panel on the next frame.
            let pane = slot_rects
                .iter()
                .copied()
                .find(|r| r.width > 0)
                .unwrap_or_default();
            let active = self.active_terminal;
            for (i, term) in self.terminals.iter_mut().enumerate() {
                term.last_area = if i == active { pane } else { Rect::default() };
            }
        } else {
            for (term, rect) in self.terminals.iter_mut().zip(slot_rects) {
                // A folded pane is not painted in whatever slot the drag left
                // it in, so it must not inherit that slot's geometry (#313):
                // the rest of the drained mouse burst hit-tests these rects,
                // and a live rect over an unpainted pane resolves clicks to a
                // pane that is not there.
                term.last_area = if term.collapsed {
                    Rect::default()
                } else {
                    rect
                };
            }
        }
        self.terminal_session_dirty = true;
    }

    /// Write the pane session if a reorder marked it pending. Called once per
    /// tick so a drag costs one write instead of one per motion report.
    pub fn flush_terminal_session(&mut self) {
        if std::mem::take(&mut self.terminal_session_dirty) {
            self.save_terminal_session();
        }
    }

    /// Drop the currently-active terminal. The shortcut and the pane menu
    /// go through [`Self::close_terminal_reporting`]; tests use this.
    #[cfg(test)]
    pub fn close_active_terminal(&mut self) -> bool {
        self.close_terminal_at(self.active_terminal)
    }

    /// Open another terminal beside the active one and say so: the `Cmd+T`
    /// chord and the pane menu's Split Terminal (#1598).
    pub(super) fn split_terminal_reporting(&mut self) {
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
    }

    /// Close the pane at `idx` and say so, or say why not: the `Cmd+W` chord
    /// (on the active pane) and the pane menu's Kill Terminal (#1598).
    pub(super) fn close_terminal_reporting(&mut self, idx: usize) {
        if self.close_terminal_at(idx) {
            self.terminal_status(format!(
                "Closed terminal: {} remaining",
                self.terminals.len()
            ));
        } else {
            self.status = String::from("Cannot close the last terminal; press Ctrl+J to hide");
        }
    }

    /// A key typed in the active pane after its shell exited (#912): close
    /// the pane, or start a new shell in its place when it is the last.
    pub(super) fn dismiss_exited_terminal(&mut self) {
        let idx = self.active_terminal;
        let last = self.terminals.len() == 1;
        if last && let Err(e) = self.split_terminal() {
            self.status = format!("Could not start a new shell: {e}");
            return;
        }
        if self.close_terminal_at(idx) {
            // Not parked for undo-close: there is no shell to bring back.
            self.closed_terminals.pop();
        }
        self.terminal_status(if last {
            String::from("Started a new shell")
        } else {
            format!("Closed terminal: {} remaining", self.terminals.len())
        });
    }

    /// Panes whose shell is still running: one kept after its shell exited
    /// (#912) is not an active terminal.
    pub(super) fn live_terminal_count(&self) -> usize {
        self.terminals.iter().filter(|t| !t.has_exited()).count()
    }

    /// Cmd+K ⇧T: bring back the most recently closed terminal pane while it
    /// is still inside the undo-close grace window. The pane returns to the
    /// slot it occupied (clamped if the layout shrank) and takes focus; its
    /// child process and scrollback were never touched.
    pub fn undo_close_terminal(&mut self) {
        let Some(parked) = self.closed_terminals.pop() else {
            self.status = String::from("No recently closed terminals");
            return;
        };
        let idx = parked.idx.min(self.terminals.len());
        self.terminals.insert(idx, parked.term);
        // Reopening is a request to see the pane, so it comes back expanded
        // whatever it was when it was closed: a restore that put a strip on
        // screen would read as the reopen having failed.
        self.terminals[idx].collapsed = false;
        self.active_terminal = idx;
        if !self.show_terminal {
            self.show_terminal = true;
        }
        self.focus_pane(Pane::Terminal);
        self.sync_focus_flags();
        self.save_terminal_session();
        let msg = format!("Restored terminal {}", self.terminals[idx].label());
        self.terminal_status(msg);
    }

    /// Drop parked closed panes whose grace window lapsed (their Drop kills
    /// the child). Called from the drain tick.
    pub(super) fn reap_closed_terminals(&mut self) {
        self.closed_terminals
            .retain(|p| p.closed_at.elapsed() <= UNDO_CLOSE_GRACE);
    }

    /// Swap the per-host accent rules (config.json reload, and tests).
    pub fn set_host_accents(&mut self, rules: &[crate::prefs::HostAccentRule]) {
        self.host_accents = compile_host_accents(rules);
    }

    /// Cycle the active terminal forward to the next pane the user can
    /// actually see, wrapping at the end.
    ///
    /// Collapsed panes are stepped over (#313). A folded pane is one column
    /// wide and is never rendered, so it paints no cursor and no focus
    /// border: cycling onto one would send every keystroke into a pane the
    /// user cannot see receiving them. `toggle_terminal_collapse` defends
    /// that same invariant on its own path; this is the other way in, and it
    /// predated the flag.
    pub fn cycle_terminal(&mut self) {
        self.step_active_terminal(true);
    }

    /// Cycle the active terminal backward, wrapping at the front. Mirror of
    /// `cycle_terminal` for the Cmd+[ binding, folded panes skipped alike.
    pub fn cycle_terminal_back(&mut self) {
        self.step_active_terminal(false);
    }

    /// Walk `active_terminal` one visible pane in either direction.
    ///
    /// The loop is bounded by the pane count and at least one pane is always
    /// expanded (`toggle_terminal_collapse` refuses to fold the last one and
    /// `ensure_a_terminal_pane_is_expanded` closes the other route), so it
    /// always terminates on a pane that is painted. If that invariant were
    /// ever broken the walk falls through without moving, which is inert
    /// rather than a hang.
    pub(super) fn step_active_terminal(&mut self, forward: bool) {
        let n = self.terminals.len();
        if n <= 1 {
            return;
        }
        for step in 1..=n {
            let cand = if forward {
                (self.active_terminal + step) % n
            } else {
                (self.active_terminal + n - (step % n)) % n
            };
            if !self.terminals[cand].collapsed {
                self.active_terminal = cand;
                break;
            }
        }
        self.sync_focus_flags();
    }

    /// OR-fold dirty flags across all terminals while clearing each. Use `|`
    /// (not `||`) so every terminal's flag is consumed even after the first
    /// dirty one is found.
    pub fn drain_terminals_dirty(&mut self) -> bool {
        let dirty = self
            .terminals
            .iter()
            .fold(false, |acc, t| acc | t.take_dirty());
        // A recording captures the SCREEN, not the byte stream (#356). The
        // PTY bytes are consumed by the grid before the app sees them, so
        // there is no text stream left to tee — and a cast of screens is
        // what a player replays anyway. Sampled here because this fires
        // exactly when something changed, so an idle session records
        // nothing rather than a frame per tick.
        if dirty && self.recording.is_some() {
            self.record_active_screen();
        }
        dirty
    }

    /// Write a resize event to the open recording (#356).
    pub(super) fn record_resize(&mut self, cols: u16, rows: u16) {
        use std::io::Write as _;
        let Some((rec, file, started)) = self.recording.as_mut() else {
            return;
        };
        let line = rec.line(&crate::asciicast::Event::Resize {
            at: started.elapsed().as_secs_f64(),
            cols,
            rows,
        });
        if writeln!(file, "{line}").is_err() {
            self.recording = None;
            self.status = String::from("Recording stopped: could not write to the cast file");
        }
    }

    /// Write the active terminal's current screen to the open recording.
    ///
    /// Clear-and-redraw rather than a delta: `\u{1b}[H\u{1b}[2J` then the
    /// rows. A delta would need the previous screen and a diff that agrees
    /// with the terminal's own scroll behaviour, and a cast that drifts a
    /// row from reality is the kind of bug nobody notices until the demo is
    /// published.
    ///
    /// Each row is written with the colours and attributes its cells
    /// showed, re-emitted as SGR per run of equal style, and the frame ends
    /// with the cursor where the shell left it (hidden when the program hid
    /// it), so a player shows the session rather than its text. A row a
    /// redact rule touches is written as its masked plain text and gives up
    /// its colour, as the scrollback dump's rows do: a mask spliced between
    /// escapes could break them.
    pub(super) fn record_active_screen(&mut self) {
        // Read everything the frame needs from the pane in ONE borrow, then
        // write. The two writes below both take `&mut self`, so interleaving
        // them with pane reads would need the pane borrowed twice around
        // them — which compiles but reads as an accident rather than as a
        // decision.
        let triggers = self.triggers.clone();
        let Some((size, lines, cursor)) = self.terminals.get(self.active_terminal).map(|t| {
            // The VISIBLE screen only, never the scrollback above it: writing
            // history after a clear scrolls the live screen straight off the
            // top, so the cast would show the tail of the history rather
            // than what the user was looking at, at ~400 KB per frame.
            let (rows, cursor) = t.screen_ansi_wrapped();
            let plains: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
            let wraps: Vec<bool> = rows.iter().map(|r| r.2).collect();
            // Masked like a scrollback dump (#360): a cast is a file made to
            // be shared, so every redact rule applies, and a reveal on
            // screen never reaches it.
            let masked = crate::triggers::mask_rows(&plains, &wraps, &triggers);
            let lines: Vec<String> = rows
                .into_iter()
                .zip(masked)
                .map(|((plain, ansi, _), masked)| if masked == plain { ansi } else { masked })
                .collect();
            ((t.last_inner.width, t.last_inner.height), lines, cursor)
        }) else {
            return;
        };
        // The resize goes FIRST, so a player has the new geometry before the
        // frame drawn at it — the other order renders one frame wrapped at
        // the old width.
        if size != self.recorded_size && size.0 > 0 && size.1 > 0 {
            self.recorded_size = size;
            self.record_resize(size.0, size.1);
        }
        let mut frame = String::from("\u{1b}[H\u{1b}[2J");
        frame.push_str(&lines.join("\r\n"));
        frame.push_str(&cursor_suffix(cursor));
        self.record_terminal_output(&frame);
    }

    /// Like `drain_terminals_dirty` but does not clear the underlying flags.
    /// The main loop uses this to decide whether to coalesce a PTY-only
    /// redraw without losing the signal if it chooses to skip.
    pub fn peek_terminals_dirty(&self) -> bool {
        self.terminals.iter().any(|t| t.peek_dirty())
    }

    /// Total bytes the PTY reader threads have advanced into their grids
    /// since the last redraw. The main loop uses this to bypass the PTY
    /// redraw cap for small interactive echo while keeping bulk streams
    /// capped. Peeks without clearing; `drain_terminals_dirty` resets it.
    pub fn peek_terminals_pending_bytes(&self) -> usize {
        self.terminals.iter().map(|t| t.peek_pending_bytes()).sum()
    }

    pub(super) fn terminal_at_pos(&self, col: u16, row: u16) -> Option<usize> {
        self.terminals
            .iter()
            .position(|t| rect_contains(t.last_area, col, row))
    }

    /// Host accents: each pane's shell-reported OSC 7 host against the prefs
    /// rules (first match wins). Cleared the moment the host stops matching
    /// (an in-pane SSH session ends). Runs from `sync_focus_flags` AND at the
    /// top of `render`: the OSC 7 host arrives on a PTY reader thread with no
    /// input event behind it, so the PTY-dirty redraw is the only wakeup —
    /// dressing must not wait for the next key or click.
    pub(super) fn dress_host_accents(&mut self) {
        for t in self.terminals.iter_mut() {
            let dress = t.shell_host().and_then(|h| {
                self.host_accents
                    .iter()
                    .find(|(m, _, _)| m.is_match(&h))
                    .map(|(_, c, b)| (*c, b.clone()))
            });
            match dress {
                Some((c, b)) => {
                    t.accent = Some(c);
                    t.accent_badge = b;
                }
                None => {
                    t.accent = None;
                    t.accent_badge = None;
                }
            }
        }
    }

    /// Start or stop recording the active terminal as an asciicast (#356).
    ///
    /// Toggling rather than two commands, because a recording is a mode with
    /// exactly one visible state: a user who cannot remember whether they
    /// started one reaches for the same entry either way, and gets told
    /// which happened.
    pub(super) fn toggle_session_recording(&mut self) {
        use std::io::Write as _;
        if let Some((_, _, started)) = self.recording.take() {
            let secs = started.elapsed().as_secs_f64();
            self.status = format!("Stopped recording after {secs:.1}s");
            return;
        }
        let Some(term) = self.terminals.get(self.active_terminal) else {
            self.status = String::from("No terminal pane to record");
            return;
        };
        // `last_inner`, not `last_area`. The PTY grid is sized from the
        // pane's INNER rect — `Borders::ALL` takes a cell on each side — so
        // recording the outer size declares a terminal two columns wider and
        // two rows taller than the content was wrapped for. The player then
        // wraps every long line at the wrong column.
        let (cols, rows) = (term.last_inner.width, term.last_inner.height);
        if cols == 0 || rows == 0 {
            // A zero-sized header makes a cast every player refuses, and the
            // error names the file rather than the moment it was started.
            self.status = String::from("The terminal has no size yet — try once it has drawn");
            return;
        }
        // A per-recording name, not a per-process one: two recordings in
        // one session would otherwise write the same file, and the second
        // would silently destroy the first — which the user only discovers
        // when they go to share the earlier one.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = self
            .workspace_root()
            .join(format!("croft-{}-{stamp}.cast", std::process::id()));
        let rec = crate::asciicast::Recorder::new(cols, rows);
        let header = rec.header(Some("croft session"));
        match std::fs::File::create(&path) {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{header}") {
                    self.status = format!("Could not write {}: {e}", path.display());
                    return;
                }
                self.recorded_size = (cols, rows);
                self.recording = Some((rec, f, std::time::Instant::now()));
                self.status = format!("\u{25cf} REC to {}", path.display());
            }
            Err(e) => self.status = format!("Could not create {}: {e}", path.display()),
        }
    }

    /// Append terminal output to the open recording, if there is one (#356).
    ///
    /// Times are measured from the recording's start rather than from the
    /// process's, because a cast's first event should be at ~0 — a player
    /// given a first timestamp of 4000 seconds sits on a blank screen for
    /// over an hour before anything happens.
    pub(crate) fn record_terminal_output(&mut self, data: &str) {
        use std::io::Write as _;
        let Some((rec, file, started)) = self.recording.as_mut() else {
            return;
        };
        let line = rec.line(&crate::asciicast::Event::Output {
            at: started.elapsed().as_secs_f64(),
            data: data.to_string(),
        });
        // A write failure STOPS the recording rather than being ignored: a
        // half-written cast that keeps growing is worse than one that ends,
        // and the user needs to know the file they are about to share is
        // truncated.
        if writeln!(file, "{line}").is_err() {
            self.recording = None;
            self.status = String::from("Recording stopped: could not write to the cast file");
        }
    }

    /// Cmd+K D: snapshot the active pane's scrollback + screen. A pane that
    /// showed colour is written, colours as SGR, to a private file under the
    /// scrollback dir and opened as a pinned rendered-log tab (#257); a pane
    /// with no colour goes into the scratch editor tab it always had. Read
    /// from one grid pass (`grid_lines_ansi`, the plain twin of which is the
    /// text the find bar searches); the blank live-screen tail is trimmed so
    /// the buffer ends at the last real row.
    pub(super) fn open_scrollback_in_editor(&mut self) {
        // Plain and coloured rows come from ONE grid read, so a redact rule
        // is applied to the row it matches and never to a neighbour that
        // shifted in while a second read was taken.
        let rows_both = self.terminal().grid_lines_ansi_wrapped();
        let last = rows_both
            .iter()
            .rposition(|(plain, _, _)| !plain.trim().is_empty())
            .map_or(0, |i| i + 1);
        let pane = self.terminal().label();
        let pane = if pane.is_empty() { "terminal" } else { pane };
        let label = format!("{pane} scrollback");
        // Every redact rule applies here, copy-mode or not: this buffer
        // leaves the pane as bytes (#360). Masking works on the plain text;
        // a row the rules touch keeps its masked plain form and gives up its
        // colour, since a mask spliced between escapes could break them.
        let mut plain_rows: Vec<String> = Vec::with_capacity(last);
        let mut rows: Vec<String> = Vec::with_capacity(last);
        let mut fallback_reason: Option<String> = None;
        // Wrapped rows are masked as the logical line they form, so a secret
        // cut by a soft wrap is masked on both rows.
        let plains: Vec<String> = rows_both[..last].iter().map(|r| r.0.clone()).collect();
        let wraps: Vec<bool> = rows_both[..last].iter().map(|r| r.2).collect();
        let masked_rows = crate::triggers::mask_rows(&plains, &wraps, &self.triggers);
        for ((plain, ansi, _), masked) in rows_both[..last].iter().zip(masked_rows) {
            rows.push(if masked == *plain {
                ansi.clone()
            } else {
                masked.clone()
            });
            plain_rows.push(masked);
        }
        // A pane that showed colour lands in the rendered log view (#257),
        // which indexes from disk, so the rows go to a real file under the
        // scrollback dir, one per pane, overwritten on each dump. The gate
        // uses the open path's own sniff, so the file can never open as an
        // editable buffer of raw escapes. A pane with no colour keeps the
        // scratch buffer it always had.
        if scrollback_opens_as_log(&rows) {
            // Keyed on the pane's stable uid as well as its label: two
            // unnamed shells both read `terminal`, and two labels can
            // sanitise to one name, and neither may overwrite the other's
            // dump or close its tab.
            // The uid is a per-process counter that restarts at 1, so two
            // croft processes would share a name without the pid, as the
            // session handoff file learned before this one.
            let file = self.scrollback_dir.join(format!(
                "{}-{}-{}-scrollback.log",
                pane.chars()
                    .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    })
                    .collect::<String>(),
                std::process::id(),
                self.terminal().uid()
            ));
            // A tab from an earlier dump holds a byte index over the OLD
            // file; it is closed so the open below rebuilds the view over the
            // new bytes rather than reading them through the stale index.
            self.close_tabs_with_path_everywhere(&file);
            // Terminal output is written owner-only whatever the umask: the
            // redaction rules are finite, and another local account has no
            // business reading a shell's history.
            let written = write_private(&self.scrollback_dir, &file, &(rows.join("\n") + "\n"));
            // Dumps left by croft processes that have exited are reaped on
            // the way past: nothing will ever overwrite them. Here rather
            // than at startup because this is the one place the dir grows,
            // so its size is bounded at the moment it would grow; the cost
            // is one `read_dir` and one `kill(0)` per stale file, paid by a
            // command that just wrote a scrollback to disk.
            if written.is_ok() {
                reap_dead_scrollback_dumps(&self.scrollback_dir);
            }
            // Pinned, as the scratch buffer was: a later file peek must not
            // replace the dump, and two panes' dumps must not share a slot.
            let opened = written
                .map_err(anyhow::Error::from)
                .and_then(|()| self.editor.open_pinned(&file));
            match opened {
                Ok(()) if self.editor.log.is_some() => {
                    self.focus_pane(Pane::Editor);
                    self.sync_open_file_poll_mtime();
                    self.status = format!("Opened {last} scrollback lines as a rendered log");
                    return;
                }
                Ok(()) => {
                    // The open path decided this was not a log after all:
                    // never leave the user an editable file of raw escapes,
                    // nor the file itself, which nothing will read now.
                    self.close_tabs_with_path_everywhere(&file);
                    let _ = std::fs::remove_file(&file);
                    fallback_reason = Some(String::from(
                        "Rendered scrollback unavailable; opened as text",
                    ));
                }
                Err(e) => {
                    // Fall through to the plain scratch buffer rather than
                    // lose the dump: a full disk still leaves the text. A
                    // partial file, if one was written, serves nobody.
                    let _ = std::fs::remove_file(&file);
                    fallback_reason = Some(format!(
                        "Rendered scrollback unavailable ({e}); opened as text"
                    ));
                }
            }
        }
        let text = plain_rows.join("\n");
        match self.editor.open_text_buffer(Path::new(&label), &text) {
            Ok(()) => {
                self.focus_pane(Pane::Editor);
                self.sync_open_file_poll_mtime();
                // The reason travels in a local, not in `status`: a message
                // left by an earlier action must not be mistaken for one
                // this dump produced.
                self.status = fallback_reason
                    .unwrap_or_else(|| format!("Opened {last} scrollback lines in the editor"));
            }
            Err(e) => self.status = format!("Open scrollback failed: {e}"),
        }
    }

    /// Surface BEL rings and OSC 9 notifications from any pane in the status
    /// bar (VS Code shows a bell on the terminal tab; croft's status line is
    /// its equivalent). Returns true when either arrived so the loop redraws.
    pub(super) fn drain_terminal_bells(&mut self) -> bool {
        self.reap_closed_terminals();
        // Long-command completion notice, the Ghostty/Warp behaviour: only
        // when the pane isn't the focused one (the user watching a pane sees
        // the command end themselves).
        const LONG_COMMAND_NOTIFY: std::time::Duration = std::time::Duration::from_secs(10);
        let mut rang: Option<String> = None;
        let mut notes: Vec<String> = Vec::new();
        // The long-command notice, with the pane it is about.
        let mut finished: Option<(u64, String)> = None;
        let mut trigger_note: Option<String> = None;
        let mut captured: Vec<crate::widgets::captures::CapturedLine> = Vec::new();
        // Finished-command outputs to run through the build matchers after
        // the immutable pane sweep (#119): (pane uid, cwd, command, output).
        let mut build_scans: Vec<(u64, Option<PathBuf>, String, String)> = Vec::new();
        // Watch-matcher batches published mid-run (#252): (pane uid, cwd,
        // the cycle's diagnostics — empty clears the pane's entries).
        let mut watch_installs: Vec<(u64, Option<PathBuf>, Vec<crate::build_matchers::BuildDiag>)> =
            Vec::new();
        // A preLaunchTask completing settles the parked debug launch (#250);
        // recorded here and acted on after the sweep (launching mutates self).
        let mut settled_launch: Option<Option<i32>> = None;
        // A watch preLaunchTask's first build ended, with this many errors.
        let mut settled_watch: Option<usize> = None;
        // The newest OSC 52 copy any pane's program made this tick (#678).
        let mut copied: Option<String> = None;
        // A task's or fenced block's command ended (#856): (pane uid, exit,
        // duration), settled against its "Running …" status after the sweep.
        let mut runs_ended: Vec<(u64, Option<i32>, std::time::Duration)> = Vec::new();
        for t in &self.terminals {
            if let Some(text) = t.take_clipboard_store() {
                copied = Some(text);
            }
            // Keep every pane on the current trigger set, wherever it was
            // created (a ptr-eq no-op when already current).
            t.set_triggers(self.triggers.clone());
            t.set_watch_set(self.watch_set.clone());
            for (cwd, batch) in t.drain_watch_batches() {
                // A watch task's first build settles the launch parked on
                // it (#1541): it will never exit to do so.
                if settled_watch.is_none()
                    && let Some(p) = self.pending_debug_launch.as_ref()
                    && p.watch
                    && p.pane == t.uid()
                {
                    let errors = batch
                        .iter()
                        .filter(|d| d.severity == crate::lsp::manager::DiagnosticSeverity::Error)
                        .count();
                    settled_watch = Some(errors);
                }
                watch_installs.push((t.uid(), cwd, batch));
            }
            for h in t.drain_trigger_hits() {
                match h.action {
                    crate::triggers::TriggerAction::Capture => {
                        captured.push(crate::widgets::captures::CapturedLine {
                            pane: t.label().to_string(),
                            shell_pid: t.shell_pid(),
                            message: h.message,
                            line: h.line,
                            anchor: h.anchor,
                        });
                    }
                    crate::triggers::TriggerAction::Bell => {
                        trigger_note =
                            Some(format!("Trigger bell in {}: {}", t.label(), h.message));
                    }
                    _ => {
                        trigger_note = Some(format!("Trigger in {}: {}", t.label(), h.message));
                    }
                }
            }
            if t.take_bell() {
                rang = Some(t.label().to_string());
            }
            for msg in t.drain_notifications() {
                let cwd = t.local_shell_cwd();
                self.notifier.emit(
                    crate::notifications::Event::Osc9 {
                        pane: t.label().to_string(),
                        message: msg.clone(),
                    },
                    self.root_for_path(cwd.as_deref()),
                    &remote_host_label(),
                );
                notes.push(msg);
            }
            // Always drain so completions never pile up unseen.
            for f in t.drain_finished_commands() {
                // Empty output included (#1488): see the scan loop below.
                build_scans.push((t.uid(), f.cwd.clone(), f.cmd.clone(), f.output.clone()));
                // Durable command history: every finished command with a
                // known text is recorded (cwd, exit, duration, timestamp)
                // for the Ctrl+Shift+R cross-session search.
                if !f.cmd.trim().is_empty() {
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    self.command_history
                        .append(crate::command_history::HistoryEntry {
                            // The durable history is on disk and cross-
                            // session: a typed `export KEY=…` is masked
                            // like any other persisted text (#360).
                            cmd: crate::triggers::mask_text(f.cmd.trim(), &self.triggers, false),
                            cwd: f
                                .cwd
                                .as_deref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default(),
                            host: f.host.clone().unwrap_or_default(),
                            exit: f.exit,
                            dur_ms: f.dur.as_millis() as u64,
                            ts,
                        });
                }
                if let Some(p) = self.pending_debug_launch.as_ref()
                    && finished_settles_pending(t.uid(), &f.cmd, p.pane, &p.command)
                {
                    settled_launch = Some(f.exit);
                }
                // The first command the pane finishes after the run was
                // typed is the run's, as for a block's output capture.
                let awaited = self
                    .pane_run_statuses
                    .iter()
                    .any(|(uid, _)| *uid == t.uid());
                if awaited {
                    runs_ended.push((t.uid(), f.exit, f.dur));
                }
                if t.focused {
                    continue;
                }
                // The sinks decide by their own threshold (#358); the
                // status-bar notice keeps the fixed one below.
                self.notifier.emit(
                    crate::notifications::Event::CommandFinished {
                        pane: t.label().to_string(),
                        cmd: crate::triggers::mask_text(f.cmd.trim(), &self.triggers, false),
                        exit: f.exit,
                        dur: f.dur,
                        cwd: f.cwd.clone(),
                        host: f.host.clone(),
                    },
                    self.root_for_path(f.cwd.as_deref()),
                    &remote_host_label(),
                );
                if f.dur < LONG_COMMAND_NOTIFY {
                    continue;
                }
                let code = f.exit.map_or_else(|| String::from("?"), |c| c.to_string());
                finished = Some((
                    t.uid(),
                    format!(
                        "Command in {} finished: exit {code} in {}",
                        t.label(),
                        crate::widgets::terminal::human_duration(f.dur)
                    ),
                ));
            }
        }
        let mut run_settled = false;
        for (pane, exit, dur) in runs_ended {
            let Some(i) = self
                .pane_run_statuses
                .iter()
                .position(|(uid, _)| *uid == pane)
            else {
                continue; // a second command the same sweep saw end
            };
            let (_, run) = self.pane_run_statuses.remove(i);
            if run.settle(&mut self.status, |label, _| {
                pane_run_outcome(label, exit, dur)
            }) {
                run_settled = true;
                // The outcome says what the long-command notice would, by
                // the run's own name. Unsettled (a newer status showing),
                // the notice still comes.
                if finished.as_ref().is_some_and(|(uid, _)| *uid == pane) {
                    finished = None;
                }
            }
        }
        // A shell that exited ends the run it was awaited for (#912).
        run_settled |= self.settle_exited_terminals();
        // A pane that closed will never report its run's end.
        let terminals = &self.terminals;
        self.pane_run_statuses
            .retain(|(uid, _)| terminals.iter().any(|t| t.uid() == *uid));
        if let Some(exit) = settled_launch
            && let Some(pending) = self.pending_debug_launch.take()
        {
            self.resume_pending_launch(exit, pending.step);
        } else if let Some(errors) = settled_watch
            && let Some(pending) = self.pending_debug_launch.take()
        {
            if errors == 0 {
                self.resume_pending_launch(Some(0), pending.step);
            } else {
                // The watcher keeps running: fixing the errors rebuilds,
                // and F5 then launches against a clean build.
                self.debug_error(format!(
                    "preLaunchTask build finished with {errors} error{} — debug launch aborted (PROBLEMS lists them; the task keeps watching)",
                    if errors == 1 { "" } else { "s" }
                ));
            }
        }
        // Liveness fallback for a still-parked launch: the FinishedCommand
        // match depends on OSC 133 marks, and a pane without integration (or
        // one the user closed) would otherwise park the launch forever.
        let mut pending_aborted = false;
        if settled_launch.is_none()
            && let Some(p) = self.pending_debug_launch.as_ref()
        {
            let pane = self.terminals.iter().find(|t| t.uid() == p.pane);
            if let Some(reason) = pending_abort_reason(
                pane.is_some(),
                pane.is_some_and(|t| t.foreground_is_shell()),
                p.started.elapsed(),
            ) {
                self.pending_debug_launch = None;
                self.debug_error(format!(
                    "preLaunchTask abandoned: {reason} — debug launch aborted"
                ));
                pending_aborted = true;
            }
        }
        let mut build_changed = false;
        // Watch publishes first: a batch replaces the pane's entries
        // wholesale, and marks the pane so the watcher's own eventual
        // FinishedCommand rescan cannot resurrect stale cycles.
        // A batch has no command line to read `-ftabstop=` from, so its
        // tabs count at the default stop.
        for (pane, cwd, diags) in watch_installs {
            self.watch_published_panes.insert(pane);
            build_changed |= self.install_build_diags(
                pane,
                cwd.as_deref(),
                diags,
                crate::build_matchers::DEFAULT_TABSTOP,
            );
        }
        for (pane, cwd, cmd, output) in build_scans {
            // A command that printed nothing (`cd`, `clear`) says nothing
            // about the build, so the pane's problems stay. It still ends
            // the pane's command, so it uses up a watcher's one-shot skip:
            // a watcher that clears the screen erases its own output, and
            // with the skip left armed the next build was the one skipped,
            // leaving the watcher's stale problems in place (#1488).
            if output.is_empty() {
                self.watch_published_panes.remove(&pane);
                continue;
            }
            build_changed |= self.apply_build_scan(pane, cwd.as_deref(), &cmd, &output);
        }
        // Captures collect silently (iTerm2's model: the panel is the
        // surface, not the status bar), but still trigger a redraw.
        let had_captures = !captured.is_empty()
            || build_changed
            || settled_launch.is_some()
            || settled_watch.is_some()
            || pending_aborted;
        for c in captured {
            self.captures.push(c);
        }
        // A program in a pane copied through OSC 52 (Claude Code, tmux,
        // nvim): send it where a paste comes from, exactly as croft's own
        // selection copy does, so a remote session copies to the user's
        // machine and not to a box with no clipboard (#678). An empty store
        // is a program clearing the clipboard at startup, not a copy the
        // user made, so it never wipes what they copied.
        let copied = copied.filter(|text| !text.is_empty());
        if let Some(text) = &copied {
            self.deliver_copied_text(text);
        }
        if let Some(msg) = trigger_note {
            self.status = msg;
            return true;
        }
        if had_captures {
            return true;
        }
        if let Some(msg) = notes.pop() {
            self.status = format!("Terminal notification: {msg}");
            return true;
        }
        if let Some((_, msg)) = finished {
            self.status = msg;
            return true;
        }
        let Some(label) = rang else {
            return copied.is_some() || run_settled;
        };
        self.status = format!("Bell in terminal: {label}");
        true
    }

    /// Act on panes whose shell has exited (#912). A task or fenced block
    /// typed into the shell ended with it (`make || exit 1`): the exit code
    /// is the run's outcome, and the pane stays so its output can be read.
    /// Any other shell pane closes, as VS Code closes a terminal whose shell
    /// exits, except the last, which waits for a key to start a new shell.
    /// A program pane keeps its `[Process exited]` footer, as before.
    /// Whether any pane's shell exited.
    pub(super) fn settle_exited_terminals(&mut self) -> bool {
        let exited: Vec<(u64, Option<i32>)> = self
            .terminals
            .iter_mut()
            .filter_map(|t| Some((t.uid(), t.poll_exit()?)))
            .collect();
        for &(uid, code) in &exited {
            if let Some(i) = self.pane_run_statuses.iter().position(|(u, _)| *u == uid) {
                let (_, run) = self.pane_run_statuses.remove(i);
                run.settle(&mut self.status, |label, dur| {
                    pane_run_outcome(label, code, dur)
                });
                continue;
            }
            let Some(idx) = self.terminals.iter().position(|t| t.uid() == uid) else {
                continue;
            };
            if self.terminals[idx].is_run_pane() {
                continue;
            }
            let mut exited = match self.terminals[idx].label() {
                "" => String::from("Terminal exited"),
                label => format!("Terminal {label} exited"),
            };
            if let Some(c) = code.filter(|&c| c != 0) {
                exited.push_str(&format!(" with code {c}"));
            }
            if self.close_terminal_at(idx) {
                // Not parked for undo-close: there is no shell to bring back.
                self.closed_terminals.pop();
                self.terminal_status(exited);
            } else {
                self.status = format!("{exited}; press a key in it to start a new shell");
            }
        }
        !exited.is_empty()
    }

    /// The workspace root a path belongs to, for a notification's title and
    /// deep link (#358): in a multi-root session an event from a pane in a
    /// secondary root must name that root, not the primary. The primary is
    /// the fallback for a path outside every root, or no path at all.
    pub(super) fn root_for_path(&self, path: Option<&Path>) -> &Path {
        path.and_then(|p| self.roots.iter().find(|r| p.starts_with(r)))
            .unwrap_or_else(|| self.roots.primary())
    }

    /// Refresh each pane's auto-label from its live foreground process. The
    /// `tcgetpgrp` reads are cheap and stay on the loop; the `sysinfo` name
    /// lookups (the slow part) run on a one-shot background thread, keyed by
    /// shell pid so results land on the right pane even if panes moved. Returns
    /// whether any label changed (for redraw).
    pub(super) fn refresh_terminal_labels(&mut self) -> bool {
        let mut changed = false;
        // Apply whatever the last background lookup resolved.
        if let Some(samples) = self.ssh_rx.try_iter().last() {
            self.apply_ssh_samples(samples);
        }
        if let Some(labels) = self.label_rx.try_iter().last() {
            for (shell_pid, name, _) in &labels {
                if let Some(t) = self
                    .terminals
                    .iter_mut()
                    .find(|t| t.shell_pid() == Some(*shell_pid))
                    && t.label() != name
                {
                    t.set_auto_label(name.clone());
                    changed = true;
                }
            }
            let samples: Vec<(i32, String)> = labels
                .iter()
                .map(|(shell_pid, name, script)| {
                    let who = self.agents.sample_name(name, script.as_deref());
                    (*shell_pid, who.to_string())
                })
                .collect();
            changed |= self.apply_agent_samples(&samples, crate::agents::QUIET_AFTER);
        }
        if self.last_label_refresh.elapsed() >= TERMINAL_LABEL_INTERVAL
            && !self
                .label_inflight
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.last_label_refresh = std::time::Instant::now();
            // (shell_pid, foreground_pid) gathered cheaply on the loop.
            let targets: Vec<(i32, i32)> = self
                .terminals
                .iter()
                .filter_map(|t| match (t.shell_pid(), t.foreground_pid()) {
                    (Some(sp), Some(fg)) => Some((sp, fg)),
                    _ => None,
                })
                .collect();
            if targets.is_empty() {
                // No pane has a foreground process to sample: any seated
                // agent is gone, and only an empty sample says so.
                changed |= self.apply_agent_samples(&[], crate::agents::QUIET_AFTER);
            }
            if !targets.is_empty() {
                let tx = self.label_tx.clone();
                let ssh_tx = self.ssh_tx.clone();
                let sample_ssh = !self.remote_offer_disabled;
                let inflight = self.label_inflight.clone();
                inflight.store(true, std::sync::atomic::Ordering::Relaxed);
                std::thread::spawn(move || {
                    let named: Vec<(i32, i32, String)> = targets
                        .into_iter()
                        .filter_map(|(sp, fg)| {
                            crate::widgets::terminal::process_name(fg).map(|n| (sp, fg, n))
                        })
                        .collect();
                    // The ssh-pane offer (#364) rides this thread: only a
                    // pane whose foreground IS ssh pays the argv read (a
                    // whole-process-table enumeration), and the config
                    // parse runs once per pass and only when one does. The
                    // verdicts themselves are `remote::ssh_pane_samples`,
                    // kept pure so the gate is testable without a pane.
                    if sample_ssh {
                        let _ = ssh_tx.send(crate::remote::ssh_pane_samples(
                            &named,
                            crate::remote::discover_ssh_targets,
                            crate::widgets::terminal::process_cmdline,
                        ));
                    }
                    // Only a script runtime pays the argv read (#1380): its
                    // name is `node` whatever agent it runs.
                    let resolved: Vec<(i32, String, Option<String>)> = named
                        .into_iter()
                        .map(|(sp, fg, n)| {
                            let script = crate::agents::is_script_runtime(&n)
                                .then(|| crate::widgets::terminal::process_cmdline(fg))
                                .flatten()
                                .and_then(|argv| crate::agents::script_name(&argv));
                            (sp, n, script)
                        })
                        .collect();
                    let _ = tx.send(resolved);
                    inflight.store(false, std::sync::atomic::Ordering::Relaxed);
                });
            }
        }
        changed
    }
}

/// The text terminal find searches: one string per logical line (#1275).
pub(super) fn find_texts(lines: Vec<crate::widgets::terminal::FindLine>) -> Vec<String> {
    lines.into_iter().map(|l| l.text).collect()
}

/// Create `dir` (0700) if needed and write `contents` to `file` (0600). On
/// non-unix hosts the modes do not exist and the plain write is used.
pub(super) fn write_private(dir: &Path, file: &Path, contents: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        // `recursive` also creates any missing parent (`~/.cache/croft`
        // itself on a first run) at 0700. Its other tenants are `sessions/`
        // (session.rs), `tmp/` (pdf.rs) and the MCP registry cache
        // (registry_index.rs), every one of them private per-user state
        // that gains nothing from a wider parent.
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        // A pre-existing dir is tightened, a link is refused: `create` adopts
        // an existing symlink to a dir, `symlink_metadata` reports the link's
        // own mode (always loose), and `set_permissions` follows the link, so
        // without this check a scrollback dir pointed at any dir the user
        // owns would silently re-mode that dir on the first dump.
        let meta = std::fs::symlink_metadata(dir)?;
        if meta.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "{} is a symlink; refusing to write through it",
                    dir.display()
                ),
            ));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        // The same for the file: `create(true).truncate(true)` follows a link
        // planted where the dump goes, and the chmod below would re-mode its
        // target after the truncate had emptied it. The dir is 0700, so the
        // planter is this user's own tooling or a restored backup, and the
        // answer is still a refusal rather than a destroyed file.
        if let Ok(fmeta) = std::fs::symlink_metadata(file)
            && fmeta.file_type().is_symlink()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "{} is a symlink; refusing to write through it",
                    file.display()
                ),
            ));
        }
        // `O_NOFOLLOW` makes the open itself refuse a link, so a link swapped
        // in between the check above and this open cannot be followed either
        // (the same pair config_layers.rs uses); the check above only buys
        // the clearer error. `mode` applies on creation alone, hence the
        // chmod below for a file left by an earlier dump.
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        opts.custom_flags(libc::O_NOFOLLOW);
        let mut f = opts.open(file)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        f.write_all(contents.as_bytes())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)?;
        std::fs::write(file, contents)
    }
}

/// Remove the dumps under `dir` whose croft process is gone. A dump is named
/// `<pane>-<pid>-<uid>-scrollback.log` and is overwritten only by the process
/// that wrote it, so every exited croft would otherwise leave its dumps
/// behind for good, each the size of a pane's scrollback; the other cache
/// dirs here all bound themselves the same way. Anything not shaped like a
/// dump is left alone, and so is every live process's file.
pub(super) fn reap_dead_scrollback_dumps(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let me = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(stem) = name
            .to_str()
            .and_then(|n| n.strip_suffix("-scrollback.log"))
        else {
            continue;
        };
        // `<pane>-<pid>-<uid>`, read from the right since a pane label may
        // itself contain dashes.
        let mut parts = stem.rsplitn(3, '-');
        let (Some(_uid), Some(pid), Some(_pane)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        if pid != me && !process_is_alive(pid) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Whether a scrollback dump goes to the rendered log view or to the plain
/// scratch buffer (#257). Must agree with the open path's own sniff, which
/// reads only the first 8 KiB of the file: a dump this says is a log but the
/// sniff says is text would open as an editable file full of raw escapes.
pub(super) fn scrollback_opens_as_log(rows: &[String]) -> bool {
    // The same 8 KiB the file sniff reads, over the same bytes it will read.
    let mut prefix = String::new();
    for row in rows {
        if prefix.len() >= 8192 {
            break;
        }
        prefix.push_str(row);
        prefix.push('\n');
    }
    let cut = prefix.floor_char_boundary(prefix.len().min(8192));
    crate::ansi_text::looks_like_ansi(&prefix[..cut])
}

/// Fold a mouse event's Shift/Alt/Ctrl state into the form `report_mouse`
/// wants, so a forwarded wheel notch or left-button event carries the same
/// modifiers the child would have seen natively.
pub(super) fn mouse_mods(m: &MouseEvent) -> MouseMods {
    MouseMods {
        shift: m.modifiers.contains(KeyModifiers::SHIFT),
        alt: m.modifiers.contains(KeyModifiers::ALT),
        ctrl: m.modifiers.contains(KeyModifiers::CONTROL),
    }
}

pub(super) const TERMINAL_ADD_LABEL: &str = " + ";

pub(super) const TERMINAL_CLOSE_LABEL: &str = " - ";

/// Codicon chevron-down, matching the OUTPUT dropdown caret. Opens the terminal
/// profile picker (VS Code's `∨` beside `+`).
pub(super) const TERMINAL_PROFILE_LABEL: &str = " \u{eab4} ";

/// Codicon screen-full (Nerd Fonts `cod-screen_full` = U+EB4C), on the panel's
/// tab row: maximize the active terminal across the panel, parking the other
/// panes in the right rail.
pub(super) const TERMINAL_MAX_LABEL: &str = " \u{eb4c} ";

/// Codicon screen-normal (Nerd Fonts `cod-screen_normal` = U+EB4D): restore
/// the even split. Swapped in for the `⛶` while a pane is maximized.
pub(super) const TERMINAL_RESTORE_LABEL: &str = " \u{eb4d} ";

/// Codicon chevron-up (U+EAB7), VS Code's "Maximize Panel Size": grow the
/// panel over the editor, as `Ctrl+Shift+J` does (#1980).
pub(super) const PANEL_MAXIMIZE_LABEL: &str = " \u{eab7} ";

/// Codicon chevron-down (U+EAB4), VS Code's "Restore Panel Size". Swapped in
/// while the panel is maximized.
pub(super) const PANEL_RESTORE_LABEL: &str = " \u{eab4} ";

/// Codicon chevron-left (Nerd Fonts `cod-chevron_left` = U+EAB5): fold this
/// pane to a strip and hand its width to the panes still expanded (#313).
///
/// Its own button rather than a new meaning for `-`: `-` closes, and it goes
/// on closing. A control that destroys on one build and folds on the next is
/// the kind of change muscle memory finds out the hard way.
pub(super) const TERMINAL_COLLAPSE_LABEL: &str = " \u{eab5} ";

/// Codicon chevron-right (U+EAB6), on the header row of a collapsed pane's
/// strip: the way back out. It points the opposite way to the button that put
/// the pane there, which is the whole of the affordance in one column.
pub(super) const TERMINAL_EXPAND_GLYPH: char = '\u{eab6}';

/// Stands in for a character too wide for the one column a strip owns. A
/// CJK glyph or an emoji is two cells and would paint over the pane next
/// door; dropping it outright left a pane named entirely in them with a
/// blank strip, which is the one thing the strip exists to prevent.
pub(super) const TERMINAL_STRIP_WIDE_STANDIN: char = '\u{b7}';

/// A collapsed pane's entire width: one column, carrying the expand chevron
/// and the pane's name set vertically.
pub(super) const TERMINAL_STRIP_W: u16 = 1;

/// Codicon terminal (Nerd Fonts `cod-terminal` = U+EA85), leading each
/// maximize-rail row the way VS Code's terminal tabs list does.
/// Cells the pane's header buttons occupy, measured in from its right edge:
/// `‹`, `-`, `∨` and `+` at three each, plus the one blank column right of
/// `+`. The `⛶` that sat left of `-` moved to the panel's tab row (#1980). The name pill paints after the buttons and would win the shared
/// cells, so it is clipped to the width left of this strip.
/// `a_long_pane_label_never_overpaints_the_header_buttons` pins it, so adding
/// another button fails a test rather than quietly erasing a glyph again -
/// which is how the collapse button (#313) was caught needing this widened
/// from 13 instead of silently sharing the pill's cells.
pub(super) const TERMINAL_BUTTON_STRIP_W: u16 = 13;

/// Width constraints for the terminal row, given which panes are collapsed
/// (#313).
///
/// A collapsed pane keeps a one-column strip; the panes still expanded divide
/// everything left over. Split out as a pure function of the flags because
/// the apportioning is the whole of the layout risk here, and this way it is
/// asserted directly - a constraint vector measured against expected widths -
/// rather than only through a rendered frame.
///
/// `Fill` rather than `Ratio` for the expanded panes: `Ratio(1, k)` is a
/// share of the WHOLE row, so mixing it with the strips' `Length(1)` asks the
/// solver for more columns than exist and it settles that overflow by shaving
/// the panes. `Fill` divides what the strips leave, which is the arithmetic
/// actually wanted.
///
/// With nothing collapsed every pane is `Fill(1)`, which apportions the row
/// evenly - the same split as before the feature existed, so the common case
/// pays nothing for it.
///
/// Every pane collapsed is unreachable through the UI ([`App::toggle_terminal_collapse`]
/// and [`App::close_terminal_at`] both keep one expanded), but it is answered
/// here rather than left to divide by zero.
pub(super) fn terminal_pane_constraints(collapsed: &[bool]) -> Vec<Constraint> {
    let any_expanded = collapsed.iter().any(|c| !c);
    collapsed
        .iter()
        .map(|&c| {
            if c && any_expanded {
                Constraint::Length(TERMINAL_STRIP_W)
            } else {
                Constraint::Fill(1)
            }
        })
        .collect()
}

pub(super) const TERMINAL_RAIL_ICON: char = '\u{ea85}';

/// How many label characters a rail row shows. Fixed and deliberately tiny:
/// the rail is a switcher, not a directory, so four letters identify a pane.
pub(super) const TERMINAL_RAIL_LABEL_W: u16 = 4;

/// The rail's fixed total width: leading space + icon + gap + the 4 label
/// characters. The rail never grows past this, whatever the names are.
pub(super) const TERMINAL_RAIL_W: u16 = 3 + TERMINAL_RAIL_LABEL_W;

/// The user's `triggers.json`, with the built-in secret redactions in front
/// of it when redaction is on (#360).
pub(super) fn load_trigger_set(
    redact_secrets: bool,
) -> std::sync::Arc<crate::triggers::TriggerSet> {
    let set = crate::triggers::TriggerSet::load(&crate::triggers::triggers_path());
    std::sync::Arc::new(if redact_secrets {
        set.with_builtin_redactions()
    } else {
        set
    })
}

/// Paint the terminal pane's header buttons on its top border and return
/// their hit-test rectangles. Any of them is `None` when the pane is too
/// narrow or short for the label to fit, and the three that act on ONE pane
/// among several (`-`, `⛶`, `‹`) are absent when a lone terminal is open and
/// there is nothing to drop, maximize against, or fold away from.
///
/// Right to left: `+`, `∨`, `-`, `⛶`, `‹`. The collapse button (#313) went on
/// the far LEFT deliberately: every button already there keeps the exact
/// column it had, so a user reaching for `-` by position still lands on `-`
/// and not on a gesture that merely looks similar.
pub(super) fn paint_terminal_pane_buttons(
    frame: &mut ratatui::Frame,
    area: Rect,
    show_close_button: bool,
    pane_maximized: bool,
    theme: crate::theme::Theme,
    pointer: Option<(u16, u16)>,
) -> TerminalPaneButtons {
    let add_w = TERMINAL_ADD_LABEL.chars().count() as u16;
    let caret_w = TERMINAL_PROFILE_LABEL.chars().count() as u16;
    let close_w = TERMINAL_CLOSE_LABEL.chars().count() as u16;
    let fold_w = TERMINAL_COLLAPSE_LABEL.chars().count() as u16;
    let mut out = TerminalPaneButtons::default();
    if area.height == 0 {
        return out;
    }
    let y = area.y;
    // Styling is the shared header-action look (see `header_pill::action_style`):
    // quiet teal icons under Black with a teal hover pill, navy chips under
    // Croft Dark. The Explorer `⋯` button draws from the same source of truth.
    let style_at = |x: u16, w: u16| -> Style {
        let hovered = pointer.is_some_and(|(px, py)| py == y && px >= x && px < x + w);
        crate::widgets::header_pill::action_style(theme, hovered)
    };
    // Right to left: [+] at the edge, the profile caret just left of it, then
    // [-]. The caret mirrors VS Code's `∨` next to `+`.
    if area.width >= add_w + 2 {
        let x = area.x + area.width - add_w - 1;
        frame
            .buffer_mut()
            .set_string(x, y, TERMINAL_ADD_LABEL, style_at(x, add_w));
        out.add = Some(Rect {
            x,
            y,
            width: add_w,
            height: 1,
        });
    }
    if area.width >= add_w + caret_w + 2 {
        let x = area.x + area.width - add_w - caret_w - 1;
        frame
            .buffer_mut()
            .set_string(x, y, TERMINAL_PROFILE_LABEL, style_at(x, caret_w));
        out.profile = Some(Rect {
            x,
            y,
            width: caret_w,
            height: 1,
        });
    }
    if show_close_button && area.width >= add_w + caret_w + close_w + 2 {
        let x = area.x + area.width - add_w - caret_w - close_w - 1;
        frame
            .buffer_mut()
            .set_string(x, y, TERMINAL_CLOSE_LABEL, style_at(x, close_w));
        out.close = Some(Rect {
            x,
            y,
            width: close_w,
            height: 1,
        });
    }
    // Collapse sits left of `-`, which stays where it was. Maximize already
    // owns the whole panel and ignores the collapse flags, so while it is on
    // the button would be a control that does nothing: it is not painted.
    if show_close_button && !pane_maximized && area.width >= add_w + caret_w + close_w + fold_w + 2
    {
        let x = area.x + area.width - add_w - caret_w - close_w - fold_w - 1;
        frame
            .buffer_mut()
            .set_string(x, y, TERMINAL_COLLAPSE_LABEL, style_at(x, fold_w));
        out.collapse = Some(Rect {
            x,
            y,
            width: fold_w,
            height: 1,
        });
    }
    out
}

/// A paste's lines (#1274), split at `\n`, `\r\n` or a lone `\r` as a shell
/// would take them, without the empty lines a trailing break leaves.
pub(super) fn pasted_lines(payload: &[u8]) -> Vec<Vec<u8>> {
    let mut lines: Vec<Vec<u8>> = vec![Vec::new()];
    let mut bytes = payload.iter().copied().peekable();
    while let Some(b) = bytes.next() {
        match b {
            b'\r' | b'\n' => {
                if b == b'\r' && bytes.peek() == Some(&b'\n') {
                    bytes.next();
                }
                lines.push(Vec::new());
            }
            b => lines.last_mut().expect("never empty").push(b),
        }
    }
    while lines.len() > 1 && lines.last().is_some_and(Vec::is_empty) {
        lines.pop();
    }
    lines
}
