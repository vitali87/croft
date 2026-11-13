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
