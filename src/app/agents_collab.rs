//! Coding agents in panes, agent and worktree lanes, approving agent edits, the AI
//! navigator, and multiplayer sessions: relay, presence and peer carets.

use super::*;

/// Why the single-host lock was last refused, for announcing each reason
/// once (see `App::pair_lock_denied`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PairLockDenial {
    Busy,
    Io,
}

/// Canonicalise `path` even when it no longer exists.
///
/// `std::fs::canonicalize` stats the final component, so it fails on a path
/// that has been deleted — and a deletion event carries exactly such a path.
/// Resolving the deepest ancestor that still exists and re-attaching the tail
/// gives the same answer for a live path and a usable one for a dead path,
/// which is what lets a removal clear the row it created (#345).
///
/// `None` when nothing on the way to the filesystem root resolves, and also
/// for a path with no file name to strip — a bare root, or one ending in
/// `..` — which cannot be climbed any further.
pub(super) fn canonicalize_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut probe = path.to_path_buf();
    loop {
        if let Ok(canon) = std::fs::canonicalize(&probe) {
            let mut out = canon;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return Some(out);
        }
        // `file_name` is None for a root or `..`; either way there is
        // nothing further to climb.
        tail.push(probe.file_name()?.to_os_string());
        probe = probe.parent()?.to_path_buf();
    }
}

/// What an Agent Lane row's `+n −m` was counted from (#345): the reviewed
/// snapshot, the agent's last write, and the file's mtime on disk.
pub(super) type LaneCountKey = (u64, u64, Option<std::time::SystemTime>);

impl App {
    /// Keep the language server's check in step with the proposal at the
    /// head of the queue (#347): send a new head's text as its file's
    /// content, and when a proposal leaves the head, give the file back:
    /// an open buffer's text is resent, a file no tab has is closed.
    pub fn sync_approval_check(&mut self) {
        let head = self
            .approvals
            .front()
            .map(|p| (p.arrived, p.proposal.path.clone(), p.proposal.after.clone()));
        if let Some(check) = self.approval_check.as_ref()
            && head.as_ref().map(|(arrived, ..)| *arrived) != Some(check.arrived)
        {
            self.end_approval_check();
        }
        let Some((arrived, path, after)) = head else {
            return;
        };
        if self.approval_check.is_some() {
            return;
        }
        let Some(lsp) = self.lsp.as_ref() else {
            return;
        };
        if crate::lsp::Language::from_path(&path).is_none() {
            return;
        }
        let opened = !self.lsp_last_seen.contains_key(&path);
        if opened {
            lsp.open_doc(path.clone(), after);
        } else {
            lsp.change_doc(path.clone(), after);
        }
        self.approval_check = Some(crate::agent_approval::ProposalCheck {
            arrived,
            started: std::time::Instant::now(),
            path,
            opened,
            by_server: std::collections::HashMap::new(),
        });
    }

    pub(super) fn end_approval_check(&mut self) {
        let Some(check) = self.approval_check.take() else {
            return;
        };
        // `sync_lsp` keeps a file open for a tab in any split group (#915).
        let tab_has_it = self.open_buffer_paths().contains(&check.path);
        if tab_has_it && (!check.opened || !self.lsp_last_seen.contains_key(&check.path)) {
            // A seq no tab has makes `sync_lsp` send the buffer's text next
            // tick, over the proposal's. (A tab opened during a check of a
            // file the check had opened needs the same: the server has it
            // open already, with the proposal.)
            self.lsp_last_seen.insert(check.path, u64::MAX);
        } else if check.opened {
            if let Some(lsp) = self.lsp.as_ref() {
                lsp.close_doc(check.path.clone());
            }
            self.lsp_last_seen.remove(&check.path);
        }
    }

    /// Take agent edit proposals off the hook socket and drop the ones
    /// whose hook has already given up, keeping the popup on the head of
    /// the queue.
    pub(super) fn drain_hook_requests(&mut self) -> bool {
        let head = self.approvals.front().map(|p| p.arrived);
        let mut changed = match self.hook_listener.as_ref() {
            Some(listener) => crate::agent_approval::accept_into(listener, &mut self.approvals),
            None => false,
        };
        // Auto-approved edits are answered before anyone is notified.
        changed |= self.apply_auto_approve();
        self.notify_new_approvals();
        let now = std::time::Instant::now();
        let before = self.approvals.len();
        self.approvals.retain(|p| !p.expired(now));
        changed |= self.approvals.len() != before;
        if self.approvals.front().map(|p| p.arrived) != head {
            self.approval_ui = None;
        }
        changed |= self.check_approval_edit();
        if self.approvals.is_empty() {
            self.approval_ui = None;
        } else if self.approval_ui.is_none() && self.approval_edit.is_none() {
            self.approval_ui = Some(crate::agent_approval::ApprovalUi::new(now));
            changed = true;
        }
        self.refresh_approval_target_dirty();
        changed
    }

    /// The unsaved text of `path`'s tab, if croft has it open and dirty in
    /// any editor group. An agent's proposal is computed from disk, so it
    /// knows nothing of these edits (#347).
    pub(super) fn dirty_tab_lines(&self, path: &Path) -> Option<Vec<String>> {
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let want = canon(path);
        let groups = std::iter::once(&self.editor).chain(self.editor_layout.inactive_groups());
        groups
            .flat_map(|g| g.editors.iter())
            .find(|e| e.dirty && e.path.as_deref().is_some_and(|p| canon(p) == want))
            .map(|e| e.lines.clone())
    }

    /// Keep the popup's dirty-target flag current: the tab may be saved or
    /// edited while the proposal waits.
    pub(super) fn refresh_approval_target_dirty(&mut self) {
        let dirty = self
            .approvals
            .front()
            .is_some_and(|p| self.dirty_tab_lines(&p.proposal.path).is_some());
        if let Some(ui) = self.approval_ui.as_mut() {
            ui.target_dirty = dirty;
        }
    }

    /// Tell the notification sinks about each proposal that arrived since
    /// the last pass, once, with its Approve and Deny links (#359).
    pub(super) fn notify_new_approvals(&mut self) {
        for i in 0..self.approvals.len() {
            if self.approvals[i].notified {
                continue;
            }
            self.approvals[i].notified = true;
            let pending = &self.approvals[i];
            let path = &pending.proposal.path;
            let file = path
                .strip_prefix(&self.hook_root)
                .unwrap_or(path)
                .display()
                .to_string();
            let event = crate::notifications::Event::ApprovalPending {
                agent: pending.request.agent.clone(),
                file,
                token: pending.token.clone(),
            };
            self.notifier
                .emit(event, &self.hook_root, &remote_host_label());
        }
    }

    /// Answer "allow" to every queued proposal from the auto-approved agent
    /// while its window lasts (#347), and drop the window once it ends.
    /// Returns true when anything was approved or the window closed.
    pub(super) fn apply_auto_approve(&mut self) -> bool {
        let Some((agent, until)) = self.auto_approve.clone() else {
            return false;
        };
        let now = std::time::Instant::now();
        if now >= until {
            self.auto_approve = None;
            self.status = format!("Stopped auto-approving {agent}: its 10 minutes are up");
            return true;
        }
        let mut approved = Vec::new();
        let mut kept = std::collections::VecDeque::new();
        // A file with unsaved edits in croft is never approved unseen: the
        // agent's edit would land under them. It waits for the popup, which
        // offers the merge (#347).
        let dirty: Vec<bool> = self
            .approvals
            .iter()
            .map(|p| self.dirty_tab_lines(&p.proposal.path).is_some())
            .collect();
        for (p, dirty) in std::mem::take(&mut self.approvals).into_iter().zip(dirty) {
            if p.request.agent == agent && !dirty {
                approved.push(p.proposal.path.display().to_string());
                p.answer(&crate::agent_hook::Decision::Allow);
            } else {
                kept.push_back(p);
            }
        }
        self.approvals = kept;
        if approved.is_empty() {
            return false;
        }
        if self.approvals.is_empty() {
            self.approval_ui = None;
        }
        self.status = format!("Auto-approved {agent}'s edit to {}", approved.join(", "));
        true
    }

    /// Agents: Stop Auto-Approving (#347).
    pub(super) fn stop_auto_approve(&mut self) {
        self.status = match self.auto_approve.take() {
            Some((agent, _)) => format!("Stopped auto-approving {agent}"),
            None => String::from("No agent is being auto-approved"),
        };
    }

    pub(super) fn handle_approval_key(&mut self, key: KeyEvent) {
        let rows = self
            .approvals
            .front()
            .map(|p| crate::agent_approval::diff_rows(&p.proposal).len())
            .unwrap_or(0);
        let Some(ui) = self.approval_ui.as_mut() else {
            return;
        };
        let Some(decision) = ui.key(key, std::time::Instant::now(), rows) else {
            if std::mem::take(&mut ui.edit_requested) {
                self.start_approval_edit();
            }
            return;
        };
        // Approving over unsaved edits would have the agent's write land
        // under them: merge instead, and approve what the merge saves.
        if matches!(decision, crate::agent_hook::Decision::Allow) && ui.target_dirty {
            ui.approve_all = false;
            self.start_approval_edit();
            return;
        }
        let approve_all = ui.approve_all;
        if let Some(head) = self.approvals.pop_front() {
            if approve_all {
                let agent = head.request.agent.clone();
                self.auto_approve = Some((
                    agent.clone(),
                    std::time::Instant::now() + crate::agent_approval::AUTO_APPROVE_FOR,
                ));
                self.status = format!(
                    "Approving every edit from {agent} for 10 minutes (Agents: Stop Auto-Approving ends it)"
                );
            }
            head.answer(&decision);
        }
        // What was already queued from that agent goes through too.
        if approve_all {
            self.apply_auto_approve();
        }
        self.approval_ui = (!self.approvals.is_empty())
            .then(|| crate::agent_approval::ApprovalUi::new(std::time::Instant::now()));
    }

    /// `e` in the approval popup (#347): open the head proposal's text as a
    /// scratch file named like the real one (so it gets its language's
    /// highlighting and server), with the popup put away. Saving it
    /// approves the saved text; closing it unsaved brings the popup back.
    pub(super) fn start_approval_edit(&mut self) {
        let Some(head) = self.approvals.front() else {
            return;
        };
        // Refused up front rather than after the user has done the editing.
        if let Err(why) = crate::agent_approval::edited_input(
            &head.request.tool,
            &head.request.input,
            head.proposal.before.as_deref(),
            &head.proposal.after,
        ) {
            self.status = why;
            return;
        }
        let name = head
            .proposal
            .path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("proposal.txt"));
        let dir = croft_cache_dir().join("approval-edits").join(&head.token);
        let file = dir.join(name);
        let token = head.token.clone();
        let agent = head.request.agent.clone();
        let written = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&file, &head.proposal.after));
        if let Err(e) = written {
            self.status = format!("Could not open the proposal to edit: {e}");
            return;
        }
        let target = head.proposal.path.clone();
        let base: Vec<String> = head
            .proposal
            .before
            .as_deref()
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        let theirs: Vec<String> = head.proposal.after.lines().map(str::to_string).collect();
        let yours = self.dirty_tab_lines(&target);
        // A tab of its own: `open` on the strip's deref would load the
        // scratch file INTO the current tab, over whatever it held.
        if let Err(e) = self.editor.open_pinned(&file) {
            self.status = format!("Could not open the proposal to edit: {e}");
            return;
        }
        self.approval_ui = None;
        self.approval_edit = Some((token, file));
        self.approval_merge_into = None;
        self.focus_pane(Pane::Editor);
        let Some(yours) = yours else {
            self.status = format!(
                "Editing {agent}'s proposal: save to approve it as edited, close the tab unsaved to go back"
            );
            return;
        };
        // Three-way: the disk text both started from, your unsaved edits
        // as Current, the agent's proposal as Incoming. What is saved is
        // what the agent writes, and your tab takes it too.
        let (mut mv, result) = crate::merge_editor::MergeView::new(base, yours, theirs, false);
        let end = self.editor.lines.len();
        self.editor.splice_result_rows(0, end, result);
        mv.synced_len = self.editor.lines.len();
        mv.synced_seq = self.editor.edit_seq;
        mv.anchor_panes_on(0);
        let n = mv.conflicts.len();
        if let Some(row) = mv.conflicts.first().map(|c| c.result_start) {
            self.editor.cursor_row = row.min(self.editor.lines.len().saturating_sub(1));
            self.editor.cursor_col = 0;
        }
        mv.proposal_from = Some(agent.clone());
        self.editor.merge = Some(mv);
        self.approval_merge_into = Some(target);
        self.status = format!(
            "Merging {agent}'s proposal with your unsaved edits ({n} conflict{}): save to approve the result, close the tab unsaved to go back",
            if n == 1 { "" } else { "s" }
        );
    }

    /// Keep an in-progress proposal edit honest (#347): the proposal gone
    /// (answered elsewhere, or the agent stopped waiting) ends it with a
    /// note; its tab closed unsaved ends it quietly, and the popup returns.
    pub(super) fn check_approval_edit(&mut self) -> bool {
        let Some((token, file)) = self.approval_edit.clone() else {
            return false;
        };
        if !self.approvals.iter().any(|p| p.token == token) {
            self.approval_edit = None;
            self.status = String::from(
                "The agent stopped waiting for that edit, so saving it approves nothing",
            );
            return true;
        }
        let groups = std::iter::once(&self.editor).chain(self.editor_layout.inactive_groups());
        let open = groups
            .flat_map(|g| g.editors.iter())
            .any(|e| e.path.as_deref() == Some(file.as_path()));
        if !open {
            self.approval_edit = None;
            let _ = std::fs::remove_file(&file);
            return true;
        }
        false
    }

    /// A save of the proposal being edited approves it with the saved text
    /// as the tool's new input (#347), and closes the scratch tab.
    pub(super) fn approve_saved_edit(&mut self, saved: &std::path::Path) {
        let Some((token, file)) = self.approval_edit.clone() else {
            return;
        };
        if saved != file {
            return;
        }
        let Some(idx) = self.approvals.iter().position(|p| p.token == token) else {
            self.approval_edit = None;
            self.status = String::from(
                "The agent stopped waiting for that edit, so saving it approves nothing",
            );
            return;
        };
        let unresolved = self
            .editor
            .merge
            .as_ref()
            .filter(|_| self.editor.path.as_deref() == Some(file.as_path()))
            .map_or(0, |mv| mv.unresolved_count());
        if unresolved > 0 {
            self.status = format!(
                "{unresolved} conflict{} left: resolve {} and save again; nothing approved yet",
                if unresolved == 1 { "" } else { "s" },
                if unresolved == 1 { "it" } else { "them" }
            );
            return;
        }
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("Could not read the edited proposal: {e}");
                return;
            }
        };
        let pending = &self.approvals[idx];
        let input = match crate::agent_approval::edited_input(
            &pending.request.tool,
            &pending.request.input,
            pending.proposal.before.as_deref(),
            &text,
        ) {
            Ok(input) => input,
            Err(why) => {
                self.status = why;
                return;
            }
        };
        let Some(pending) = self.approvals.remove(idx) else {
            return;
        };
        let agent = pending.request.agent.clone();
        pending.answer(&crate::agent_hook::Decision::AllowEdited { input });
        self.approval_edit = None;
        if self.editor.path.as_deref() == Some(file.as_path()) {
            self.editor.close_active();
        }
        let _ = std::fs::remove_file(&file);
        self.status = format!("Approved {agent}'s edit as you changed it");
        if let Some(target) = self.approval_merge_into.take() {
            // Your tab takes the merged text, one Undo from your edits, so
            // the agent's write lands on what the buffer already holds.
            let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            let want = canon(&target);
            let lines: Vec<String> = text.lines().map(str::to_string).collect();
            let mut groups = self.editor_layout.inactive_groups_mut();
            groups.push(&mut self.editor);
            for tab in groups
                .into_iter()
                .flat_map(|g| g.editors.iter_mut())
                .filter(|e| e.path.as_deref().is_some_and(|p| canon(p) == want))
            {
                let end = tab.lines.len();
                tab.splice_result_rows(0, end, lines.clone());
            }
            self.status = format!(
                "Approved the merge of your edits and {agent}'s; your tab holds the same text"
            );
        }
        if !self.approvals.is_empty() {
            self.approval_ui = Some(crate::agent_approval::ApprovalUi::new(
                std::time::Instant::now(),
            ));
        }
    }

    pub(super) fn render_approval_popup(&self, frame: &mut ratatui::Frame) {
        let (Some(ui), Some(head)) = (self.approval_ui.as_ref(), self.approvals.front()) else {
            return;
        };
        let area = frame.area();
        crate::widgets::approval_popup::render(
            area,
            frame.buffer_mut(),
            self.theme,
            head,
            ui,
            self.approvals.len(),
            self.workspace_root(),
            self.approval_check
                .as_ref()
                .filter(|c| c.arrived == head.arrived),
        );
    }

    /// "Session: Participants" (Cmd+K A): who is attached to this
    /// multiplayer session, from the host's presence sidecar
    /// (docs/MULTIPLAYER.md). Enter on a row opens the action picker.
    pub(super) fn open_participants_picker(&mut self) {
        if self.session_channel.is_none() {
            self.status = String::from("Not a persistent session (start one with `croft attach`)");
            return;
        }
        let rows: Vec<crate::widgets::list_picker::ListRow> = self
            .session_participants
            .iter()
            .map(participant_row)
            .collect();
        let picker = crate::widgets::list_picker::ListPicker::new(
            crate::widgets::list_picker::ListPurpose::SessionParticipant,
            "Session Participants",
            rows,
        );
        self.open_list_picker(picker, "No participants yet");
    }

    /// Session: Detach (#679): disconnect the client that asked, when only
    /// one client can have (see `sole_detach_target`). The session keeps
    /// running, and the client restores its terminal on the way out.
    pub(super) fn detach_session_client(&mut self) {
        if self.session_channel.is_none() {
            self.status =
                String::from("Not attached to a persistent session: nothing to detach from");
            return;
        }
        // Only a control holder's keys reach croft, so with one control
        // holder (or one client) the invoker is known. With several, the
        // typing attribution can race another writer's keys, so refuse
        // rather than disconnect the wrong client: the chord, caught by the
        // attach client itself, always detaches its own client.
        let Some(id) = crate::session_host::sole_detach_target(&self.session_participants) else {
            self.status = String::from(
                "Several clients can type here: press Cmd+K Shift+Q in the client to leave, or use Session: Participants (Cmd+K A)",
            );
            return;
        };
        let name = self
            .session_participants
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| format!("participant {id}"));
        let Some(channel) = self.session_channel.as_mut() else {
            return;
        };
        self.status = if channel.kick(id) {
            format!("{name} detached; the session keeps running")
        } else {
            String::from("Session host unreachable")
        };
    }

    /// Second-level picker for one participant: grant or revoke write
    /// control, or disconnect them. Safe to expose to every attacher: a
    /// read-only guest's keystrokes are dropped at the session host, so any
    /// key that reaches this UI came from a control-holding client.
    pub(super) fn open_participant_actions(&mut self, id: u64) {
        let Some(p) = self.session_participants.iter().find(|p| p.id == id) else {
            return;
        };
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let control_row = if p.control {
            ListRow {
                id: format!("revoke:{id}"),
                label: String::from("Revoke write control"),
            }
        } else {
            ListRow {
                id: format!("grant:{id}"),
                label: String::from("Grant write control"),
            }
        };
        let rows = vec![
            control_row,
            ListRow {
                id: format!("kick:{id}"),
                label: String::from("Disconnect"),
            },
        ];
        let picker = ListPicker::new(ListPurpose::SessionParticipantAction, p.name.clone(), rows);
        self.open_list_picker(picker, "No actions");
    }

    /// Send a participant action to the session host and report it. The
    /// roster update comes back through the presence poll.
    pub(super) fn run_participant_action(&mut self, verb: ParticipantVerb, id: u64) {
        let name = self
            .session_participants
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| format!("participant {id}"));
        let Some(channel) = self.session_channel.as_mut() else {
            return;
        };
        let ok = match verb {
            ParticipantVerb::Grant => channel.set_control(id, true),
            ParticipantVerb::Revoke => channel.set_control(id, false),
            ParticipantVerb::Kick => channel.kick(id),
        };
        self.status = if ok {
            match verb {
                ParticipantVerb::Grant => format!("Granted write control to {name}"),
                ParticipantVerb::Revoke => format!("Revoked write control from {name}"),
                ParticipantVerb::Kick => format!("Disconnected {name}"),
            }
        } else {
            String::from("Session host unreachable")
        };
    }

    /// Poll the session host's presence sidecar (multiplayer roster): a stat
    /// every 500ms, reparse only when the mtime moves. Surfaces joins and
    /// leaves in the status line and keeps an open participants picker live.
    pub(super) fn poll_session_presence(&mut self) -> bool {
        const INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
        if self.session_channel.is_none() {
            return false;
        }
        if self.last_session_presence_poll.elapsed() < INTERVAL {
            return false;
        }
        // A host swap (#238) tears this privileged channel down while the
        // session lives on under the successor host. The socket and token in
        // the environment are still valid - the successor inherited both -
        // so revival is just authenticating again, throttled by this poll's
        // own interval.
        if self.session_channel.as_ref().is_some_and(|c| c.is_dead())
            && let Some(fresh) = crate::session_host::InnerChannel::from_env()
        {
            self.session_channel = Some(fresh);
        }
        let Some(channel) = self.session_channel.as_ref() else {
            return false;
        };
        self.last_session_presence_poll = std::time::Instant::now();
        // #238: the serve wrapper normally re-execs itself into an updated
        // binary, carrying the session along - the marker appears only when
        // that handoff could not happen (exec failed, no handoff fds), so
        // the host is stuck on the old image and the one recovery is a
        // fresh host, which a detach + reattach of the last participant
        // produces. Checked before the presence-mtime early-return: a
        // roster that never changes must not hide the hint.
        if !self.session_host_stale_seen && channel.stale_marker.exists() {
            self.session_host_stale_seen = true;
            self.status = String::from(
                "Session host runs a pre-update binary; detach and reattach (all participants) to pick up the update",
            );
        }
        let path = channel.presence.clone();
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if mtime == self.session_presence_mtime {
            return false;
        }
        self.session_presence_mtime = mtime;
        let roster = crate::session_host::read_presence(&path).unwrap_or_default();
        // A client older than this session is told here, in croft's own
        // status line, never over its screen (#626, #652). The reference is
        // the newer of this croft and its host: a host swap moves the host
        // past this croft until the updater relaunches it, and a client
        // still on the old binary is then out of date even though it
        // matches us. Raised only
        // when the notice changes (a client resizing churns the roster), and
        // decided before the unchanged-roster return, since a swap can leave
        // the roster identical while the host's version moved.
        let host_version = crate::session_host::read_host_version(&channel.host_version);
        let reference = crate::session_host::session_version(
            env!("CARGO_PKG_VERSION"),
            host_version.as_deref(),
        );
        let stale = crate::session_host::stale_client_notice_on_change(
            self.session_stale_notice.as_deref(),
            &roster,
            reference,
        );
        self.session_stale_notice = crate::session_host::stale_client_notice(&roster, reference);
        if roster == self.session_participants {
            // Unchanged roster: redraw only for a newly raised notice.
            let Some(notice) = stale else {
                return false;
            };
            self.status = notice;
            return true;
        }
        let before = self.session_participants.len();
        let after = roster.len();
        self.session_participants = roster;
        // before == 0 is the initial self-only read; not worth announcing.
        if after > before && before > 0 {
            self.status = format!(
                "A participant attached read-only ({after} attached). Manage via Session: Participants (Cmd+K A)"
            );
        } else if after < before {
            self.status = format!("A participant detached ({after} attached)");
        }
        if let Some(notice) = stale {
            self.status = notice;
        }
        // Keep an open participants picker live without wiping the user's
        // typed filter, highlight, or scroll (any client resizing churns the
        // roster within the poll window; a naive rebuild would reset it).
        if let Some(p) = self.list_picker.as_ref()
            && p.purpose == crate::widgets::list_picker::ListPurpose::SessionParticipant
        {
            let (query, cursor, selected, scroll) =
                (p.query.clone(), p.cursor, p.selected, p.scroll);
            self.open_participants_picker();
            if let Some(p) = self.list_picker.as_mut() {
                let last = p.rows.len().saturating_sub(1);
                p.query = query;
                p.cursor = cursor;
                p.selected = selected.min(last);
                p.scroll = scroll.min(last);
            }
        }
        true
    }

    /// Drain typing attribution from the session host: when the typing
    /// participant changes, hand the shared cursor over (stash the previous
    /// typist's caret, restore the new one's).
    pub(super) fn poll_session_typing(&mut self) -> bool {
        let Some(channel) = self.session_channel.as_mut() else {
            return false;
        };
        let typists = channel.drain_typing();
        if typists.is_empty() {
            return false;
        }
        let mut changed = false;
        for id in typists {
            if self.session_typist != Some(id) {
                self.apply_typist_change(id);
                changed = true;
            }
        }
        changed
    }

    /// Participant `id` is typing now: park the previous typist's caret and
    /// move the shared cursor back to where `id` last was (same file only;
    /// a caret parked in another file just renders there as a ghost once
    /// that file is active again).
    pub(super) fn apply_typist_change(&mut self, id: u64) {
        let path = self.editor.path.clone();
        if let (Some(prev), Some(path)) = (self.session_typist, path.as_ref()) {
            self.session_carets.insert(
                prev,
                SessionCaret {
                    path: path.clone(),
                    row: self.editor.cursor_row,
                    col: self.editor.cursor_col,
                },
            );
        }
        self.session_typist = Some(id);
        if let (Some(parked), Some(path)) = (self.session_carets.get(&id), path.as_ref())
            && parked.path == *path
            && !self.editor.lines.is_empty()
        {
            let row = parked.row.min(self.editor.lines.len() - 1);
            let col = parked.col.min(self.editor.lines[row].chars().count());
            self.editor.cursor_row = row;
            self.editor.cursor_col = col;
        }
    }

    /// The resident navigator's activation check (docs/MULTIPLAYER.md):
    /// stat the workspace's pair record on a 1s cadence, seat or unseat the
    /// pilot on `enabled` transitions, and — on a plain launch with no
    /// collab session — self-appoint the OWNER seat first, because only an
    /// owner answers the pilot's snapshot requests. Solo guests never host:
    /// the owner's croft does.
    pub(super) fn maybe_seat_navigator(&mut self) -> bool {
        let due = self
            .last_pair_check
            .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(1));
        if !due {
            return false;
        }
        self.last_pair_check = Some(std::time::Instant::now());
        // A rewritten record re-arms seating: after a failed seat the user
        // re-runs `croft pair`, and the CLI promises "a running croft seats
        // it within a second" — but an enabled→enabled rewrite has no other
        // observable transition. The record's BYTES are the signal (every
        // write stamps them, and mtime misses a same-grain rewrite on a
        // coarse-timestamp filesystem).
        let raw = std::fs::read(&self.pair_record_path).ok();
        if self.last_pair_record_raw != raw {
            if self.last_pair_record_raw.is_some() {
                self.navigator_down = false;
            }
            self.last_pair_record_raw = raw.clone();
        }
        let record: Option<crate::session::PairRecord> = raw
            .as_deref()
            .and_then(|bytes| serde_json::from_slice(bytes).ok());
        let want = record.as_ref().is_some_and(|r| r.enabled);
        if !want {
            // Deactivation clears the death latch, so `croft pair --off`
            // then `croft pair` (or a palette off/on) re-activates.
            self.navigator_down = false;
            self.pair_lock_denied = None;
            // A fix stream cannot outlive the seat: with the navigator
            // deactivated no TurnDone can arrive, and a marker left here
            // spun the row (and woke the renderer) forever (#374 review).
            self.clear_problem_fix();
            if let Some(host) = self.pair_host.take() {
                // Drop tears the seat down; its parked caret goes with it.
                let sites = host.caret_sites();
                self.collab_carets.retain(|s, _| !sites.contains(s));
                self.navigator_notes.clear();
                self.pair_commentary_buf.clear();
                self.pair_turn_origin = None;
                // The turn counters die with the seat: leaking them made
                // the NEXT seat's first TurnDone report a dead seat's notes.
                self.pair_notes_this_turn = 0;
                self.pair_last_noted_file = None;
                self.pair_noted_files.clear();
                self.editor.comment_focus = None;
                self.status = String::from("Navigator unseated");
                return true;
            }
            return false;
        }
        if self.pair_host.is_some() {
            return false;
        }
        // The pilot died or failed to seat: do not respawn it every second.
        // The user re-activates (off/on) to try again; a fresh croft retries.
        if self.navigator_down {
            return false;
        }
        if self
            .collab_config
            .as_ref()
            .is_some_and(|(_, role)| *role == crate::collab::CollabRole::Guest)
        {
            return false;
        }
        let record = record.expect("want implies a record");
        let mut self_appointed_now = false;
        if self.collab_config.is_none() {
            // Only one croft may self-appoint owner per workspace: two owners
            // would both claim site id 1 and corrupt the shared buffer. Claim
            // the single-host lock first; if another croft holds it, do not
            // host (keep checking — take over when that croft exits).
            if self.pair_host_lock.is_none() {
                match crate::session::try_acquire_pair_host_lock(&self.pair_host_lock_path) {
                    Ok(lock) => {
                        self.pair_host_lock = Some(lock);
                        self.pair_lock_denied = None;
                    }
                    Err(e) => {
                        // Announced ONCE per reason: the takeover poll keeps
                        // running every second, and re-announcing clobbered
                        // the window's status line forever at a 1 Hz repaint.
                        let denial = match &e {
                            crate::session::PairHostLockError::Busy => PairLockDenial::Busy,
                            crate::session::PairHostLockError::Io(_) => PairLockDenial::Io,
                        };
                        if self.pair_lock_denied == Some(denial) {
                            return false;
                        }
                        self.pair_lock_denied = Some(denial);
                        // Busy and unopenable read the same from here (no
                        // host this tick, poll again) but not to the user:
                        // one has a window to close, the other has a path
                        // to fix and no window anywhere (#337).
                        self.status = match e {
                            crate::session::PairHostLockError::Busy => format!(
                                "Navigator '{}' is hosted by another croft window in this workspace",
                                record.name
                            ),
                            crate::session::PairHostLockError::Io(e) => format!(
                                "Navigator '{}': cannot open host lock {}: {e}",
                                record.name,
                                self.pair_host_lock_path.display()
                            ),
                        };
                        return true;
                    }
                }
            }
            if let Err(e) = crate::collab::ensure_relay(&self.pair_socket) {
                self.status = format!("Navigator: relay failed: {e}");
                // The spawn failure's sibling: this window holds the lock
                // but cannot host without a relay. Release it so another
                // window can try; this one retries next second and may
                // re-acquire.
                self.pair_host_lock = None;
                return true;
            }
            self.collab_config = Some((self.pair_socket.clone(), crate::collab::CollabRole::Owner));
            self.last_collab_connect = None; // connect this tick, not in 2s
            self_appointed_now = true;
        }
        let cfg = crate::pair::PairConfig {
            socket: self.pair_socket.clone(),
            workspace: self.tree.root.clone(),
            name: record.name.clone(),
            model: record.model.clone(),
            // Resident seating never replays a task: a persisted task would
            // re-execute on every launch, and an @file task would freeze the
            // UI thread on seat (the owner that must serve the file is this
            // very thread). Instructions come from Cmd+K Q. The --repl driver
            // still takes a one-shot task via PairConfig directly.
            task: None,
            provider: crate::pair::Provider::from_record(
                record.provider.as_deref(),
                record.base_url.as_deref(),
            ),
        };
        #[cfg(test)]
        let spawned = match &self.pair_spawn_override {
            Some(spawn) => spawn(&cfg),
            None => crate::pair_host::PairHost::spawn(cfg),
        };
        #[cfg(not(test))]
        let spawned = crate::pair_host::PairHost::spawn(cfg);
        match spawned {
            Ok(host) => {
                self.status = format!(
                    "Navigator '{}' seated (Cmd+K Q asks, Cmd+K Y yields)",
                    host.title()
                );
                self.pair_host = Some(host);
            }
            Err(e) => {
                self.status = format!("Navigator failed to seat: {e}");
                self.navigator_down = true; // no 1s retry storm
                if self_appointed_now {
                    // A window that cannot host must not block one that can:
                    // undo the self-appointment made moments ago (nothing
                    // has connected yet within this same call) and release
                    // the lock so another croft may claim the seat. A host
                    // whose PILOT died later keeps both — it is still the
                    // live collab owner and re-seats on re-activation.
                    self.collab_config = None;
                    self.pair_host_lock = None;
                }
            }
        }
        true
    }

    /// Drain the navigator's per-tick events: commentary to the Navigator
    /// OUTPUT channel, note/turn/death news to the status line, and the
    /// active file's note snapshot for the gutter marks and popup.
    pub(super) fn poll_pair(&mut self) -> bool {
        let Some(host) = self.pair_host.as_mut() else {
            return false;
        };
        let name = host.name().to_string();
        let events = host.poll();
        let mut changed = !events.is_empty();
        let mut died = false;
        for event in events {
            match event {
                crate::pair_host::PairEvent::Commentary(text) => {
                    // The navigator's voice is its comment boxes: prose
                    // accumulates and lands as one box when the turn ends.
                    if !text.trim().is_empty() {
                        if !self.pair_commentary_buf.is_empty() {
                            self.pair_commentary_buf.push('\n');
                        }
                        self.pair_commentary_buf.push_str(text.trim_end());
                    }
                }
                crate::pair_host::PairEvent::NoteAdded { file, row, body } => {
                    let snippet: String = body.chars().take(60).collect();
                    self.status = format!("{name} commented on {file}:{}: {snippet}", row + 1);
                    self.pair_notes_this_turn += 1;
                    self.pair_noted_files.insert(file.clone());
                    self.pair_last_noted_file = Some(file);
                }
                crate::pair_host::PairEvent::TurnDone { cancelled, failed } => {
                    let mut notes = std::mem::take(&mut self.pair_notes_this_turn);
                    let mut files = std::mem::take(&mut self.pair_noted_files);
                    let file = self.pair_last_noted_file.take();
                    let prose = std::mem::take(&mut self.pair_commentary_buf);
                    let origin = self.pair_turn_origin.take();
                    // Whatever the outcome, the fix turn is over: the row's
                    // spinner stops, and the LSP's next publish decides
                    // whether the row itself goes (#374).
                    self.clear_problem_fix();
                    if !cancelled && !prose.trim().is_empty() {
                        // Anchor the turn's prose at its invocation point (or
                        // where it last commented). A file that is not live
                        // any more falls back to the OUTPUT channel rather
                        // than losing the navigator's words.
                        let anchor = origin.or_else(|| {
                            file.clone().map(|f| {
                                // The caret row only means something in the
                                // file it is in: pairing the last-noted
                                // file with the ACTIVE tab's cursor row
                                // anchored the box at an unrelated line.
                                // Any other file clamps to its end.
                                let same = self
                                    .editor
                                    .path
                                    .as_ref()
                                    .and_then(|p| collab_file_key(&self.tree.root, p))
                                    .is_some_and(|active| active == f);
                                let row = if same {
                                    self.editor.cursor_row
                                } else {
                                    usize::MAX
                                };
                                (f, row)
                            })
                        });
                        let landed = anchor.as_ref().and_then(|(f, row)| {
                            self.pair_host
                                .as_ref()
                                .and_then(|h| h.add_note_now(f, *row, prose.trim()))
                        });
                        match (landed, anchor) {
                            (Some(_), Some((f, _))) => {
                                notes += 1;
                                files.insert(f);
                            }
                            _ => {
                                for line in prose.lines().filter(|l| !l.trim().is_empty()) {
                                    crate::output::push(
                                        "Navigator",
                                        crate::output::OutputLevel::Info,
                                        line,
                                    );
                                }
                            }
                        }
                    }
                    self.status = if cancelled {
                        format!("{name}: turn cancelled")
                    } else if let Some(err) = failed {
                        let err: String = err.chars().take(80).collect();
                        format!("{name}: turn failed: {err}")
                    } else if notes > 0 {
                        format!(
                            "{name} finished: {} · F4 to review",
                            turn_note_summary(notes, &files)
                        )
                    } else {
                        format!("{name} finished its turn")
                    };
                }
                crate::pair_host::PairEvent::Died(reason) => {
                    self.status = format!("Navigator: {reason}");
                    died = true;
                }
            }
        }
        if died {
            let sites = self
                .pair_host
                .take()
                .map(|h| h.caret_sites())
                .unwrap_or_default();
            self.collab_carets.retain(|s, _| !sites.contains(s));
            self.navigator_notes.clear();
            self.pair_commentary_buf.clear();
            self.pair_turn_origin = None;
            self.clear_problem_fix();
            // The turn counters die with the seat (see the unseat twin).
            self.pair_notes_this_turn = 0;
            self.pair_last_noted_file = None;
            self.pair_noted_files.clear();
            self.editor.comment_focus = None;
            self.navigator_down = true; // no auto-respawn; re-activate to retry
            return true;
        }
        // Refresh the active file's snapshot every tick: rows move under
        // concurrent edits without any new event arriving.
        if let Some(file) = self
            .editor
            .path
            .as_ref()
            .and_then(|p| collab_file_key(&self.tree.root, p))
            && let Some(host) = &self.pair_host
        {
            let snap = host.notes_snapshot(&file);
            if self.navigator_notes.get(&file) != Some(&snap) {
                self.navigator_notes.insert(file, snap);
                changed = true;
            }
        }
        changed
    }

    /// Open the ask box: an instruction input for the resident navigator,
    /// scoped to a 0-based inclusive line `range` of the active file, with
    /// `selection` carrying selected text when the scope came from one.
    /// Cmd+K Q, the gutter menu, and the body menu all land here.
    pub(super) fn open_ask_navigator(&mut self, range: (usize, usize), selection: String) {
        let Some(host) = &self.pair_host else {
            self.status =
                String::from("Navigator is not active (run croft pair in this workspace)");
            return;
        };
        if host.is_busy() {
            self.status = format!("{} is mid-turn; wait for it to finish", host.name());
            return;
        }
        let Some(file) = self
            .editor
            .path
            .as_ref()
            .and_then(|p| collab_file_key(&self.tree.root, p))
        else {
            self.status = String::from("Ask Navigator: no active workspace file");
            return;
        };
        let scope = if range.0 == range.1 {
            format!("line {}", range.0 + 1)
        } else {
            format!("lines {}-{}", range.0 + 1, range.1 + 1)
        };
        self.open_input_prompt(crate::widgets::input_prompt::InputPrompt::new(
            crate::widgets::input_prompt::InputPurpose::AskNavigator {
                file: file.clone(),
                range,
                selection,
            },
            format!("Ask {} · {file} · {scope}", host.name()),
            "What should it look at or do?",
        ));
    }

    /// Cmd+K Y: hand the navigator the floor on the active file. The turn
    /// is comment-only (host-enforced): its say lands as anchored notes and
    /// commentary, and it must ask before editing.
    pub(super) fn yield_to_navigator(&mut self) {
        let Some(host) = &self.pair_host else {
            self.status =
                String::from("Navigator is not active (run croft pair in this workspace)");
            return;
        };
        if host.is_busy() {
            self.status = format!("{} is mid-turn; wait for it to finish", host.name());
            return;
        }
        let Some(file) = self
            .editor
            .path
            .as_ref()
            .and_then(|p| collab_file_key(&self.tree.root, p))
        else {
            self.status = String::from("Yield: no active workspace file");
            return;
        };
        let content = self.editor.lines.join("\n");
        match host.send_yield_turn(&file, &content) {
            Ok(()) => {
                self.status = format!("Yielded {file} to {} (comment-only turn)", host.name());
                // Anchor the turn's prose commentary at the caret.
                self.pair_turn_origin = Some((file, self.editor.cursor_row));
            }
            Err(e) => self.status = format!("Yield failed: {e}"),
        }
    }

    /// Proactive navigator look (docs/MULTIPLAYER.md): when the driver
    /// completes a NEW construct in the active file (tree-sitter judged —
    /// half-typed code never parses as one) and pauses typing, the seated
    /// navigator takes the same comment-only turn Cmd+K Y hands it, anchored
    /// at the new construct. Only files the navigator has already looked at
    /// re-engage it (this is RE-engagement, not ambush), and a buffer state
    /// is scanned at most once. Runs once per frame tick; true = turn sent.
    pub(super) fn tick_proactive_navigator(&mut self) -> bool {
        if !self.proactive_navigator_enabled {
            return false;
        }
        let Some(host) = &self.pair_host else {
            return false;
        };
        if host.is_busy() || self.editor.comment_focus.is_some() {
            return false;
        }
        // The user is mid-typing in a prompt (the ask box included):
        // stealing the seat now would make their Enter land on a busy host
        // and fail. Suppress WITHOUT consuming the scan latch below, so the
        // look fires once the prompt closes.
        if self.input_prompt.is_some() {
            return false;
        }
        let Some(edited) = self.editor.last_edit_at else {
            return false;
        };
        if edited.elapsed() < PROACTIVE_PAUSE {
            return false;
        }
        let Some(path) = self.editor.path.clone() else {
            return false;
        };
        let seq = self.editor.edit_seq;
        if self.proactive_scanned.get(&path) == Some(&seq) {
            return false;
        }
        self.proactive_scanned.insert(path.clone(), seq);
        let Some(file) = collab_file_key(&self.tree.root, &path) else {
            return false;
        };
        let Some(kind) = crate::highlight::lang_for_path(&path) else {
            return false;
        };
        let Some(old) = host.last_seen(&file) else {
            return false; // it never looked at this file; no self-invite
        };
        let content = self.editor.lines.join("\n");
        let Some(row) = crate::pair::proactive::new_construct_row(kind, &old, &content) else {
            return false;
        };
        match host.send_yield_turn(&file, &content) {
            Ok(()) => {
                self.status = format!("{} is taking a look at {file}:{}", host.name(), row + 1);
                self.pair_turn_origin = Some((file, row));
                true
            }
            Err(_) => false,
        }
    }

    /// Palette toggle for the proactive looks, persisted like the inline
    /// blame preference.
    pub(super) fn toggle_proactive_navigator(&mut self) {
        self.proactive_navigator_enabled = !self.proactive_navigator_enabled;
        self.status = if self.proactive_navigator_enabled {
            String::from("Navigator proactive comments: on")
        } else {
            String::from("Navigator proactive comments: off")
        };
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_proactive_navigator(
                self.proactive_navigator_enabled,
            ));
        }
    }

    /// Palette toggle: flip the workspace's pair record and let the next
    /// activation check seat or unseat the pilot (within a second).
    pub(super) fn toggle_navigator(&mut self) {
        let current = crate::session::read_pair_record(&self.pair_record_path);
        let enabled = !current.as_ref().is_some_and(|r| r.enabled);
        let record = crate::session::PairRecord {
            model: current.as_ref().and_then(|r| r.model.clone()),
            name: current
                .as_ref()
                .map(|r| r.name.clone())
                .unwrap_or_else(|| String::from("claude")),
            enabled,
            task: None,
            // The seat's backend survives an off/on round trip: erasing it
            // silently reseated a local-only navigator on the cloud claude
            // CLI - with a model name claude does not even have.
            provider: current.as_ref().and_then(|r| r.provider.clone()),
            base_url: current.as_ref().and_then(|r| r.base_url.clone()),
        };
        if let Some(dir) = self.pair_record_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match crate::session::write_pair_record(&self.pair_record_path, &record) {
            Ok(()) => {
                self.last_pair_check = None; // act on this tick, not in 1s
                self.status = if enabled {
                    format!("Navigator '{}' activated; seating", record.name)
                } else {
                    String::from("Navigator deactivated; unseating")
                };
            }
            Err(e) => self.status = format!("Navigator toggle failed: {e}"),
        }
    }

    /// Palette: drop every navigator comment — the pilot's anchors, the
    /// per-file snapshots, and the boxes they feed.
    pub(super) fn clear_navigator_notes(&mut self) {
        if let Some(host) = &self.pair_host {
            host.clear_notes();
        }
        self.navigator_notes.clear();
        self.editor.comment_boxes.clear();
        self.editor.comment_focus = None;
        self.status = String::from("Navigator comments cleared");
    }

    /// Phase D per-tick collab pump (docs/MULTIPLAYER.md): connect lazily,
    /// have guests bootstrap newly opened workspace files, extract local
    /// edits into ops BEFORE applying inbound ones (the echo-storm
    /// invariant), then resolve inbound events against the open buffers.
    pub(super) fn poll_collab(&mut self) -> bool {
        use crate::collab::{CollabChannel, CollabEvent, CollabRole, CollabSession};
        if self.collab.is_none() {
            let Some((socket, role)) = self.collab_config.clone() else {
                return false;
            };
            // While the link is down there is no doc to converge panes of a
            // shared file through, yet input still reaches the active pane:
            // mirror it onto its siblings so panes cannot diverge
            // independently — the reconnect bootstrap keeps and replays
            // exactly ONE divergent text per file, and a second divergent
            // pane would be silently replaced by the attach pass.
            let mirrored = self.mirror_offline_siblings();
            let due = self
                .last_collab_connect
                .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(2));
            if !due {
                return mirrored;
            }
            self.last_collab_connect = Some(std::time::Instant::now());
            match CollabChannel::connect(&socket, role) {
                Some(ch) => {
                    let mut session = CollabSession::new(ch, collab_display_name());
                    if session.role == CollabRole::Guest {
                        session.send_notes_request();
                    }
                    self.collab = Some(session);
                }
                None => return mirrored,
            }
        }
        let mut session = self.collab.take().expect("connected above");
        let root = self.tree.root.clone();
        let mut changed = false;

        // Guests share every text file they open under the workspace; files
        // not open on a peer are ignored until opened, then bootstrapped.
        if session.role == CollabRole::Guest {
            let mut files: Vec<String> = Vec::new();
            let mut collect = |editors: &[crate::widgets::editor::Editor]| {
                for ed in editors {
                    if ed.has_non_text_view() {
                        continue;
                    }
                    if let Some(file) = ed.path.as_ref().and_then(|p| collab_file_key(&root, p)) {
                        files.push(file);
                    }
                }
            };
            collect(&self.editor.editors);
            for group in self.editor_layout.inactive_groups() {
                collect(&group.editors);
            }
            for file in files {
                if !session.is_live(&file) && !session.is_bootstrapping(&file) {
                    session.request_file(&file);
                    changed = true;
                }
            }
        }

        // Invariant 1a: extract-before-apply. Applying remote ops first
        // would re-diff them below as local edits and rebroadcast (echo).
        // Only ATTACHED buffers extract (collab_doc_gen != 0): a buffer
        // created after its file went live — a split duplicate, a reopen —
        // was loaded from the disk copy, which is stale by construction
        // (guests never write shared files). Diffing it against the doc
        // would broadcast a delete-everything/reinsert-the-old-file op set
        // and revert every peer; the attach pass below seeds it instead.
        let mut extract = |editors: &mut [crate::widgets::editor::Editor]| {
            for ed in editors.iter_mut() {
                if ed.has_non_text_view() {
                    continue;
                }
                let Some(file) = ed.path.as_ref().and_then(|p| collab_file_key(&root, p)) else {
                    continue;
                };
                if ed.collab_doc_gen == 0
                    || ed.edit_seq == ed.collab_synced_seq
                    || !session.is_live(&file)
                {
                    continue;
                }
                session.local_change(&file, &ed.lines.join("\n"));
                ed.collab_synced_seq = ed.edit_seq;
                ed.collab_doc_gen = session.doc_gen(&file).unwrap_or(0);
            }
        };
        extract(&mut self.editor.editors);
        for group in self.editor_layout.inactive_groups_mut() {
            extract(&mut group.editors);
        }

        // Invariant 1b: drain and resolve. The owner-side lookup opens the
        // requested file as a background tab when needed, so guest edits
        // land in a real buffer that saves, auto-saves, and reloads through
        // the one code path.
        let events = session.poll(|file| self.owner_buffer_text(&root, file));
        for event in events {
            changed = true;
            match event {
                CollabEvent::RemoteEdit { file, site, spans } => {
                    let doc_gen = session.doc_gen(&file).unwrap_or(0);
                    // An AI stream's fragments span many ticks; the whole
                    // stream is one undo step, as its cancel already is, so
                    // the driver can still reject it (#1207). Any other
                    // peer's edit opens none (#919).
                    let group = self
                        .collab_stream
                        .as_ref()
                        .filter(|st| st.file == file && st.site == site)
                        .map(|st| st.undo_group);
                    self.apply_collab_spans(&root, &file, &spans, doc_gen, group);
                }
                CollabEvent::Bootstrapped { file, text } => {
                    let doc_gen = session.doc_gen(&file).unwrap_or(0);
                    let owner_text = text.clone();
                    let base = self.collab_offline_base.remove(&file);
                    if let Some(kept) = self.finish_collab_bootstrap(&root, &file, text, doc_gen) {
                        // A buffer diverged while the link was down: replay
                        // the divergence as local edits so the offline work
                        // reaches every peer; the attach pass below re-seeds
                        // the kept panes from the doc once it holds the
                        // replayed text. With a harvested base, only this
                        // side's offline delta replays (three-way against
                        // the owner's snapshot), so owner edits made during
                        // the outage survive.
                        let replay = match base {
                            Some(base) => {
                                crate::collab::merge_offline_texts(&base, &kept, &owner_text)
                            }
                            None => kept,
                        };
                        session.local_change(&file, &replay);
                    }
                }
                CollabEvent::BootstrapTimedOut { file } => {
                    self.status = format!("{file}: no session owner answered; editing locally");
                }
                CollabEvent::OwnerLeft => {
                    self.status = String::from("The session owner left; saves now write to disk");
                }
                CollabEvent::OwnerReturned { files } => {
                    // The new owner starts from the file on disk, so that is
                    // the merge base for this guest's work since the owner
                    // left (#1147): only this side's changes replay, and any
                    // the new owner already made survive.
                    for file in files {
                        match crate::collab::contained_path(&root, &file)
                            .and_then(|p| std::fs::read_to_string(p).ok())
                        {
                            Some(disk) => {
                                self.collab_offline_base
                                    .insert(file, disk.replace("\r\n", "\n"));
                            }
                            None => {
                                self.collab_offline_base.remove(&file);
                            }
                        }
                    }
                    self.status = String::from("The session owner is back; rejoining shared files");
                }
                CollabEvent::Caret {
                    file,
                    site,
                    row,
                    col,
                    name,
                } => {
                    // Truncate at ingest so a peer's name can never paint a
                    // whole row; refresh the fade clock only on real moves
                    // (a re-broadcast of a parked caret keeps its old tag
                    // age, or the tag would never fade).
                    let name: String = name.chars().take(24).collect();
                    let moved = self.collab_carets.get(&site).is_none_or(|c| {
                        c.file != file || c.row != row || c.col != col || c.name != name
                    });
                    if moved {
                        self.collab_carets.insert(
                            site,
                            CollabCaret {
                                file,
                                row,
                                col,
                                name,
                                last_moved: std::time::Instant::now(),
                            },
                        );
                    }
                }
                CollabEvent::StreamState {
                    file,
                    site,
                    name,
                    active,
                } => {
                    // Truncate like caret names: a badge can never paint a
                    // whole status row.
                    let name: String = name.chars().take(24).collect();
                    let same = |st: &CollabStreamInfo| st.file == file && st.site == site;
                    let previous = self.collab_stream.take();
                    let undo_group = match &previous {
                        Some(st) if active && same(st) => st.undo_group,
                        _ => next_remote_undo_group(),
                    };
                    // A stream that ended (or gave way to another) closes
                    // its undo step; one whose cancel restored the text
                    // leaves none.
                    if let Some(st) = previous.filter(|st| !(active && same(st))) {
                        self.end_collab_undo_group(&root, &st.file, st.undo_group);
                    }
                    self.collab_stream = active.then_some(CollabStreamInfo {
                        file,
                        name,
                        site,
                        undo_group,
                    });
                }
                // The cancel request is for the streaming pilot, not for
                // viewers; the badge clears via StreamState(inactive).
                CollabEvent::StreamCancel => {}
                CollabEvent::Note(note) => {
                    if self.notes.merge(note) {
                        self.save_notes(Some(&session));
                    }
                }
                CollabEvent::NotesRequested => {
                    for note in self.notes.all().to_vec() {
                        session.send_note(&note);
                    }
                }
            }
        }

        // Attach-and-mirror: a buffer of a live file that has never synced
        // (gen 0: a fresh split duplicate or reopen holding stale disk
        // text) is seeded from the replicated text, and a clean buffer
        // whose generation fell behind (a sibling pane extracted a local
        // edit) mirrors the doc. Every pane of a live file converges every
        // tick — "the first found stays synced" was unstable, because
        // focusing another group reorders which pane is found first, and a
        // behind pane accepting a keystroke would diff its older text
        // against the doc and silently delete the other pane's work.
        let mut attach = |editors: &mut [crate::widgets::editor::Editor]| {
            for ed in editors.iter_mut() {
                // Diff/image/sheet tabs can carry a shared file's path, but
                // their `lines` are not documents: never seed or mark them.
                if ed.has_non_text_view() {
                    continue;
                }
                let Some(file) = ed.path.as_ref().and_then(|p| collab_file_key(&root, p)) else {
                    continue;
                };
                let Some(doc_gen) = session.doc_gen(&file) else {
                    continue;
                };
                if ed.collab_doc_gen == doc_gen
                    || (ed.collab_doc_gen != 0 && ed.edit_seq != ed.collab_synced_seq)
                {
                    continue;
                }
                let text = session
                    .doc_text(&file)
                    .expect("doc_gen implies a live doc")
                    .to_string();
                // The owner's buffer at snapshot creation already IS the
                // doc text: attach without a cursor-jolting replace.
                if ed.lines.join("\n") != text {
                    ed.replace_all_lines(text.split('\n').map(str::to_string).collect());
                    changed = true;
                }
                ed.collab_synced_seq = ed.edit_seq;
                ed.collab_doc_gen = doc_gen;
            }
        };
        attach(&mut self.editor.editors);
        for group in self.editor_layout.inactive_groups_mut() {
            attach(&mut group.editors);
        }

        // Broadcast this participant's caret when it moved in a live file.
        if let Some(file) = self
            .editor
            .path
            .as_ref()
            .and_then(|p| collab_file_key(&root, p))
            && session.is_live(&file)
        {
            let caret = (file, self.editor.cursor_row, self.editor.cursor_col);
            if self.collab_caret_sent.as_ref() != Some(&caret) {
                session.send_caret(&caret.0, caret.1, caret.2);
                self.collab_caret_sent = Some(caret);
            }
        }

        if session.disconnected() {
            self.drop_lost_collab_session(session);
            return true;
        }
        self.collab = Some(session);
        changed | self.collab_labels_dirty()
    }

    /// The relay hung up (EOF or a failed write): keeping the dead session
    /// made `is_live` lie forever while every op vanished. Drop it; the
    /// connect path re-attaches (2s backoff) and guest files re-bootstrap
    /// through the normal request path.
    pub(super) fn drop_lost_collab_session(&mut self, session: crate::collab::CollabSession) {
        // Harvest each live doc's replicated text first: it is the merge
        // base for offline edits at the next bootstrap. Without it, the
        // whole diverged buffer replays and reads the owner's concurrent
        // (unseen) edits as deletions.
        for (file, text) in session.live_texts() {
            self.collab_offline_base.insert(file, text);
        }
        self.status = String::from("Shared session link lost; reconnecting");
    }

    /// One redraw when the set of visible caret name tags changes with no
    /// accompanying wire traffic — i.e. the last tag crossing its 2s fade
    /// window (appearances already redraw via the Caret event itself).
    pub(super) fn collab_labels_dirty(&mut self) -> bool {
        let now = std::time::Instant::now();
        let any = self
            .collab_carets
            .values()
            .any(|c| now.duration_since(c.last_moved) < CARET_LABEL_FADE);
        let flipped = any != self.collab_labels_visible;
        self.collab_labels_visible = any;
        flipped
    }

    /// Cancel the AI stream (gutter stop button, Cmd+K X, palette): ask the
    /// streaming pilot over the relay to stop and revert. The badge clears
    /// when the pilot broadcasts StreamState(inactive), not optimistically —
    /// the revert is the pilot's to confirm.
    pub fn collab_cancel_stream(&mut self) {
        let Some(stream) = &self.collab_stream else {
            self.status = "No AI stream is active".into();
            return;
        };
        let Some(session) = &mut self.collab else {
            self.status = "No AI stream is active".into();
            return;
        };
        session.send_stream_cancel();
        self.status = format!("Asked {} to stop streaming", stream.name);
    }

    /// The buffer row wearing the gutter stop button: the pilot's ghost
    /// caret row when the streamed file is active here (the button rides
    /// the stream), else the top visible row so the button is always in
    /// view; None when no stream targets the active file.
    pub(super) fn stream_stop_row(&self) -> Option<usize> {
        let stream = self.collab_stream.as_ref()?;
        let active = self
            .editor
            .path
            .as_ref()
            .and_then(|p| collab_file_key(&self.tree.root, p))?;
        if active != stream.file {
            return None;
        }
        Some(
            self.collab_carets
                .get(&stream.site)
                .filter(|c| c.file == stream.file)
                .map(|c| c.row)
                .unwrap_or(self.editor.scroll),
        )
    }

    /// While the collab link is down, mirror the active pane onto its
    /// sibling panes of the same file (nothing else converges them without
    /// a live doc). Input only reaches the active pane, so running this
    /// every tick keeps all panes equal and the reconnect bootstrap sees
    /// exactly one divergent text per file. Only a previously-attached pane
    /// (generation != 0) mirrors out: a fresh reopen's disk text is stale
    /// by construction and would revert siblings still holding session
    /// text.
    pub(super) fn mirror_offline_siblings(&mut self) -> bool {
        if self.editor.collab_doc_gen == 0 || self.editor.has_non_text_view() {
            return false;
        }
        let Some(path) = self.editor.path.clone() else {
            return false;
        };
        if collab_file_key(&self.tree.root, &path).is_none() {
            return false;
        }
        let lines = self.editor.lines.clone();
        let mut changed = false;
        let mut mirror = |ed: &mut crate::widgets::editor::Editor| {
            if ed.has_non_text_view() {
                return;
            }
            if ed.path.as_deref() != Some(path.as_path()) || ed.lines == lines {
                return;
            }
            ed.replace_all_lines(lines.clone());
            changed = true;
        };
        let active = self.editor.active_index();
        for (i, ed) in self.editor.editors.iter_mut().enumerate() {
            if i != active {
                mirror(ed);
            }
        }
        for group in self.editor_layout.inactive_groups_mut() {
            for ed in group.editors.iter_mut() {
                mirror(ed);
            }
        }
        changed
    }

    /// The owner's current text for a file a guest asked to share: the open
    /// buffer if any, else open it as a background tab (keeping the owner's
    /// focus where it was) and read that. None declines the request
    /// (missing, unreadable, or not a text buffer).
    pub(super) fn owner_buffer_text(&mut self, root: &Path, file: &str) -> Option<String> {
        let path = crate::collab::contained_path(root, file)?;
        // First TEXT tab with the path: a diff tab of the same file found
        // first must not make the owner decline a share its real buffer
        // could serve.
        let is_text = |ed: &&crate::widgets::editor::Editor| {
            ed.path.as_deref() == Some(path.as_path())
                && ed.diff.is_none()
                && ed.image.is_none()
                && ed.sheet.is_none()
        };
        if let Some(ed) = self.editor.editors.iter().find(is_text) {
            return Some(ed.lines.join("\n"));
        }
        for group in self.editor_layout.inactive_groups() {
            if let Some(ed) = group.editors.iter().find(is_text) {
                return Some(ed.lines.join("\n"));
            }
        }
        if !path.is_file() {
            return None;
        }
        let keep = self.editor.active_index();
        self.editor.open_in_new_tab(&path).ok()?;
        let opened = self.editor.active_index();
        self.editor.select(keep);
        let ed = &self.editor.editors[opened];
        (ed.diff.is_none() && ed.image.is_none() && ed.sheet.is_none()).then(|| ed.lines.join("\n"))
    }

    /// Close a remote undo group in every text tab of `file` (#1207).
    pub(super) fn end_collab_undo_group(&mut self, root: &Path, file: &str, group: u64) {
        let Some(path) = crate::collab::contained_path(root, file) else {
            return;
        };
        let groups = self.editor_layout.inactive_groups_mut();
        for ed in self
            .editor
            .editors
            .iter_mut()
            .chain(groups.into_iter().flat_map(|g| g.editors.iter_mut()))
        {
            if ed.path.as_deref() == Some(path.as_path()) && !ed.has_non_text_view() {
                ed.end_remote_undo_group(group);
            }
        }
    }

    /// Replay resolved remote spans onto every open buffer for `file`,
    /// converting each byte span to the editor's char coordinates. Spans are
    /// sequential (each relative to the text with earlier ones applied), so
    /// they apply one at a time: an AI stream's all in `undo_group`'s one
    /// undo step (#1207), a peer's (no group) in no step of their own but
    /// carried into the undo history (#919). Every ATTACHED pane
    /// gets them (attached panes hold identical text, so the byte offsets
    /// are valid in each); a never-attached pane still holds stale disk
    /// text where the offsets would splice garbage — the attach pass seeds
    /// it from the doc instead.
    pub(super) fn apply_collab_spans(
        &mut self,
        root: &Path,
        file: &str,
        spans: &[crate::collab::ResolvedSpan],
        doc_gen: u64,
        undo_group: Option<u64>,
    ) {
        let Some(path) = crate::collab::contained_path(root, file) else {
            return;
        };
        // Every matching TEXT tab, not the first tab found by path: a
        // diff/image/sheet tab carries the path too (close-by-path), and a
        // first-found lookup let one shadow the real text tab out of its
        // updates while the spans spliced into a buffer that is not a
        // document.
        let apply = |ed: &mut crate::widgets::editor::Editor| {
            if ed.has_non_text_view() {
                return;
            }
            if ed.collab_doc_gen == 0 {
                return;
            }
            for s in spans {
                let edit = crate::widgets::editor::TextSpanEdit {
                    start: crate::collab::position(&ed.lines, s.at),
                    end: crate::collab::position(&ed.lines, s.at + s.deleted),
                    new_text: s.inserted.clone(),
                    utf16: false,
                };
                match undo_group {
                    Some(group) => ed.apply_remote_span_edits(&[edit], group),
                    None => ed.apply_peer_span_edit(&edit),
                };
            }
            // Echo suppression: apply_span_edits bumped edit_seq, which would
            // re-arm the tick diff and rebroadcast what was just applied.
            ed.collab_synced_seq = ed.edit_seq;
            ed.collab_doc_gen = doc_gen;
        };
        for ed in self.editor.editors.iter_mut() {
            if ed.path.as_deref() == Some(path.as_path()) {
                apply(ed);
            }
        }
        for group in self.editor_layout.inactive_groups_mut() {
            for ed in group.editors.iter_mut() {
                if ed.path.as_deref() == Some(path.as_path()) {
                    apply(ed);
                }
            }
        }
    }

    /// A guest's bootstrap snapshot landed: swap every open buffer for the
    /// file to the owner's canonical text (usually identical to what was
    /// read from disk) and mark them synced, which also lifts the input
    /// gate. A buffer that was already attached to an earlier session
    /// incarnation and diverges from the snapshot holds work made while the
    /// link was down — work that exists nowhere else, so replacing it would
    /// silently delete it. Such a buffer keeps its text and is returned for
    /// the caller to re-inject as local edits (panes converged every tick
    /// before the link died, so any divergent pane carries the same text).
    pub(super) fn finish_collab_bootstrap(
        &mut self,
        root: &Path,
        file: &str,
        text: String,
        doc_gen: u64,
    ) -> Option<String> {
        let path = crate::collab::contained_path(root, file)?;
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let mut kept = None;
        let mut finish = |ed: &mut crate::widgets::editor::Editor| {
            if ed.has_non_text_view() {
                return;
            }
            if ed.collab_doc_gen != 0 && ed.lines != lines {
                // Offline divergence: keep the buffer; gen 0 lets the
                // attach pass re-mark it once the doc holds the merged
                // text (the caller's local_change makes them equal).
                if kept.is_none() {
                    kept = Some(ed.lines.join("\n"));
                }
                ed.collab_doc_gen = 0;
                return;
            }
            if ed.lines != lines {
                ed.replace_all_lines(lines.clone());
            }
            ed.collab_synced_seq = ed.edit_seq;
            ed.collab_doc_gen = doc_gen;
        };
        // Every matching TEXT tab (see apply_collab_spans): first-found let
        // a diff tab of the file swallow the snapshot while the text tab
        // went unmarked.
        for ed in self.editor.editors.iter_mut() {
            if ed.path.as_deref() == Some(path.as_path()) {
                finish(ed);
            }
        }
        for group in self.editor_layout.inactive_groups_mut() {
            for ed in group.editors.iter_mut() {
                if ed.path.as_deref() == Some(path.as_path()) {
                    finish(ed);
                }
            }
        }
        self.status = format!("{file} is now live-shared");
        kept
    }

    /// True when this croft is a solo-viewport guest: the session owner is
    /// the one writer of shared files to disk, so every save/reload/replace
    /// path for workspace files is gated here. Config-based (not
    /// connection-based) so the gates hold while the relay is still coming
    /// up.
    pub(super) fn is_collab_guest(&self) -> bool {
        matches!(
            self.collab_config,
            Some((_, crate::collab::CollabRole::Guest))
        )
    }
}

impl App {
    /// Ask for a new lane's name (#348); the palette command and `Cmd+K
    /// Shift+L` both land here.
    pub(super) fn open_new_lane_prompt(&mut self) {
        self.open_input_prompt(crate::widgets::input_prompt::InputPrompt::new(
            crate::widgets::input_prompt::InputPurpose::NewWorktreeLane,
            String::from("New worktree lane"),
            String::from("what the agent will work on"),
        ));
    }

    /// Create a worktree lane and add it as a workspace root (#348).
    ///
    /// The worktree is a SIBLING of the repo, never a child: a worktree
    /// nested inside its own repo is a working tree git then tries to track,
    /// and the Explorer would show the lane inside the root it was cut from.
    pub(super) fn create_worktree_lane(&mut self, name: &str) {
        let repo = self.workspace_root().to_path_buf();
        let Some(mut lane) = crate::git::WorktreeLane::plan(&repo, name) else {
            // Refused rather than defaulted: a name with nothing usable in
            // it would otherwise become the branch `agent/`.
            self.status = format!("{name:?} has no letters or digits to name a lane after");
            return;
        };
        if lane.path.exists() {
            self.status = format!("{} already exists — pick another name", lane.path.display());
            return;
        }
        match crate::git::add_worktree_lane(&repo, &mut lane) {
            Ok(()) => {
                self.add_workspace_folder(lane.path.clone());
                self.open_lane_pane(&lane);
            }
            Err(why) => self.status = format!("Could not create the lane: {why}"),
        }
    }

    /// The pane half of a new lane (#348): a named terminal in the worktree,
    /// running the configured agent's launch line when there is one. The
    /// status says which of the three outcomes happened — agent started,
    /// plain shell because no agent is configured, or plain shell because
    /// the configured agent has no launch line — since a lane that quietly
    /// opens a shell where the user expected an agent reads as broken.
    pub(super) fn open_lane_pane(&mut self, lane: &crate::git::WorktreeLane) {
        let slug = lane
            .branch
            .strip_prefix("agent/")
            .unwrap_or(&lane.branch)
            .to_string();
        let agent = self
            .lane_agent
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_ascii_lowercase);
        let launch = agent
            .as_deref()
            .and_then(|a| self.agents.launch_line(a))
            .map(str::to_string);
        let mut term = match PtyTerminal::new(&lane.path) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!(
                    "Lane {} on branch {} — could not open its pane: {e}",
                    lane.path.display(),
                    lane.branch
                );
                return;
            }
        };
        term.set_manual_name(Some(format!("Lane: {slug}")));
        if let Some(cmd) = &launch {
            term.write_input(lane_launch_input(cmd).as_bytes());
        }
        let seated = launch.is_some();
        // Recorded BEFORE the insert: `insert_terminal` saves the terminal
        // session, and the save reads this map.
        self.lane_panes.insert(
            term.uid(),
            crate::terminal_session::LaneRecord {
                // Canonical, like the workspace roots it is compared with
                // when the lane closes; taken now, while the directory
                // exists to resolve.
                path: lane
                    .path
                    .canonicalize()
                    .unwrap_or_else(|_| lane.path.clone())
                    .display()
                    .to_string(),
                branch: lane.branch.clone(),
                agent: if seated { agent.clone() } else { None },
                base: lane.base.clone(),
            },
        );
        self.insert_terminal(term);
        let pane = match (agent, seated) {
            (Some(a), true) => format!("{a} started in pane Lane: {slug}"),
            (Some(a), false) => {
                format!(
                    "plain shell in pane Lane: {slug} (agents.json has no launch line for {a:?})"
                )
            }
            (None, _) => format!("shell in pane Lane: {slug} (set lane_agent to start an agent)"),
        };
        self.status = format!(
            "Lane {} on branch {} — {pane}",
            lane.path.display(),
            lane.branch
        );
    }

    /// The review-queue summary, one row per agent, grouped by the workspace
    /// root each file lives under (#348).
    ///
    /// Grouping is what makes the queue usable once worktree lanes exist:
    /// with an agent working in a lane and you working in the main checkout,
    /// a flat list of basenames cannot say which `mod.rs` it means. Each
    /// group is named by its lane's BRANCH where the root is a lane, and by
    /// the root's display label otherwise.
    ///
    /// A row whose files all sit under ONE root keeps the pre-grouping flat
    /// shape, `agent: n to review (a.rs, b.rs)`: with one group the label
    /// names the only place the files could be and the group count merely
    /// repeats `n`. So the common single-root session is untouched, and
    /// grouping appears exactly when there is something to tell apart.
    ///
    /// A grouped row carries labels and counts but no file names, because
    /// the status bar is one row that elides its MIDDLE: a row long enough
    /// to be cut loses the group labels, which is precisely what grouping
    /// exists to show. The flat row keeps its sample, being short already.
    pub(super) fn agent_lane_rows(&self) -> Vec<String> {
        let roots: Vec<PathBuf> = self.roots.iter().map(Path::to_path_buf).collect();
        let labels = crate::workspace::root_display_labels(&roots);
        // A lane's branch names it better than its directory does: the
        // directory is `repo-fix-login`, the branch is what the agent is on.
        let branch_of = |root: &Path| -> Option<String> {
            self.lane_panes
                .values()
                .find(|l| Path::new(&l.path) == root)
                .map(|l| l.branch.clone())
        };
        // Up to three names per group, with an ellipsis when there are more:
        // a count next to a truncated list otherwise reads as an
        // enumeration that lost two entries.
        let sample = |files: &[&crate::agent_lane::LaneFile]| -> String {
            let names: Vec<String> = files
                .iter()
                .take(3)
                .map(|f| {
                    let name = f
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if f.shared {
                        format!("{name} (shared)")
                    } else {
                        name
                    }
                })
                .collect();
            let mut out = names.join(", ");
            if files.len() > names.len() {
                out.push_str(", \u{2026}");
            }
            out
        };
        self.agent_ledger
            .agents()
            .into_iter()
            .map(|agent| {
                let n = self.agent_ledger.unreviewed_count(agent);
                let groups = self.agent_ledger.lane_by_root(agent, &roots);
                // One group is the single-root session this feature does not
                // change: the root label would name the only place the files
                // could be, and the group count would repeat `n`.
                if groups.len() <= 1 {
                    let files = groups.first().map(|(_, f)| sample(f)).unwrap_or_default();
                    return format!("{agent}: {n} to review ({files})");
                }
                // A lane's branch is derived from its slug alone, so two
                // lanes cut from different repos can share one; fall back to
                // the disambiguated root label when that happens, since a
                // row naming two groups identically is worse than a row
                // naming them by folder.
                let branches: Vec<Option<String>> = groups
                    .iter()
                    .map(|(root, _)| root.and_then(&branch_of))
                    .collect();
                let named: Vec<String> = groups
                    .iter()
                    .zip(&branches)
                    .map(|((root, files), branch)| {
                        let root_label = |r: &Path| {
                            roots
                                .iter()
                                .position(|candidate| candidate == r)
                                .and_then(|i| labels.get(i).cloned())
                                .unwrap_or_else(|| r.display().to_string())
                        };
                        let label = match (root, branch) {
                            (Some(_), Some(b))
                                if branches.iter().flatten().filter(|o| *o == b).count() == 1 =>
                            {
                                b.clone()
                            }
                            (Some(r), _) => root_label(r),
                            (None, _) => String::from("outside the workspace"),
                        };
                        // Labels and counts only. The status bar is ONE row
                        // and elides the MIDDLE of anything too long
                        // (`elide_middle`), so a row carrying samples for
                        // every group loses the group labels first — the
                        // one thing grouping adds. Which files they are is
                        // a question the Explorer's dots already answer.
                        format!("{label} {}", files.len())
                    })
                    .collect();
                format!("{agent}: {n} to review \u{2014} {}", named.join("; "))
            })
            .collect()
    }

    /// Give each lane's root row in the Explorer its branch and its agent
    /// (#348), from the lane panes: `agent/fix-login · ◆ claude ●` while the
    /// agent is seated, the configured agent's name before it is, the branch
    /// alone for a plain-shell lane. Runs from the top of `render`; the map
    /// is swapped only when its contents change, so the common frame costs a
    /// walk over a handful of lanes and a small map that is then dropped.
    pub(super) fn sync_lane_root_badges(&mut self) {
        // Several panes on one lane: the pane that knows the most decides
        // the badge, whichever order the map walks them in. Seated beats
        // configured beats plain, decided from the state rather than by
        // reading the glyph back out of the text.
        let mut best: std::collections::HashMap<PathBuf, (u8, String)> =
            std::collections::HashMap::new();
        for (uid, lane) in &self.lane_panes {
            let seated = self
                .terminals
                .iter()
                .find(|t| t.uid() == *uid)
                .and_then(|t| t.agent());
            let rank = match (seated, lane.agent.as_deref()) {
                (Some(_), _) => 2,
                (None, Some(_)) => 1,
                (None, None) => 0,
            };
            let key = PathBuf::from(&lane.path);
            if best.get(&key).is_some_and(|(have, _)| *have >= rank) {
                continue;
            }
            let badge = lane_root_badge(&lane.branch, seated, lane.agent.as_deref());
            best.insert(key, (rank, badge));
        }
        let badges: std::collections::HashMap<PathBuf, String> =
            best.into_iter().map(|(k, (_, b))| (k, b)).collect();
        // Swapped only on a change: the map built here is dropped, not
        // handed to the tree, when nothing moved.
        if *self.tree.root_badges != badges {
            self.tree.root_badges = std::sync::Arc::new(badges);
        }
    }

    /// Close the panes that belong to the lane at `lane` (#348), through the
    /// undoable close so a mistaken lane removal does not also lose the
    /// pane's scrollback. Returns how many closed and how many stayed.
    ///
    /// The close refuses the last pane, and a lane pane left standing would
    /// sit in a directory git just removed; so when the lane's panes are ALL
    /// the panes, a fresh shell in the primary root is opened first to be
    /// the one that stays. Only when that shell cannot spawn does a lane
    /// pane remain, stripped of its lane name, and the caller says so.
    pub(super) fn close_lane_panes(&mut self, lane: &Path) -> (usize, usize) {
        let doomed: Vec<usize> = self
            .terminals
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                self.lane_panes
                    .get(&t.uid())
                    .is_some_and(|l| Path::new(&l.path) == lane)
            })
            .map(|(i, _)| i)
            .collect();
        if !doomed.is_empty() && doomed.len() == self.terminals.len() {
            // Pushed, not `insert_terminal`ed: the doomed indices must stay
            // where they are, and the session is saved by the closes below.
            match PtyTerminal::new(self.roots.primary()) {
                Ok(term) => self.terminals.push(term),
                Err(e) => self.status = format!("Could not open a replacement pane: {e}"),
            }
        }
        let (mut closed, mut kept) = (0, 0);
        // Highest index first so the earlier indices stay valid.
        for idx in doomed.into_iter().rev() {
            let uid = self.terminals[idx].uid();
            if self.close_terminal_at(idx) {
                closed += 1;
            } else {
                // The last pane stays; it is no longer a lane's (its
                // worktree is gone), so it stops wearing the lane's name.
                kept += 1;
                self.terminals[idx].set_manual_name(None);
                self.lane_panes.remove(&uid);
            }
        }
        (closed, kept)
    }

    /// The first file under `dir` open with unsaved edits in any editor
    /// group, named relative to `dir`, for a removal to refuse on: git
    /// sees only what is on disk, and the tab's edits are not there yet.
    /// A sheet cell still being typed into is an unsaved edit too.
    pub(super) fn unsaved_file_under(&self, dir: &Path) -> Option<String> {
        std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .filter(|e| e.dirty || e.sheet_pending_cell().is_some())
            .find_map(|e| e.path.as_deref().filter(|p| p.starts_with(dir)))
            .map(|p| p.strip_prefix(dir).unwrap_or(p).display().to_string())
    }

    /// Point the COMMITS graph at `base..lane` for the active lane (#348).
    ///
    /// Uses `active_scm_root` like `close_worktree_lane` beside it, rather
    /// than the primary: a lane is never index 0, and a sibling command that
    /// silently switched which root it acted on is how that one destroyed a
    /// lane in testing.
    ///
    /// Leaves the panel showing the diff until something else refreshes it -
    /// the tag on the reply keeps a background history fetch from replacing
    /// it, and returning to history is the ordinary refresh path.
    pub(super) fn diff_worktree_lane(&mut self) {
        let lane = self.active_scm_root.clone();
        if lane == self.roots.primary() {
            self.status =
                String::from("The primary folder is not a lane — open a file in the lane first");
            return;
        }
        let Some(record) = self
            .lane_panes
            .values()
            .find(|r| std::path::Path::new(&r.path) == lane)
        else {
            self.status = String::from("No lane is open for this folder");
            return;
        };
        // A lane made before the base was recorded, or cut from a detached
        // HEAD, has nothing to diff against. Say which rather than showing an
        // empty graph that reads as "no commits".
        let Some(base) = record.base.clone() else {
            self.status = format!(
                "Lane {} records no base branch to diff against",
                record.branch
            );
            return;
        };
        let branch = record.branch.clone();
        self.graph_revspec = Some(format!("{base}..{branch}"));
        self.refresh_commit_graph();
        self.status = format!("COMMITS: {branch} against {base}");
    }

    /// Remove the active root's worktree lane, refusing when it is dirty
    /// (#348) or a tab in it has unsaved edits (#1050), and closing its
    /// panes and tabs.
    ///
    /// The refusal is the point. `git worktree remove` DISCARDS uncommitted
    /// work, so this is the one place in the lane flow where being wrong
    /// destroys something rather than annoying someone — and the check fails
    /// closed, treating a tree git cannot report on as unsafe.
    pub(super) fn close_worktree_lane(&mut self) {
        // The ACTIVE root, not the primary. `workspace_root()` is
        // `roots.primary()`, and a lane is appended by `add_workspace_folder`
        // so it is never index 0 — this targeted the user's main repo every
        // time. Today git's own "is a main working tree" refusal masks that,
        // but the guard is git's rather than ours: launch croft INSIDE a lane
        // (the workflow this feature creates) and the primary becomes a
        // removable worktree, so the directory is deleted, the folder cannot
        // be dropped, and the status reports success.
        let lane = self.active_scm_root.clone();
        if lane == self.roots.primary() {
            self.status =
                String::from("The primary folder is not a lane — open a file in the lane first");
            return;
        }
        // The diff outlives the lane otherwise: `remove_workspace_folder`
        // re-seeds the primary, and its COMMITS panel is then stuck on a
        // range whose right side no longer resolves.
        //
        // Cleared BEFORE the refusals, not in the success arm: `git worktree
        // remove` can still fail after `lane_removal_block` passed (a locked
        // worktree, below), and that arm returns too. On a refusal the rows
        // already drawn outlive the clear until the next refresh, which is
        // correct - they are still this lane's commits, and the lane is
        // still here.
        self.graph_revspec = None;
        // Unsaved tabs before git: git answers for the disk only, and once
        // the lane is gone such a tab's save recreates its folder as a
        // stray directory, off the lane's branch. First, too, because
        // saving one is what makes the tree dirty for the refusal below.
        if let Some(file) = self.unsaved_file_under(&lane) {
            self.status = format!(
                "Keeping the lane: {file} has unsaved changes \u{2014} save or revert first"
            );
            return;
        }
        if let Some(why) = crate::git::lane_removal_block(&lane) {
            self.status = format!("Keeping the lane: {why}");
            return;
        }
        // git FIRST, then the folder. `lane_removal_block` answers "is there
        // uncommitted work", which is not the same question as "will git
        // remove this" — a LOCKED worktree reports a clean status and is
        // then refused, measured. Dropping the folder first would leave the
        // user with the lane gone from their workspace and the directory
        // still on disk, which is the worst of both.
        match crate::git::remove_worktree_lane(&lane) {
            Ok(()) => {
                self.remove_workspace_folder(lane.clone());
                // `remove_workspace_folder` refuses the primary and says so
                // in the status; only claim success when the folder really
                // left, or a refusal there would be overwritten by a message
                // saying the whole thing worked.
                let (closed, kept) = self.close_lane_panes(&lane);
                // Its tabs go with its panes: every one is saved (the check
                // above), and each now shows a file that no longer exists.
                self.close_clean_tabs_under_everywhere(&lane);
                if self.roots.iter().any(|r| r == lane) {
                    self.status = format!(
                        "Removed {} on disk, but it is still in this workspace",
                        lane.display()
                    );
                } else if kept > 0 {
                    self.status = format!(
                        "Removed lane {} — its pane stays (no replacement pane could open) in a directory that is now gone",
                        lane.display()
                    );
                } else if closed > 0 {
                    self.status = format!(
                        "Removed lane {} and closed its pane (Cmd+K Shift+T reopens it)",
                        lane.display()
                    );
                } else {
                    self.status = format!("Removed lane {}", lane.display());
                }
            }
            Err(why) => self.status = format!("Keeping the lane: {why}"),
        }
    }

    /// Seat, re-judge, or unseat the agent in EVERY pane (#344), from the
    /// `(shell_pid, process_name)` samples the pill uses, and queue the
    /// transitions. Driven over panes rather than over samples: a pane whose
    /// foreground did not resolve this round (the agent exited and took the
    /// shell with it, a pid `sysinfo` missed) has no sample, and that must
    /// unseat it rather than leave the badge on forever. `quiet_after` is
    /// the working/quiet threshold ([`crate::agents::QUIET_AFTER`]; tests
    /// pass zero). Returns whether any pane's lane changed.
    pub(crate) fn apply_agent_samples(
        &mut self,
        samples: &[(i32, String)],
        quiet_after: std::time::Duration,
    ) -> bool {
        let by_pid: std::collections::HashMap<i32, &str> = samples
            .iter()
            .map(|(pid, name)| (*pid, name.as_str()))
            .collect();
        let mut changed = false;
        for t in &mut self.terminals {
            let kind = t
                .shell_pid()
                .and_then(|pid| by_pid.get(&pid))
                .and_then(|name| self.agents.classify(name));
            let prev = t.agent().cloned();
            // Only the seated agent's own OSC 7501 reports judge it: not a
            // departed agent's, nor another program's.
            t.sample_reporter(
                prev.as_ref().map(|p| p.name.as_str()),
                kind.map(|k| k.name.as_str()),
            );
            let next = kind.map(|kind| crate::agents::AgentLane {
                name: kind.name.clone(),
                // The agent's own report when it gave one; the screen only
                // for an agent that has not.
                status: match t.reported_status() {
                    Some(state) => crate::agents::reported(state),
                    None => crate::agents::judge(kind, t.quiet_for(), &t.tail_rows(6), quiet_after),
                },
            });
            if prev == next {
                continue;
            }
            let pane = t.label().to_string();
            self.agent_events.extend(crate::agents::transition(
                &pane,
                prev.as_ref(),
                next.as_ref(),
            ));
            t.set_agent(next);
            changed = true;
        }
        changed
    }

    /// Consume the agent transitions the last sample queued: a waiting agent
    /// is announced in the status bar (the chip and the pill already show
    /// it). Returns whether anything was said.
    pub(super) fn drain_agent_events(&mut self) -> bool {
        let mut said = false;
        while let Some(ev) = self.agent_events.pop_front() {
            // A gone agent with nothing left to review takes its lane with
            // it; one with a queue keeps it, since the files it changed
            // still need looking at (#345). The event is per PANE while the
            // lane is per NAME, so a second pane still running the same
            // agent — or an agent swap in one pane, which fires Gone then
            // Seated — must not take the shared lane down with it.
            if let crate::agents::AgentEvent::Gone { agent, .. } = &ev
                && self.agent_ledger.unreviewed_count(agent) == 0
                && !self
                    .terminals
                    .iter()
                    .any(|t| t.agent().is_some_and(|a| a.name == *agent))
            {
                let agent = agent.clone();
                // No decoration sync needed here, and deliberately so: the
                // guard above only lets this run when the lane has NOTHING
                // unreviewed, so it removes no row the Explorer is drawing
                // a dot for (#345).
                self.agent_ledger.forget(&agent);
            }
            if let crate::agents::AgentEvent::Waiting { pane, agent } = ev {
                self.status = format!("{agent} in {pane} is waiting for your input");
                said = true;
            }
        }
        said
    }

    /// `(agents seated, of which waiting)` across every pane, for the chip.
    pub(super) fn agent_counts(&self) -> (usize, usize) {
        let seated = self
            .terminals
            .iter()
            .filter(|t| t.agent().is_some())
            .count();
        let waiting = self
            .terminals
            .iter()
            .filter(|t| {
                t.agent()
                    .is_some_and(|a| a.status == crate::agents::AgentStatus::Waiting)
            })
            .count();
        (seated, waiting)
    }

    /// The agents whose panes are producing output right now (#345). A
    /// merely SEATED agent does not count: a quiet agent's pane is no
    /// reason to blame it for the user's own save.
    pub(super) fn working_agents(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .terminals
            .iter()
            .filter_map(|t| t.agent())
            .filter(|lane| lane.status == crate::agents::AgentStatus::Working)
            .map(|lane| lane.name.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Record each changed file against every working agent, hashing its
    /// current content. A file that cannot be read right now is skipped
    /// rather than recorded at a stale hash.
    ///
    /// A DELETION is processed whether or not an agent is working: a file
    /// can be removed after its agent goes idle, and the row would
    /// otherwise sit in the queue forever pointing at a file that is not
    /// there (its lane staying visible with it). Only attribution — adding
    /// or updating a row — needs a working agent to attribute to.
    pub(super) fn attribute_writes_to_working_agents(
        &mut self,
        changed: &std::collections::BTreeSet<PathBuf>,
    ) -> bool {
        let working = self.working_agents();
        let mut any = false;
        // Resolved once: the roots cannot change inside this loop, and on a
        // symlinked root the direct check never hits, so leaving it inside
        // would pay `files x roots` canonicalisations on every drain.
        let root_canons = self.root_canons();
        for path in changed {
            // Only workspace files: an agent editing its own config under
            // $HOME is not part of this workspace's review queue. ANY root
            // counts, not just the primary — a secondary folder's files are
            // as much this session's work as the first folder's (#345).
            // Compare in the same spelling on both sides. The watcher reports
            // the realpath, while a root is stored as the user gave it — so on
            // macOS a workspace under /var arrives as /private/var and matches
            // no root. Every agent write was silently dropped and the queue
            // stayed empty with nothing failing. Any root reached through a
            // symlink has the same shape, which is ordinary on Linux too.
            //
            // The path is recorded in the form the ROOT uses, so the ledger's
            // keys match what the Explorer's rows are keyed on and a later
            // "mark reviewed" finds the row instead of adding a second.
            let Some(under_root) = self.path_under_roots(&root_canons, path) else {
                continue;
            };
            let path = &under_root;
            match crate::agent_lane::read_baseline(path) {
                // Attribution needs someone to attribute to; a quiet
                // agent's pane is no reason to blame it for the user's
                // own save.
                crate::agent_lane::Baseline::Hash(hash) if !working.is_empty() => {
                    any |= self.agent_ledger.record_write(path, hash, &working);
                }
                crate::agent_lane::Baseline::Hash(_) => {}
                // The agent deleted it: the row goes, rather than sitting in
                // the queue forever pointing at a file that is not there.
                crate::agent_lane::Baseline::Gone => {
                    any |= self.agent_ledger.forget_path(path);
                }
                // Unreadable this instant; the next write re-reports it.
                crate::agent_lane::Baseline::Unreadable => {}
            }
        }
        any
    }

    /// `path` expressed under whichever workspace root owns it, or `None`
    /// when no root does.
    ///
    /// Tries the path as given first — the common case, costing nothing —
    /// then compares canonicalised, which is what a file watcher's realpath
    /// needs to match a root the user spelled with a symlink in it. The
    /// returned path is rooted the way the ROOT is spelled, so everything
    /// keyed on it agrees with the Explorer's rows.
    ///
    /// A DELETED path still resolves. `canonicalize` stats the final
    /// component and fails with `ENOENT` on a path that is gone — which is
    /// exactly what a deletion event carries, so resolving the whole path
    /// would make the ledger's `forget_path` arm unreachable and strand rows
    /// on files that no longer exist. The walk therefore resolves the
    /// deepest ancestor that DOES exist and re-attaches the tail: a deleted
    /// file's parent directory is almost always still there, and when it is
    /// not the walk keeps climbing.
    ///
    /// Longest prefix wins, mirroring `WorkspaceRoots::owning_root`: with
    /// nested roots the deeper one owns the path, and picking the first
    /// match instead would make the ledger's key depend on the order the
    /// user added folders.
    /// The roots are resolved by the CALLER so a loop over many changed
    /// paths can hoist the canonicalisation, which depends only on the root
    /// set and cannot change between iterations. Not a cache — there is
    /// nothing to invalidate, because the value does not outlive the loop
    /// that computes it.
    pub(super) fn path_under_roots(
        &self,
        root_canons: &[(PathBuf, PathBuf)],
        path: &Path,
    ) -> Option<PathBuf> {
        // The direct check FIRST, and that ordering is load-bearing rather
        // than merely quick. If an intermediate directory is itself a
        // symlink pointing outside the workspace, resolving the whole path
        // would land the result outside the root; hitting on the spelled
        // path here means the walk below never sees that case.
        if self.roots.owning_root(path).is_some() {
            return Some(path.to_path_buf());
        }
        let canon = canonicalize_existing_ancestor(path)?;
        root_canons
            .iter()
            .cloned()
            .filter_map(|(spelled, canon_root)| {
                let rel = canon.strip_prefix(&canon_root).ok()?;
                Some((canon_root.components().count(), spelled.join(rel)))
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, joined)| joined)
    }

    /// Each root as `(spelling, canonical)`.
    ///
    /// Recomputed per call rather than cached on `App`: the set is one or
    /// two entries in practice, and this runs only when a path failed the
    /// direct check — i.e. on a symlinked root, where it is the price of the
    /// feature working at all. A cache here would need invalidating from
    /// every root mutation, which is more ways to be wrong than it saves.
    pub(super) fn root_canons(&self) -> Vec<(PathBuf, PathBuf)> {
        self.roots
            .iter()
            .map(|r| {
                (
                    r.to_path_buf(),
                    std::fs::canonicalize(r).unwrap_or_else(|_| r.to_path_buf()),
                )
            })
            .collect()
    }

    /// Called from every place the ledger's CONTENT changes, rather than on
    /// a schedule: a decoration that lags the thing it describes is worse
    /// than none, because "no dot" is exactly what a reviewed file looks
    /// like, so a stale one says the user reviewed something they did not.
    ///
    /// Every site that mutates the ledger's content calls this, and here is
    /// why that is all of them. `record_write`
    /// and `forget_path` both run under the fs-watch drain, behind
    /// `attribute_writes_to_working_agents`'s return; `mark_reviewed` under
    /// `mark_agent_file_reviewed`; `mark_lane_reviewed` under
    /// `mark_agent_lane_reviewed`; `forget_path` again from
    /// `remove_workspace_folder`. Of the ledger's other `&mut self`
    /// methods, `note_dropped_events` and `settle_if_empty` move only the
    /// incomplete flag and cannot change the set, and `forget` is reachable
    /// only behind a guard that requires the lane to be empty already.
    pub(super) fn sync_agent_lane_decorations(&mut self) {
        let paths = self.agent_ledger.unreviewed_paths();
        // Skip the Arc swap when both sides are empty, which is the common
        // shape early in a session and after a review clears the queue.
        if paths.is_empty() && self.tree.agent_touched.is_empty() {
            return;
        }
        self.tree.agent_touched = std::sync::Arc::new(paths);
    }

    /// Snapshot `bytes` as `path`'s review baseline and return the millis
    /// that names it (#345).
    ///
    /// The store is only ever written by croft's OWN saves; the agent-write
    /// path that creates lane rows records nothing. So adopting whatever
    /// snapshot happened to be newest anchored the review to an unrelated
    /// earlier save and then labelled the diff "(reviewed)" — the exact
    /// "baseline other than the one claimed" the diff refuses elsewhere.
    /// Recording here is what makes the anchor name the reviewed content.
    ///
    /// `record_in` dedupes identical content and, inside the merge window,
    /// REPLACES the newest entry instead of appending — so the snapshot that
    /// ends up holding these bytes need not be at `millis`. Read the store
    /// back rather than assuming, and return `None` when nothing holds them:
    /// history is best-effort, and a row with no anchor is still reviewed,
    /// it just cannot show a diff.
    pub(super) fn record_review_baseline(
        history_root: &Path,
        path: &Path,
        bytes: &[u8],
    ) -> Option<u64> {
        let millis = now_millis();
        // Kept: an autosave seconds later must not replace the baseline the
        // review diff points at, nor may this replace the user's own save.
        let _ = crate::history::record_kept_in(history_root, path, bytes, millis);
        crate::history::entries_in(history_root, path)
            .into_iter()
            .find(|s| std::fs::read(&s.file).is_ok_and(|held| held == bytes))
            .map(|s| s.millis)
    }

    /// Agents: Review a Changed File (#345): every lane's files in a picker,
    /// labelled as the AGENT LANE section labels them; Enter opens the
    /// chosen file's diff since review, as a click on its row does.
    pub(super) fn pick_agent_lane_file(&mut self) {
        use crate::widgets::agent_lane::LaneRow;
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let mut rows = Vec::new();
        self.pending_lane_picks.clear();
        for row in self.agent_lane_panel_rows() {
            if let LaneRow::File {
                agent,
                path,
                label,
                unreviewed,
                changes,
            } = row
            {
                let mark = if unreviewed { "\u{25cf}" } else { " " };
                let counts = changes
                    .filter(|&(a, r)| a + r > 0)
                    .map(|(a, r)| format!("  +{a} \u{2212}{r}"))
                    .unwrap_or_default();
                rows.push(ListRow {
                    id: self.pending_lane_picks.len().to_string(),
                    label: format!("{mark} {label}  ({agent}){counts}"),
                });
                self.pending_lane_picks.push((agent, path));
            }
        }
        if rows.is_empty() {
            self.status = String::from("No agent has changed a file");
            return;
        }
        self.list_picker = Some(ListPicker::new(
            ListPurpose::AgentLaneFile,
            String::from("Review a file an agent changed"),
            rows,
        ));
    }

    /// Diff one agent-lane row against the snapshot that was its baseline
    /// when the user last marked it reviewed (#345), rather than against
    /// HEAD.
    ///
    /// Refuses rather than falling back to a different baseline. A row with
    /// no recorded snapshot (never reviewed, or reviewed before the store
    /// had one) and a row whose snapshot has since been PRUNED — the store
    /// keeps 50 per file against a 1s autosave, so this is a live case, not
    /// only a legacy one — both say so. A diff against a baseline other than
    /// the one claimed is worse than no diff, because the user cannot see
    /// which one they got.
    pub(crate) fn diff_agent_lane_row(&mut self, agent: &str, path: &Path) {
        // "Restore Snapshot" writes to the path recorded here, and only
        // `open_timeline_diff` cleared it. Browsing a.txt's timeline and then
        // opening this diff on b.txt left restore armed at a.txt, so the
        // command would overwrite a file the user was not even looking at.
        // Disarm on every exit below, including the refusals: a refused diff
        // leaves the PREVIOUS diff on screen, and arming restore to that is
        // exactly the stale pairing this guards.
        self.history_restore = None;
        let Some(row) = self
            .agent_ledger
            .lane(agent)
            .into_iter()
            .find(|f| f.path == path)
        else {
            self.status = format!("{} is not in {agent}'s lane", path.display());
            return;
        };
        let Some(millis) = row.reviewed_millis else {
            self.status = if row.reviewed_hash.is_some() {
                format!(
                    "{} was reviewed before a snapshot existed - nothing to diff against",
                    path.display()
                )
            } else {
                format!("{} has not been reviewed yet", path.display())
            };
            return;
        };
        let Some(snap_file) = crate::history::snapshot_file_in(&self.history_root, path, millis)
        else {
            self.status = format!(
                "The snapshot {} was reviewed against is no longer available",
                path.display()
            );
            return;
        };
        let Ok(content) = std::fs::read(&snap_file) else {
            self.status = format!("Could not read the reviewed snapshot of {}", path.display());
            return;
        };
        // Snapshots hold raw bytes; decode like the local-history diff above
        // so a UTF-16 file diffs as text rather than mojibake.
        let enc = encoding_rs::Encoding::for_bom(&content)
            .map(|(e, _)| e)
            .unwrap_or(encoding_rs::UTF_8);
        let text = enc.decode(&content).0.into_owned();
        let rel = match path.strip_prefix(&self.tree.root) {
            Ok(r) => r.to_string_lossy().into_owned(),
            Err(_) => path.display().to_string(),
        };
        let label = std::path::PathBuf::from(format!("{rel} (reviewed)"));
        let unsaved = self.unsaved_text_in_any_group(path);
        if let Err(e) = self
            .editor
            .open_head_diff_with_text_resolved(label, &text, path, false, unsaved)
        {
            self.status = format!("Could not open the reviewed diff: {e}");
            return;
        }
        self.tag_open_diff(crate::widgets::diff::DiffSource::FixedLeft { left_text: text });
        // Armed to the snapshot now on screen, matching `open_timeline_diff`:
        // "Restore Snapshot" puts back the version the user reviewed.
        self.history_restore = Some((path.to_path_buf(), millis, content));
        self.focus_pane(Pane::Editor);
        self.status = format!("{rel}: changes since you last reviewed it");
    }

    /// Mark one file reviewed in one agent's lane (#345): the content on
    /// DISK now becomes the baseline, so the row returns only on a later
    /// write.
    pub(crate) fn mark_agent_file_reviewed(&mut self, agent: &str, path: &Path) -> bool {
        let changed = self.mark_agent_file_reviewed_inner(agent, path);
        if changed {
            self.sync_agent_lane_decorations();
        }
        changed
    }

    pub(super) fn mark_agent_file_reviewed_inner(&mut self, agent: &str, path: &Path) -> bool {
        // A file too big to hash is too big to snapshot: `read_baseline`
        // stamps those from metadata rather than slurping 4 MiB+, and
        // copying one into the history store on every review would be worse.
        // It reviews with NO anchor - honest, and the diff then says it has
        // nothing to compare against.
        if std::fs::metadata(path).is_ok_and(|m| m.len() > crate::agent_lane::MAX_HASH_BYTES) {
            return match crate::agent_lane::read_baseline(path) {
                crate::agent_lane::Baseline::Hash(hash) => {
                    self.agent_ledger.mark_reviewed(agent, path, hash, None)
                }
                crate::agent_lane::Baseline::Gone => self.agent_ledger.forget_path(path),
                crate::agent_lane::Baseline::Unreadable => {
                    self.status = format!("Could not read {} to mark it reviewed", path.display());
                    false
                }
            };
        }
        // One read, used for BOTH the hash and the snapshot: reading twice
        // lets a write land in between, and the baseline would then be a hash
        // of content the anchor does not hold.
        match std::fs::read(path) {
            Ok(bytes) => {
                let hash = crate::agent_lane::content_hash(&bytes);
                let snap = Self::record_review_baseline(&self.history_root, path, &bytes);
                self.agent_ledger.mark_reviewed(agent, path, hash, snap)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.agent_ledger.forget_path(path)
            }
            // Unreadable right now is NOT reviewed: say so instead of
            // clearing content the user has not seen.
            Err(_) => {
                self.status = format!("Could not read {} to mark it reviewed", path.display());
                false
            }
        }
    }

    /// Mark an agent's whole lane reviewed (#345).
    pub(crate) fn mark_agent_lane_reviewed(&mut self, agent: &str) -> usize {
        let root = self.history_root.clone();
        // Same contract as the single-file path: the anchor must hold the
        // bytes that were hashed, so read once and snapshot THOSE. Reading
        // separately in the two closures would let a write land between them.
        let (reviewed, dropped) = self.agent_ledger.mark_lane_reviewed(agent, |path| {
            // Too big to hash is too big to snapshot: stamp it and
            // review with no anchor, as the single-file path does.
            if std::fs::metadata(path).is_ok_and(|m| m.len() > crate::agent_lane::MAX_HASH_BYTES) {
                return (crate::agent_lane::read_baseline(path), None);
            }
            match std::fs::read(path) {
                Ok(bytes) => (
                    crate::agent_lane::Baseline::Hash(crate::agent_lane::content_hash(&bytes)),
                    Self::record_review_baseline(&root, path, &bytes),
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    (crate::agent_lane::Baseline::Gone, None)
                }
                Err(_) => (crate::agent_lane::Baseline::Unreadable, None),
            }
        });
        // A file that vanished was not REVIEWED; saying so would tell the
        // user they looked at something that is not there.
        let gone = match dropped {
            0 => String::new(),
            1 => String::from(", 1 gone"),
            n => format!(", {n} gone"),
        };
        self.status = match reviewed {
            0 if dropped == 0 => format!("{agent}: nothing waiting for review"),
            0 => format!("{agent}: nothing to review{gone}"),
            1 => format!("{agent}: 1 file marked reviewed{gone}"),
            n => format!("{agent}: {n} files marked reviewed{gone}"),
        };
        if reviewed + dropped > 0 {
            self.sync_agent_lane_decorations();
        }
        reviewed + dropped
    }

    /// Focus the first waiting agent's pane, else the first seated one. Goes
    /// through `set_bottom_panel_tab` like `captures_open_selected`, so a
    /// click from the editor with the panel collapsed reveals the pane
    /// rather than focusing something the user cannot see.
    pub(super) fn focus_agent_pane(&mut self) {
        let pick = self
            .terminals
            .iter()
            .position(|t| {
                t.agent()
                    .is_some_and(|a| a.status == crate::agents::AgentStatus::Waiting)
            })
            .or_else(|| self.terminals.iter().position(|t| t.agent().is_some()));
        match pick {
            Some(idx) => {
                self.reveal_terminal_pane(idx);
                self.set_bottom_panel_tab(BottomPanelTab::Terminal);
            }
            // The chip is showing a review queue whose agents have all gone:
            // a click that silently did nothing was the worst of the three
            // options (#345 review).
            None => self.run_command(crate::widgets::command_palette::Command::ShowAgentLane),
        }
    }

    /// CAPTURES: ask the resident Navigator about the selected captured line
    /// (#372). The line, the trigger that caught it, and the pane rows around
    /// it (redacted like any text that leaves a pane, #360) ride the ask. A
    /// `path:line` in the line opens that file first so the reply lands as
    /// a comment box at the referenced line; otherwise the ask is anchored
    /// on the active file, and with no file at all it says so.
    pub(crate) fn ask_navigator_about_capture(&mut self) {
        let Some(entry) = self.captures.selected_entry().cloned() else {
            self.status = String::from("No captured line is selected");
            return;
        };
        let Some(host) = &self.pair_host else {
            self.status =
                String::from("Navigator is not active (run croft pair in this workspace)");
            return;
        };
        if host.is_busy() {
            self.status = format!("{} is mid-turn; wait for it to finish", host.name());
            return;
        }
        let name = host.name().to_string();
        let (idx, anchor) = self.capture_pane(&entry);
        // The rows around the line, if it is still in the scrollback; the
        // line alone otherwise.
        let needle: String = entry.line.trim_end().chars().take(60).collect();
        let context: Vec<String> = match self.terminals.get(idx) {
            Some(t) => match t.find_captured_line(&needle, anchor) {
                Some(line) => {
                    let (lines, top) = t.grid_lines();
                    let hit = usize::try_from(line - top).unwrap_or(0);
                    crate::widgets::captures::context_window(
                        &lines,
                        hit,
                        crate::widgets::captures::ASK_CONTEXT_ROWS,
                    )
                }
                None => vec![entry.line.clone()],
            },
            None => vec![entry.line.clone()],
        };
        // Anchor: the file the line names, else the active file.
        let referenced = crate::widgets::captures::first_file_ref(&entry.line)
            .and_then(|fr| self.resolve_pane_path(idx, &fr.path).map(|abs| (abs, fr)))
            .filter(|(abs, _)| abs.is_file());
        let opened = match referenced {
            Some((abs, fr)) => self.open_resolved_file_ref(&abs, fr.line, fr.column, fr.end_line),
            None => false,
        };
        let Some(file) = self
            .editor
            .path
            .as_ref()
            .and_then(|p| collab_file_key(&self.tree.root, p))
        else {
            self.status = String::from(
                "Ask Navigator: the captured line names no workspace file and none is open",
            );
            return;
        };
        let row = self.editor.cursor_row;
        let range = (row, row);
        // Every part of the ask is masked here, the line and the trigger
        // message included: a capture is stored raw (#360).
        let triggers = self.triggers.clone();
        let (instruction, selection) =
            crate::widgets::captures::ask_prompt(&entry, &context, |t| {
                crate::triggers::mask_text(t, &triggers, false)
            });
        let content = self.editor.lines.join("\n");
        let Some(host) = &self.pair_host else {
            // Unreachable today (nothing above touches the host), but a
            // silent return would leave the jump's status reading as success.
            self.status = String::from("Navigator is not active");
            return;
        };
        match host.send_ask_turn(&file, range, &selection, &instruction, &content) {
            Ok(()) => {
                // Always name the anchor: when the line's own file did not
                // open, the ask went to the active file and the user must
                // see which.
                self.status = format!(
                    "Asked {name} about the captured line, anchored at {file}:{}{}",
                    row + 1,
                    if opened { "" } else { " (the active file)" }
                );
                self.pair_turn_origin = Some((file, row));
            }
            Err(e) => self.status = format!("Ask Navigator failed: {e}"),
        }
    }

    /// PROBLEMS: Fix with Navigator (#374). Opens the file at the diagnostic
    /// and sends the navigator an ask turn built from the diagnostic
    /// (message, source, position) with the server's quick fixes as
    /// `hints`, anchored on the diagnostic's line. The navigator's edit
    /// streams into the buffer under its caret exactly as any ask does, so
    /// every safety property of that path holds: visible, cancellable with
    /// `Cmd+K X` (which reverts the streamed text), never written to disk by
    /// the navigator. The row wears a spinner until the turn ends, and the
    /// server's next publish clears or updates it.
    ///
    /// With no navigator seated the action says so rather than doing
    /// nothing; the menu model has no disabled state (#372's shape).
    pub(crate) fn fix_problem_with_navigator(
        &mut self,
        path: PathBuf,
        item: crate::widgets::problems::ProblemItem,
        hints: Vec<String>,
    ) {
        let Some(host) = &self.pair_host else {
            self.status =
                String::from("Navigator is not active (run croft pair in this workspace)");
            return;
        };
        if host.is_busy() {
            self.status = format!("{} is mid-turn; wait for it to finish", host.name());
            return;
        }
        let name = host.name().to_string();
        // Land on the diagnostic first: the turn is anchored on the active
        // file, and the navigator's caret parks where the user is looking.
        let opened = if item.col_utf16 {
            self.open_at_utf16(&path, item.line, item.col)
        } else {
            self.open_at(&path, item.line as usize, item.col as usize)
        };
        if let Err(e) = opened {
            self.status = format!("Fix with Navigator: open failed: {e}");
            return;
        }
        let Some(file) = self
            .editor
            .path
            .as_ref()
            .and_then(|p| collab_file_key(&self.tree.root, p))
        else {
            self.status = String::from("Fix with Navigator: the file is outside the workspace");
            return;
        };
        let row = (item.line as usize).min(self.editor.lines.len().saturating_sub(1));
        let selection = self.editor.lines.get(row).cloned().unwrap_or_default();
        let instruction = crate::widgets::problems::fix_prompt(&item, &file, &hints);
        let content = self.editor.lines.join("\n");
        let Some(host) = &self.pair_host else {
            // Unreachable today (the open above does not touch the seat),
            // but a silent return would leave the jump's status as success.
            self.status = String::from("Navigator is not active");
            return;
        };
        match host.send_ask_turn(&file, (row, row), &selection, &instruction, &content) {
            Ok(()) => {
                let what: String = item
                    .message
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(60)
                    .collect();
                self.status = format!(
                    "Asked {name} to fix {file}:{}: {what} (Cmd+K X cancels)",
                    row + 1
                );
                self.pair_turn_origin = Some((file, row));
                self.problems.fixing = Some((path, item.line));
                self.problems_fix_started = Some(std::time::Instant::now());
            }
            Err(e) => self.status = format!("Fix with Navigator failed: {e}"),
        }
    }

    /// The diagnostic under the caret in the active file, as a PROBLEMS
    /// item, for the `Cmd+.` and palette entry points of Fix with Navigator
    /// (#374). The first diagnostic covering the caret row wins; its source
    /// is recovered from the per-server store, since the editor's overlay
    /// does not carry it.
    pub(crate) fn navigator_fix_candidate(
        &self,
    ) -> Option<(PathBuf, crate::widgets::problems::ProblemItem)> {
        let path = self.editor.path.clone()?;
        let row = self.editor.cursor_row as u32;
        let d = self
            .editor
            .diagnostics_in_line_range(row, row)
            .into_iter()
            .next()?;
        let source = self
            .lsp_diagnostics
            .get(&path)
            .and_then(|by_server| {
                by_server.iter().find_map(|(server, diags)| {
                    diags
                        .iter()
                        .any(|x| {
                            x.start_line == d.start_line
                                && x.start_char == d.start_char
                                && x.message == d.message
                        })
                        .then(|| diagnostics_source(server).to_string())
                })
            })
            .unwrap_or_default();
        Some((
            path,
            crate::widgets::problems::ProblemItem {
                line: d.start_line,
                col: d.start_char,
                col_utf16: true,
                severity: d.severity,
                message: d.message,
                source,
                code: d.code,
                related: Vec::new(),
            },
        ))
    }

    /// Stop the PROBLEMS-row spinner: the fix turn ended, was cancelled, or
    /// the seat is gone (#374).
    pub(super) fn clear_problem_fix(&mut self) {
        self.problems.fixing = None;
        self.problems_fix_started = None;
    }

    /// Monotonic phase while a Fix with Navigator turn streams, so the loop
    /// repaints its spinner; `0` otherwise, so it never wakes the renderer
    /// idle (the same contract as the update spinner).
    pub(super) fn problems_fix_spinner_phase(&self) -> u128 {
        match self.problems_fix_started {
            Some(t) => t.elapsed().as_millis() / UPDATE_SPINNER_FRAME_MS,
            None => 0,
        }
    }

    /// `root`'s saved agent review queue (#345), less files deleted while
    /// croft was closed; an empty one when nothing is saved or there is no
    /// store.
    pub(super) fn load_agent_ledger(&self, root: &Path) -> crate::agent_lane::AgentLedger {
        let Some(store) = self.agent_ledger_store.as_ref() else {
            return crate::agent_lane::AgentLedger::new();
        };
        let map: std::collections::HashMap<String, crate::agent_lane::AgentLedger> =
            std::fs::read_to_string(store)
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
        map.get(&agent_ledger_key(root))
            .cloned()
            .map(|l| l.restored(Path::exists))
            .unwrap_or_default()
    }

    /// Save the agent review queue under this workspace's key when it has
    /// changed since the last save: at most once a second from the frame
    /// loop, or at once when `now` (leaving the workspace). An emptied
    /// queue removes the key rather than keeping an empty entry.
    pub(super) fn persist_agent_ledger(&mut self, now: bool) {
        let Some(store) = self.agent_ledger_store.clone() else {
            return;
        };
        let generation = self.agent_ledger.generation();
        let (saved, at) = self.agent_ledger_saved;
        if generation == saved || (!now && at.elapsed() < std::time::Duration::from_secs(1)) {
            return;
        }
        let key = agent_ledger_key(self.workspace_root());
        let ledger = self.agent_ledger.clone();
        let result = crate::workspace::update_json_store::<crate::agent_lane::AgentLedger, _>(
            &store,
            |map| {
                if ledger.is_empty() {
                    map.remove(&key);
                } else {
                    map.insert(key.clone(), ledger.clone());
                }
            },
        );
        if let Err(e) = result {
            crate::output::push(
                "Agents",
                crate::output::OutputLevel::Error,
                &format!("Could not save the agent review queue: {e}"),
            );
        }
        self.agent_ledger_saved = (generation, std::time::Instant::now());
    }
}

/// The TurnDone status fragment for a turn's notes: the file is named only
/// when every note landed in the SAME file. "3 comments in b.rs" was a lie
/// when two of them sat in a.rs (F4 only cycles the active file, so the
/// name tells the user where to look).
pub(super) fn turn_note_summary(
    notes: usize,
    files: &std::collections::BTreeSet<String>,
) -> String {
    let plural = if notes == 1 { "comment" } else { "comments" };
    match files.len() {
        0 => format!("{notes} {plural}"),
        1 => format!(
            "{notes} {plural} in {}",
            files.iter().next().expect("len checked")
        ),
        n => format!("{notes} {plural} across {n} files"),
    }
}

/// Index of the next navigator note to visit from `here` = (row, index of
/// the currently focused note, or `usize::MAX` when the walk starts from a
/// bare caret row): the note with the smallest (row, index) strictly greater
/// than `here`, wrapping to the smallest overall. The index tie-break makes
/// every note on a shared row reachable - the reply flow anchors the
/// navigator's answer on the answered note's own row, and a row-only walk
/// could never step from the first co-anchored note to the second.
pub(super) fn next_note_by_row(notes: &[(u64, usize, String)], here: (usize, usize)) -> usize {
    notes
        .iter()
        .enumerate()
        .filter(|(i, (_, row, _))| (*row, *i) > here)
        .min_by_key(|(i, (_, row, _))| (*row, *i))
        .or_else(|| {
            notes
                .iter()
                .enumerate()
                .min_by_key(|(i, (_, row, _))| (*row, *i))
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// The text a lane's Explorer root row wears after its name (#348): the
/// branch, then the seated agent as the pane pill shows it (`◆ claude ●`),
/// or the configured agent's bare name while nothing is seated yet, or
/// nothing more for a plain-shell lane.
pub(super) fn lane_root_badge(
    branch: &str,
    seated: Option<&crate::agents::AgentLane>,
    configured: Option<&str>,
) -> String {
    match (seated, configured) {
        (Some(agent), _) => format!(
            "{branch} \u{b7} \u{25c6} {} {}",
            agent.name,
            agent.status.glyph()
        ),
        (None, Some(agent)) => format!("{branch} \u{b7} {agent}"),
        (None, None) => branch.to_string(),
    }
}

/// What a lane pane's fresh shell is fed to start its agent (#348): the
/// line cleared first, as `run_project_task` does, so a half-typed line
/// cannot concatenate with the command.
pub(super) fn lane_launch_input(cmd: &str) -> String {
    format!("{}{cmd}\r", crate::widgets::terminal::CLEAR_TYPED_LINE)
}

/// The per-workspace agent review queues (#345), keyed by
/// [`agent_ledger_key`].
pub(super) fn agent_ledger_store_path() -> PathBuf {
    croft_cache_dir().join("agent_lane.json")
}

/// A workspace's key in the agent review store: its canonical root, so the
/// save and the load agree however the root was spelled.
pub(super) fn agent_ledger_key(root: &Path) -> String {
    std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .display()
        .to_string()
}

/// Action verb parsed from a participant-action row id (`grant:3` etc.).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ParticipantVerb {
    Grant,
    Revoke,
    Kick,
}

/// How long a peer's caret keeps its name tag after it last moved (VS Code
/// Live Share shows the name while the cursor works, then collapses to the
/// bare colored cell).
pub(super) const CARET_LABEL_FADE: std::time::Duration = std::time::Duration::from_secs(2);

/// The typing pause that arms a proactive navigator look: long enough that
/// mid-thought keystrokes never fire it, short enough to feel like a pair
/// partner glancing over (double the 1s auto-save delay).
pub(super) const PROACTIVE_PAUSE: std::time::Duration = std::time::Duration::from_secs(2);

/// A fresh remote undo group id (#1207), unique for the process.
pub(super) fn next_remote_undo_group() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The name this participant broadcasts on its carets: `CROFT_COLLAB_NAME`
/// when set, else the mux's user@host. Truncated here (and again at ingest
/// for remote names) so a tag can never paint a whole row.
pub(super) fn collab_display_name() -> String {
    std::env::var("CROFT_COLLAB_NAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(crate::session_host::client_name)
        .chars()
        .take(24)
        .collect()
}

/// The stable color a participant's ghost caret wears, from a small palette
/// keyed by their id (also the join order, so the first guest is always
/// green, the second magenta, and so on).
pub(super) fn participant_color(id: u64) -> Color {
    const PALETTE: [Color; 6] = [
        Color::Rgb(0x8c, 0xc2, 0x65), // green
        Color::Rgb(0xd6, 0x6b, 0xd0), // magenta
        Color::Rgb(0x56, 0xb6, 0xc2), // cyan
        Color::Rgb(0xe0, 0x9a, 0x4e), // amber
        Color::Rgb(0x9a, 0x86, 0xfd), // purple
        Color::Rgb(0xe0, 0x6c, 0x75), // red
    ];
    PALETTE[(id % PALETTE.len() as u64) as usize]
}

/// The color a collab caret paints in: the seated navigator's carets wear
/// its fixed identity accent (the same orange as its comment boxes) so the
/// AI's presence is unmistakable; humans keep the join-order palette.
/// Identity is the seat's site ids, never its display name — names are
/// neither unique nor length-stable (truncated to 24 chars at ingest).
pub(super) fn collab_caret_color(navigator_sites: &[u64], site: u64) -> Color {
    if navigator_sites.contains(&site) {
        crate::widgets::editor::NAVIGATOR_ACCENT
    } else {
        participant_color(site)
    }
}

/// `path` relative to `repo_root` with forward slashes, as GitHub names it.
pub(super) fn repo_relative(repo_root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(repo_root).ok()?;
    Some(
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// The workspace-relative key a file replicates under in a collab session
/// (docs/MULTIPLAYER.md, Phase D). None outside the workspace: only
/// workspace files are shared.
pub(super) fn collab_file_key(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Parse a participants action-picker row id back into (verb, participant).
pub(super) fn parse_participant_action(id: &str) -> Option<(ParticipantVerb, u64)> {
    let (verb, id) = id.split_once(':')?;
    let id = id.parse().ok()?;
    let verb = match verb {
        "grant" => ParticipantVerb::Grant,
        "revoke" => ParticipantVerb::Revoke,
        "kick" => ParticipantVerb::Kick,
        _ => return None,
    };
    Some((verb, id))
}

/// The roster row shown for one participant: name, window size, role.
pub(super) fn participant_row(
    p: &crate::session_host::Participant,
) -> crate::widgets::list_picker::ListRow {
    let role = if p.control { "write" } else { "read-only" };
    let size = if p.cols > 0 && p.rows > 0 {
        format!("{}\u{d7}{}", p.cols, p.rows)
    } else {
        String::from("size unknown")
    };
    crate::widgets::list_picker::ListRow {
        id: p.id.to_string(),
        label: format!("{}  {}  ({})", p.name, size, role),
    }
}
