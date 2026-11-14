//! SSH connect and install, the ssh-pane offer, handing off to a remote croft, Remote
//! Explorer keys, config sync, drop relay and scp uploads, port forwarding, and opening
//! URLs on the local machine.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemotePullKind {
    /// File or directory copied from the user's local Mac into the
    /// inbox; on completion the relay file is moved into `dest_dir`.
    File,
    /// Local-Mac clipboard contents staged at `<inbox>/<id>/clipboard.txt`;
    /// on completion the bytes are pasted into the focused terminal.
    Clipboard,
    /// URL handed to the user's local Mac `open(1)`; on completion croft
    /// just surfaces a status confirmation. No payload is shipped back.
    Open,
    /// Remote loopback port forwarded home over the SSH master; the `.ok`
    /// payload carries the chosen local port, which croft records so the PORTS
    /// panel can label and open it. `remote_port` is the port being forwarded.
    Forward { remote_port: u16 },
}

pub(super) const REMOTE_PULL_TIMEOUT: Duration = Duration::from_secs(120);

impl App {
    pub(super) fn render_local_open_confirm(&self, frame: &mut ratatui::Frame) {
        let Some(url) = &self.pending_local_open else {
            return;
        };
        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(50, 96).min(area.width);
        let height: u16 = 8;
        let x = (area.width.saturating_sub(width)) / 2 + area.x;
        let y = (area.height.saturating_sub(height)) / 2 + area.y;
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        let rect = rect.intersection(area);
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(self.theme.ui(Color::Rgb(0xff, 0xa5, 0x00))))
            .style(Style::default().bg(self.theme.ui(Color::Rgb(0x1e, 0x1e, 0x1e))))
            .title(ratatui::text::Span::styled(
                " OPEN ON LOCAL MAC? ",
                Style::default()
                    .fg(Color::Black)
                    .bg(self.theme.ui(Color::Rgb(0xff, 0xa5, 0x00)))
                    .add_modifier(Modifier::BOLD),
            ));
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(block, rect);
        let inner = Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width.saturating_sub(4),
            height: rect.height.saturating_sub(2),
        };
        let body = ratatui::text::Text::from(vec![
            ratatui::text::Line::from(ratatui::text::Span::styled(
                "This URL will open in YOUR LOCAL MAC's browser via the croft relay.",
                Style::default().fg(self.theme.ui(Color::White)),
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(ratatui::text::Span::styled(
                truncate_for_display(url, inner.width as usize),
                Style::default().fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))),
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(vec![
                ratatui::text::Span::styled(
                    "[Y]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("es once   "),
                ratatui::text::Span::styled(
                    "[A]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("lways for this session   "),
                ratatui::text::Span::styled(
                    "[N]",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("o / Esc"),
            ]),
        ]);
        frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
        crate::widgets::approval_popup::paint_warning_card(
            frame.buffer_mut(),
            rect,
            self.theme,
            self.theme.ui(Color::Rgb(0xff, 0xa5, 0x00)),
        );
    }

    pub(super) fn handle_remote_key(&mut self, key: KeyEvent) {
        let has_mod = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        match key.code {
            KeyCode::Up => self.remote.move_up(),
            KeyCode::Down => self.remote.move_down(),
            KeyCode::PageUp => self.remote.scroll_up(10),
            KeyCode::PageDown => self.remote.scroll_down(10),
            KeyCode::Backspace if !self.remote.filter.is_empty() => {
                self.remote.pop_filter_char();
            }
            KeyCode::Char(' ') if has_mod => {}
            KeyCode::Char(c) if !has_mod => {
                self.remote.push_filter_char(c);
            }
            KeyCode::Enter => {
                if let Some(target) = self.remote.selected_target().cloned() {
                    self.request_remote_launch(target.alias, None);
                }
            }
            KeyCode::Esc if !self.remote.clear_filter() => {
                self.set_sidebar_view(SidebarView::Explorer);
            }
            _ => {}
        }
    }

    /// Remote: Sync Config Now (#262): run `croft sync-config <host>` in a
    /// terminal pane, where ssh can ask for a password and the report of
    /// what was pushed, kept or left for the user to settle stays readable.
    pub(super) fn sync_config_to(&mut self, host: &str) {
        let host = host.trim();
        // Typed into a shell below: an alias is plain, so anything else is
        // refused rather than quoted.
        let plain = !host.is_empty()
            && !host.starts_with('-')
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._@-".contains(c));
        if !plain {
            self.status = format!("{host:?} is not an ssh host alias");
            return;
        }
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| String::from("croft"));
        match crate::widgets::terminal::PtyTerminal::new(self.roots.primary()) {
            Ok(mut term) => {
                term.set_manual_name(Some(format!("config sync: {host}")));
                term.write_input(
                    format!(
                        "{} sync-config {host}\r",
                        crate::remote::shell_quote_for_e_arg(std::path::Path::new(&exe))
                    )
                    .as_bytes(),
                );
                self.insert_terminal(term);
                self.show_terminal = true;
                self.focus_pane(Pane::Terminal);
                self.status = format!("Syncing config to {host}");
            }
            Err(e) => self.status = format!("Could not open a terminal: {e}"),
        }
    }

    /// Housekeeping for the ssh-pane offer (#364), from the top of `render`
    /// like `dress_host_accents`: an offer nobody took or dismissed clears
    /// itself after [`SSH_OFFER_TTL`], and a pane that closed takes its offer
    /// and its per-pane memory along. No system calls: the sampling itself
    /// rides the label thread (see `refresh_terminal_labels`).
    pub(super) fn expire_ssh_offer(&mut self) {
        let now = std::time::Instant::now();
        if self
            .ssh_offer
            .as_ref()
            .is_some_and(|o| now.duration_since(o.since) > SSH_OFFER_TTL)
        {
            self.dismiss_ssh_offer();
        }
        let live: std::collections::BTreeSet<u64> =
            self.terminals.iter().map(|t| t.uid()).collect();
        self.ssh_offer_seen.retain(|uid, _| live.contains(uid));
        if self
            .ssh_offer
            .as_ref()
            .is_some_and(|o| !live.contains(&o.pane))
        {
            self.dismiss_ssh_offer();
        }
    }

    /// Fold the label thread's ssh samples into the offer state (#364):
    /// each `(shell pid, host)` names the pane by its shell and goes through
    /// `consider_ssh_offer` keyed on the pane's uid.
    pub(super) fn apply_ssh_samples(&mut self, samples: Vec<(i32, Option<String>)>) {
        for (shell_pid, host) in samples {
            let Some(uid) = self
                .terminals
                .iter()
                .find(|t| t.shell_pid() == Some(shell_pid))
                .map(|t| t.uid())
            else {
                continue;
            };
            self.consider_ssh_offer(uid, host);
        }
    }

    /// One pane's sampled ssh host, against what it was last seen connected
    /// to (#364). A session is offered when it STARTS: the same host on the
    /// next sample is the same session, not a new reason to prompt. `None`
    /// ends the session; an offer for that pane goes with it.
    pub(super) fn consider_ssh_offer(&mut self, pane: u64, host: Option<String>) {
        let previous = self.ssh_offer_seen.insert(pane, host.clone()).flatten();
        match host {
            None => {
                if self.ssh_offer.as_ref().is_some_and(|o| o.pane == pane) {
                    self.dismiss_ssh_offer();
                }
            }
            Some(host) => {
                if previous.as_deref() == Some(host.as_str()) {
                    return;
                }
                // The pane moved to another host: whatever it was offered
                // for is no longer where it is, offerable or not.
                if self.ssh_offer.as_ref().is_some_and(|o| o.pane == pane) {
                    self.dismiss_ssh_offer();
                }
                if !crate::remote::offer_allowed(
                    &host,
                    self.remote_offer_disabled,
                    &self.remote_offer_excluded,
                    &self.remote_offer_refused,
                    std::time::SystemTime::now(),
                ) {
                    return;
                }
                self.status =
                    format!("Connected to {host} · Open workspace here? (Cmd+K G, Esc dismisses)");
                let cwd_at_offer = self.pane_cwd_report(pane);
                self.ssh_offer = Some(SshOffer {
                    pane,
                    host,
                    since: std::time::Instant::now(),
                    cwd_at_offer,
                });
            }
        }
    }

    /// The pane's last OSC 7 report: its shell's directory and host.
    pub(super) fn pane_cwd_report(&self, pane: u64) -> Option<crate::remote::CwdReport> {
        let term = self.terminals.iter().find(|t| t.uid() == pane)?;
        Some((term.shell_cwd()?, term.shell_host()))
    }

    /// Take the open offer for launching (#364), clearing it: the host, and
    /// the remote shell's directory when the pane reported one that can be
    /// trusted to be on that host (see `remote::remote_workspace_path`).
    pub(super) fn accept_ssh_offer(&mut self) -> Option<(String, Option<String>)> {
        let offer = self.ssh_offer.take()?;
        if self.status.starts_with("Connected to ") {
            self.status.clear();
        }
        let now = self.pane_cwd_report(offer.pane);
        let path = crate::remote::remote_workspace_path(
            &offer.host,
            offer.cwd_at_offer.as_ref(),
            now.as_ref(),
        );
        Some((offer.host, path))
    }

    /// Remember that provisioning `host` failed (#364): the ssh-pane offer
    /// stops prompting for it for `REFUSED_HOST_TTL`. Recorded for any
    /// install that fails, offer-launched or not, because the fact learned
    /// is about the host; and undone by the next success (below), so a box
    /// that merely failed on a bad day is offered again once it works.
    pub(super) fn note_provisioning_failed(&mut self, host: &str, detail: &str) {
        crate::output::push(
            crate::output::CHANNEL_REMOTE,
            crate::output::OutputLevel::Error,
            &format!("Could not install croft on {host}: {detail}"),
        );
        let key = host.to_ascii_lowercase();
        let now = std::time::SystemTime::now();
        self.remote_offer_refused.insert(key.clone(), now);
        let _ = crate::remote::remember_refused_host(
            &crate::remote::refused_hosts_path(&croft_cache_dir()),
            &key,
            now,
        );
    }

    /// A provisioning of `host` succeeded (#364): any remembered refusal is
    /// stale, in memory and on disk. The disk side runs unconditionally:
    /// the file is shared by every croft on the machine, so another
    /// instance may have recorded a refusal this one never loaded, and
    /// `forget_refused_host` is a no-op when there is nothing to forget.
    pub(super) fn note_provisioning_succeeded(&mut self, host: &str) {
        crate::output::push(
            crate::output::CHANNEL_REMOTE,
            crate::output::OutputLevel::Info,
            &format!("croft is installed on {host}; launching the remote workspace"),
        );
        let key = host.to_ascii_lowercase();
        self.remote_offer_refused.remove(&key);
        let _ = crate::remote::forget_refused_host(
            &crate::remote::refused_hosts_path(&croft_cache_dir()),
            &key,
        );
    }

    /// Drop the open offer and its status line, if any (#364).
    pub(super) fn dismiss_ssh_offer(&mut self) {
        if self.ssh_offer.take().is_some() && self.status.starts_with("Connected to ") {
            self.status.clear();
        }
    }

    pub(super) fn request_remote_launch(&mut self, host: String, path: Option<String>) {
        // The launch ends this croft once the remote one is up (#862).
        self.guard_unsaved(UnsavedExit::RemoteLaunch { host, path });
    }

    pub(super) fn start_remote_launch(&mut self, host: String, path: Option<String>) {
        self.status = format!("Connecting to {host}");
        match crate::remote_connect::SshAuth::start(&host) {
            Ok(auth) => {
                self.connect_auth = Some(auth);
                self.connect_dialog = Some(crate::widgets::connect_dialog::ConnectDialog::new(
                    host.clone(),
                ));
                self.pending_remote_launch_path = path;
                self.pending_remote_launch_host = Some(host);
                self.overlays.welcome.request_clear();
            }
            Err(e) => {
                self.status = format!("Could not start ssh: {e}");
            }
        }
    }

    /// Re-root the workspace onto the host the active pane is ssh'd into
    /// (#364).
    ///
    /// Reads the pane's foreground argv rather than its scrollback: what the
    /// user typed may have scrolled away, been edited, or never been typed at
    /// all (a shell function, a `Host` alias resolved by ssh itself), while
    /// argv is what is actually running.
    ///
    /// The foreground PROCESS GROUP LEADER is what identifies the session,
    /// and it is right for the case that matters: `ssh box` typed at a
    /// prompt is its own group leader. A PIPELINE is the limit — `ssh box |
    /// tee log` makes the whole pipeline one group whose leader is not
    /// necessarily the ssh, so the command reports "not an SSH session"
    /// rather than guessing. Measured rather than assumed: on zsh the leader
    /// resolves to the LAST stage. Refusing is the right failure here, since
    /// the alternative is walking the process tree to find an ssh somewhere
    /// under the pane and offering to re-root onto a host the user piped
    /// output from rather than one they are working on.
    ///
    /// Explicit in the first slice, deliberately: the automatic status-bar
    /// offer needed a per-host opt-out, a memory of hosts that refused
    /// provisioning and a decision about how often to prompt, none of which
    /// belonged alongside the detection itself. Those exist now (the second
    /// slice: `consider_ssh_offer` and its gates), and the offer accepts
    /// through the same `request_remote_launch` this command reaches for.
    pub(super) fn open_workspace_on_ssh_host(&mut self) {
        let Some(term) = self.terminals.get(self.active_terminal) else {
            self.status = String::from("No terminal pane is open");
            return;
        };
        let Some(fg) = term.foreground_pid() else {
            self.status = String::from("This pane is not running anything");
            return;
        };
        let Some(cmd) = crate::widgets::terminal::process_cmdline(fg) else {
            self.status = String::from("Could not read what this pane is running");
            return;
        };
        let argv: Vec<&str> = cmd.iter().map(String::as_str).collect();
        let targets = crate::remote::discover_ssh_targets();
        match crate::remote::ssh_reroot_decision(&argv, &targets) {
            Ok(host) => {
                let alias = host.alias.clone();
                // The far side's OSC 7 report opens the workspace where the
                // remote shell is. With no earlier report to tell a fresh one
                // from a stale one, it is trusted only when it names this
                // host; otherwise the login directory, as before.
                let uid = self.terminals[self.active_terminal].uid();
                let report = self.pane_cwd_report(uid);
                let path =
                    crate::remote::remote_workspace_path(&alias, report.as_ref(), report.as_ref());
                self.request_remote_launch(alias, path);
            }
            // The message distinguishes "not ssh" from "ssh to a box croft
            // has no config entry for": the second names the host, because
            // adding it to ~/.ssh/config is the action available.
            Err(why) => self.status = why,
        }
    }

    pub fn poll_connect_dialog(&mut self) -> bool {
        let Some(auth) = self.connect_auth.as_mut() else {
            return false;
        };
        let events = auth.drain_events();
        if events.is_empty() {
            return false;
        }
        let Some(dialog) = self.connect_dialog.as_mut() else {
            return false;
        };
        let mut authenticated = false;
        let mut failed: Option<String> = None;
        for ev in events {
            match ev {
                crate::remote_connect::SshAuthEvent::Log(chunk) => {
                    dialog.push_log(&chunk);
                }
                crate::remote_connect::SshAuthEvent::Prompt { kind, line } => {
                    dialog.set_prompt(kind, line);
                }
                crate::remote_connect::SshAuthEvent::Authenticated => {
                    dialog.set_installing();
                    authenticated = true;
                }
                crate::remote_connect::SshAuthEvent::Failed(detail) => {
                    failed = Some(detail);
                }
            }
        }
        if let Some(detail) = failed {
            dialog.set_failed(detail);
            self.tear_down_connect_auth();
        }
        if authenticated
            && let (Some(auth), Some(host)) = (
                self.connect_auth.take(),
                self.pending_remote_launch_host.take(),
            )
        {
            let adopted = auth.hand_off();
            let path = self.pending_remote_launch_path.take();
            self.install_session = Some(crate::install_session::InstallSession::start(
                adopted,
                host.clone(),
                path,
            ));
            crate::output::push(
                crate::output::CHANNEL_REMOTE,
                crate::output::OutputLevel::Info,
                &format!(
                    "Preparing croft on {host}: checking for an install, provisioning if missing"
                ),
            );
            self.status = format!("Preparing remote croft on {host}");
        }
        true
    }

    pub fn poll_install_session(&mut self) -> bool {
        let Some(session) = self.install_session.as_mut() else {
            return false;
        };
        let events = session.drain_events();
        let mut changed = !events.is_empty();
        let mut can_launch = false;
        let mut done = false;
        let mut failure: Option<String> = None;
        for ev in events {
            match ev {
                crate::install_session::InstallEvent::Log(line) => {
                    if let Some(dialog) = self.connect_dialog.as_mut() {
                        dialog.push_log(&line);
                    }
                }
                crate::install_session::InstallEvent::CanLaunch => can_launch = true,
                crate::install_session::InstallEvent::FallbackPrompt { reason } => {
                    if let Some(dialog) = self.connect_dialog.as_mut() {
                        // What a yes installs, shown before the answer (#950).
                        dialog.set_fallback_prompt(
                            &reason,
                            &crate::remote::REMOTE_COMPILE_SIDE_EFFECTS,
                        );
                    } else if let Some(session) = self.install_session.as_ref() {
                        // No dialog to ask through: decline so the installer
                        // errors out with the `croft setup-cross` guidance
                        // instead of blocking forever on an answer.
                        session.respond_fallback(false);
                    }
                }
                crate::install_session::InstallEvent::Done(Ok(())) => done = true,
                crate::install_session::InstallEvent::Done(Err(detail)) => {
                    failure = Some(detail);
                }
            }
        }
        // Once we've handed the terminal to the remote croft, the install
        // keeps running in a detached thread and signals completion to that
        // remote process via the install-stamp it watches; nothing here.
        if self.remote_launch.is_some() {
            return changed;
        }
        // A croft is already on the remote: drop the user into it now and
        // let the (re)install finish in the background. The running remote
        // croft re-execs into the new binary when the stamp advances.
        if can_launch {
            if let Some(session) = self.install_session.as_mut()
                && let Some(adopted) = session.take_adopted()
            {
                let launch = RemoteLaunch {
                    host: session.host.clone(),
                    path: session.path.clone(),
                    adopted: Some(adopted),
                };
                self.hand_off_to_remote(launch);
            }
            return true;
        }
        if let Some(detail) = failure {
            // Remember the host as one croft could not provision (#364), so
            // the ssh-pane offer stops prompting for it for a while. Kept
            // beside the failure it learns from, and best-effort: a write
            // that fails costs one repeated offer.
            if let Some(host) = self.install_session.as_ref().map(|s| s.host.clone()) {
                self.note_provisioning_failed(&host, &detail);
            }
            if let Some(dialog) = self.connect_dialog.as_mut() {
                dialog.set_failed(detail.clone());
            }
            self.install_session = None;
            self.status = format!("Remote install failed: {detail}");
            changed = true;
        } else if done {
            if let Some(mut session) = self.install_session.take()
                && let Some(adopted) = session.take_adopted()
            {
                let launch = RemoteLaunch {
                    host: session.host.clone(),
                    path: session.path.clone(),
                    adopted: Some(adopted),
                };
                self.hand_off_to_remote(launch);
            }
            changed = true;
        }
        changed
    }

    /// The croft on the remote is ready: quit so the terminal goes to it
    /// (the main loop launches `launch` once this one has let go). The quit
    /// ends this croft, so edits made since the connect was asked for are
    /// asked about first (#862); what the user already chose to leave then
    /// goes without asking again.
    pub(super) fn hand_off_to_remote(&mut self, launch: RemoteLaunch) {
        let host = launch.host.clone();
        self.note_provisioning_succeeded(&host);
        self.remote_launch = Some(launch);
        self.status = format!("Launching croft on {host}");
        self.connect_dialog = None;
        let handoff = UnsavedExit::RemoteHandoff { host };
        let left_as_agreed = self
            .remote_handoff_ok
            .take()
            .is_some_and(|ok| Some(ok) == self.unsaved_stamp());
        if left_as_agreed {
            self.finish_unsaved_exit(handoff);
        } else {
            self.guard_unsaved(handoff);
        }
    }

    pub(super) fn tear_down_connect_auth(&mut self) {
        if let Some(mut auth) = self.connect_auth.take() {
            auth.cancel();
        }
        self.install_session = None;
        self.pending_remote_launch_host = None;
        self.pending_remote_launch_path = None;
        self.remote_handoff_ok = None;
    }

    pub fn handle_connect_dialog_key(&mut self, key: KeyEvent) {
        let Some(dialog) = self.connect_dialog.as_mut() else {
            return;
        };
        if dialog.phase.is_terminal() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char(' ')) {
                self.connect_dialog = None;
                self.tear_down_connect_auth();
            }
            return;
        }
        let has_mod = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        match key.code {
            KeyCode::Esc => {
                self.connect_dialog = None;
                self.tear_down_connect_auth();
                self.status = String::from("Connect cancelled");
            }
            KeyCode::Enter => {
                if !dialog.phase.awaits_input() {
                    return;
                }
                if matches!(
                    dialog.phase,
                    crate::widgets::connect_dialog::DialogPhase::AwaitingFallbackConfirmation
                ) {
                    let payload = dialog.input_for_submit();
                    self.answer_install_fallback(&payload);
                    return;
                }
                let payload = dialog.input_for_submit();
                dialog.submitted = true;
                dialog.status_line = format!("Verifying credentials with {}", dialog.host);
                dialog.clear_input();
                if let Some(auth) = self.connect_auth.as_mut() {
                    auth.respond_to_prompt(&payload);
                }
            }
            KeyCode::Backspace => dialog.pop_input_char(),
            KeyCode::Char('l') | KeyCode::Char('L') if has_mod || !dialog.phase.awaits_input() => {
                dialog.toggle_logs();
            }
            KeyCode::Char(c) if !has_mod => dialog.push_input_char(c),
            _ => {}
        }
    }

    pub fn handle_connect_dialog_click(&mut self, col: u16, row: u16) -> bool {
        let Some(dialog) = self.connect_dialog.as_mut() else {
            return false;
        };
        if rect_contains(dialog.toggle_logs_btn, col, row) {
            dialog.toggle_logs();
            return true;
        }
        if rect_contains(dialog.cancel_btn, col, row) {
            self.connect_dialog = None;
            self.tear_down_connect_auth();
            self.status = String::from("Connect cancelled");
            return true;
        }
        if rect_contains(dialog.submit_btn, col, row) && dialog.phase.awaits_input() {
            if matches!(
                dialog.phase,
                crate::widgets::connect_dialog::DialogPhase::AwaitingFallbackConfirmation
            ) {
                let payload = dialog.input_for_submit();
                self.answer_install_fallback(&payload);
                return true;
            }
            let payload = dialog.input_for_submit();
            dialog.submitted = true;
            dialog.status_line = format!(
                "Sending {} chars to {}",
                payload.chars().count(),
                dialog.host
            );
            dialog.clear_input();
            if let Some(auth) = self.connect_auth.as_mut() {
                auth.respond_to_prompt(&payload);
            }
            return true;
        }
        rect_contains(dialog.last_area, col, row)
    }

    /// Resolve the connect dialog's slow-fallback question. "y"/"yes"
    /// continues with the compile-on-remote install; anything else aborts
    /// the connect so the user can run `croft setup-cross` and reconnect
    /// for the seconds-fast prebuilt-binary path.
    pub(super) fn answer_install_fallback(&mut self, payload: &str) {
        let proceed = matches!(payload.trim().to_ascii_lowercase().as_str(), "y" | "yes");
        if proceed {
            if let Some(dialog) = self.connect_dialog.as_mut() {
                // What a yes installs must have been on screen (#950): a
                // window too short to show it all keeps the question open.
                if !dialog.notes.is_empty() && !dialog.notes_shown {
                    dialog.error = Some(String::from(
                        "Make the window taller to read what a remote compile installs, then answer",
                    ));
                    // Emptied, so the y typed again once it fits is a yes,
                    // not "yy".
                    dialog.clear_input();
                    return;
                }
                dialog.set_installing();
            }
            if let Some(session) = self.install_session.as_ref() {
                session.respond_fallback(true);
            }
            return;
        }
        if let Some(session) = self.install_session.as_ref() {
            session.respond_fallback(false);
        }
        self.connect_dialog = None;
        self.tear_down_connect_auth();
        self.status = String::from(
            "Connect cancelled — run `croft setup-cross`, then reconnect for a fast install",
        );
    }

    pub(super) fn open_ssh_config_in_editor(&mut self) {
        let Some(path) = crate::remote::primary_ssh_config_path() else {
            self.status = String::from("Cannot find $HOME/.ssh/config");
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if !path.exists()
            && let Err(e) = std::fs::write(&path, "")
        {
            self.status = format!("Could not create {}: {e}", path.display());
            return;
        }
        match self.editor.open_pinned(&path) {
            Ok(()) => {
                self.focus_pane(Pane::Editor);
                self.status = format!("Opened {}", self.status_path(&path));
            }
            Err(e) => {
                self.status = format!("Open {} failed: {e}", self.status_path(&path));
            }
        }
    }

    pub(super) fn open_ssh_learn_more(&mut self) {
        let url = "https://man.openbsd.org/ssh_config";
        if self.drop_relay_active() {
            if self.trust_local_browser {
                self.request_remote_url_open(url.to_string());
                self.status = String::from("Opening ssh_config(5) on your local Mac");
            } else {
                self.pending_local_open = Some(url.to_string());
            }
            return;
        }
        match open_url(url) {
            Ok(()) => self.status = String::from("Opened ssh_config(5) docs"),
            Err(e) => self.status = format!("Open link failed: {e}"),
        }
    }

    pub(super) fn refresh_remote_if_config_changed(&mut self) -> bool {
        if self.sidebar_view != SidebarView::Remote {
            return false;
        }
        if !self.remote.refresh_if_config_changed() {
            return false;
        }
        self.status = String::from("Reloaded SSH remotes");
        true
    }

    /// Queue every dropped path for an interactive SCP upload to the
    /// currently selected SSH target. The actual scp invocation happens
    /// in `main_loop` after suspending the alt-screen, so scp inherits
    /// the host shell's stdin / stdout / stderr — that means password
    /// prompts, FIDO touch requests, and host-key confirmations all work
    /// the way the user expects, and the user sees scp's progress and
    /// errors directly. After all uploads finish (success or failure),
    /// croft prompts for Enter and resumes the TUI.
    pub(super) fn import_paths_into_remote(&mut self, paths: &[PathBuf]) {
        let Some(target) = self.remote.selected_target().cloned() else {
            self.status = String::from("Drop ignored: no Remote Explorer host selected");
            return;
        };
        if paths.is_empty() {
            return;
        }
        for src in paths {
            self.pending_scp_uploads.push(ScpUpload {
                alias: target.alias.clone(),
                src: src.clone(),
            });
        }
        self.status = format!(
            "Queued {} item(s) for scp copy to {} (you'll see scp's prompts next)",
            paths.len(),
            target.alias,
        );
    }

    pub fn take_pending_scp_uploads(&mut self) -> Vec<ScpUpload> {
        std::mem::take(&mut self.pending_scp_uploads)
    }

    /// True when this croft is a remote-launched session whose local parent is
    /// running a drop pump. Gated on `CROFT_RELAY_KEY`, set only by a `croft
    /// remote` launch command. The key is the deterministic `hash(launch arg)`
    /// shared with the local pump; because it's deterministic, dtach freezing it
    /// at first launch is harmless — every reconnect recomputes the same value,
    /// so the running croft and a fresh pump always agree on one relay dir. A
    /// session predating this var (an old long-lived dtach session) simply has
    /// no key and reports the relay unavailable rather than timing out.
    pub(super) fn drop_relay_active(&self) -> bool {
        self.relay_dir().is_some()
    }

    /// `$HOME/.cache/croft/relay-<CROFT_RELAY_KEY>/` on this remote box. The key
    /// is `hash(launch arg)` — the same id the dtach socket uses — computed by
    /// the local launcher and the local pump and carried here in env. Keying on
    /// the launch identity (not `workspace_root`) keeps the rendezvous stable
    /// across an in-session workspace change and the updater's self-re-exec,
    /// both of which move `workspace_root` away from where the pump's `pwd`
    /// lands.
    /// `None` when no `CROFT_RELAY_KEY` is set (not a relay-capable session).
    pub(super) fn relay_dir(&self) -> Option<PathBuf> {
        let id = std::env::var("CROFT_RELAY_KEY")
            .ok()
            .filter(|s| !s.is_empty())?;
        Some(croft_cache_dir().join(format!("relay-{id}")))
    }

    pub(super) fn relay_log_path(&self) -> Option<PathBuf> {
        self.relay_dir().map(|d| d.join("requests.log"))
    }

    pub(super) fn relay_inbox_path(&self) -> Option<PathBuf> {
        self.relay_dir().map(|d| d.join("inbox"))
    }

    /// Ensure the relay dir + inbox exist before writing a request. On the first
    /// drop after a dtach reattach (or on a fresh box) the local pump's
    /// `tail -F` may have started against a path this croft hadn't created yet;
    /// `tail -F` retries, so a lazy mkdir here is enough. Idempotent and cheap.
    pub(super) fn ensure_relay_dir(&self) {
        if let Some(inbox) = self.relay_inbox_path() {
            let _ = std::fs::create_dir_all(inbox);
        }
    }

    pub(super) fn request_remote_pulls(&mut self, paths: &[PathBuf]) {
        let dest_dir = self.paste_target_dir();
        let Some(log_path) = self.relay_log_path() else {
            self.status = String::from("Drop relay not available on this remote session");
            return;
        };
        self.ensure_relay_dir();
        let mut lines = String::new();
        let mut staged: Vec<PendingRemotePull> = Vec::new();
        let now = std::time::Instant::now();
        for src in paths {
            let request_id = Self::relay_request_id("pull");
            let basename = src
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| String::from("dropped"));
            lines.push_str("pull\t");
            lines.push_str(&request_id);
            lines.push('\t');
            lines.push_str(&src.to_string_lossy());
            lines.push('\n');
            staged.push(PendingRemotePull {
                request_id,
                src_display: src.to_string_lossy().into_owned(),
                basename,
                dest_dir: dest_dir.clone(),
                started_at: now,
                kind: RemotePullKind::File,
            });
        }
        match append_to_relay_log(&log_path, &lines) {
            Ok(()) => {
                self.status = format!(
                    "Fetching {} item(s) from your local Mac via croft relay",
                    staged.len(),
                );
                self.pending_remote_pulls.extend(staged);
            }
            Err(e) => {
                self.status = format!("Drop relay write failed: {e}");
            }
        }
    }

    /// Check the relay inbox for completed pulls and surface them in the
    /// explorer. Returns true if any pending pull resolved (success,
    /// failure, or timeout) so the main loop knows to redraw.
    pub fn drain_remote_pulls(&mut self) -> bool {
        if self.pending_remote_pulls.is_empty() {
            return false;
        }
        let Some(inbox) = self.relay_inbox_path() else {
            self.pending_remote_pulls.clear();
            self.status = String::from("Drop relay vanished mid-pull");
            return true;
        };
        let mut still_pending: Vec<PendingRemotePull> = Vec::new();
        let mut placed: Vec<PathBuf> = Vec::new();
        let mut opened_urls: Vec<String> = Vec::new();
        let mut forwarded: Vec<(u16, u16)> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut affected: BTreeSet<PathBuf> = BTreeSet::new();
        let pulls = std::mem::take(&mut self.pending_remote_pulls);
        let mut clipboard_payload: Option<Vec<u8>> = None;
        for pull in pulls {
            let request_dir = inbox.join(&pull.request_id);
            let ok = request_dir.join(".ok");
            let err = request_dir.join(".err");
            if ok.exists() {
                match pull.kind {
                    RemotePullKind::File => {
                        let staged_src = request_dir.join(&pull.basename);
                        affected.insert(pull.dest_dir.clone());
                        match crate::widgets::file_tree::move_into(&pull.dest_dir, &staged_src) {
                            Ok(p) => placed.push(p),
                            Err(e) => errors.push(format!("{}: {e}", pull.src_display)),
                        }
                    }
                    RemotePullKind::Clipboard => {
                        let staged = request_dir.join("clipboard.txt");
                        match std::fs::read(&staged) {
                            Ok(bytes) => clipboard_payload = Some(bytes),
                            Err(e) => errors.push(format!("clipboard relay: {e}")),
                        }
                    }
                    RemotePullKind::Open => {
                        opened_urls.push(pull.src_display.clone());
                    }
                    RemotePullKind::Forward { remote_port } => {
                        let local_port = std::fs::read_to_string(&ok)
                            .ok()
                            .and_then(|s| s.trim().parse::<u16>().ok())
                            .unwrap_or(remote_port);
                        forwarded.push((remote_port, local_port));
                    }
                }
                let _ = std::fs::remove_dir_all(&request_dir);
            } else if err.exists() {
                let msg =
                    std::fs::read_to_string(&err).unwrap_or_else(|_| String::from("relay error"));
                errors.push(format!("{}: {}", pull.src_display, msg.trim()));
                let _ = std::fs::remove_dir_all(&request_dir);
            } else if pull.started_at.elapsed() > REMOTE_PULL_TIMEOUT {
                errors.push(format!("{}: timed out after 120s", pull.src_display));
                let _ = std::fs::remove_dir_all(&request_dir);
            } else {
                still_pending.push(pull);
            }
        }
        if let Some(bytes) = clipboard_payload
            && !bytes.is_empty()
        {
            self.paste_terminal_input(&bytes);
        }
        let resolved_any = !placed.is_empty()
            || !opened_urls.is_empty()
            || !forwarded.is_empty()
            || !errors.is_empty();
        for (remote_port, local_port) in &forwarded {
            self.ports.mark_forwarded(*remote_port, *local_port);
        }
        self.pending_remote_pulls = still_pending;
        for dir in &affected {
            if let Some(idx) = self.tree.index_of_dir(dir) {
                self.tree.refresh_children(idx);
            }
        }
        if !placed.is_empty() {
            self.tree.marked.clear();
            for p in &placed {
                self.tree.marked.insert(p.clone());
            }
            if let Some(first) = placed.first()
                && let Some(idx) = self.tree.nodes.iter().position(|n| &n.path == first)
            {
                self.tree.selected = idx;
                self.tree.anchor = idx;
            }
        }
        if resolved_any {
            self.status = if errors.is_empty() {
                if !forwarded.is_empty() && placed.is_empty() && opened_urls.is_empty() {
                    let (remote, local) = forwarded[0];
                    if local == remote {
                        format!("Forwarded port {remote} to your machine")
                    } else {
                        format!("Forwarded remote port {remote} to local {local}")
                    }
                } else if !opened_urls.is_empty() && placed.is_empty() {
                    if opened_urls.len() == 1 {
                        format!("Opened {} in your local browser", opened_urls[0])
                    } else {
                        format!("Opened {} URL(s) in your local browser", opened_urls.len())
                    }
                } else if placed.len() == 1 {
                    format!("Pulled {} from your Mac", placed[0].display())
                } else {
                    format!("Pulled {} item(s) from your Mac", placed.len())
                }
            } else if placed.is_empty() && opened_urls.is_empty() {
                format!("Drop relay failed: {}", errors.join("; "))
            } else {
                format!(
                    "Pulled {} / opened {}; failures: {}",
                    placed.len(),
                    opened_urls.len(),
                    errors.join("; "),
                )
            };
        }
        resolved_any
    }

    pub fn report_scp_results(
        &mut self,
        moved: usize,
        total: usize,
        errors: usize,
        affected_dirs: &[PathBuf],
    ) {
        for dir in affected_dirs {
            if let Some(idx) = self.tree.index_of_dir(dir) {
                self.tree.refresh_children(idx);
            }
        }
        self.status = if errors == 0 {
            format!("Uploaded {moved}/{total} via scp")
        } else {
            format!("Uploaded {moved}/{total} via scp; {errors} error(s)")
        };
    }

    /// Click on a port-detection toast button. Returns whether a button was hit
    /// (and the action taken); a click elsewhere returns false so it falls
    /// through to normal handling.
    pub(super) fn handle_port_toast_click(&mut self, x: u16, y: u16) -> bool {
        let Some(toast) = self.port_toast.as_ref() else {
            return false;
        };
        let Some(action) = toast
            .buttons
            .iter()
            .find(|(r, _)| rect_contains(*r, x, y))
            .map(|(_, a)| *a)
        else {
            return false;
        };
        let port = toast.port;
        self.port_toast = None;
        match action {
            PortToastAction::ForwardAndOpen => self.request_remote_forward(port, true),
            PortToastAction::ForwardOnly => self.request_remote_forward(port, false),
            PortToastAction::Open => match open_url(&format!("http://127.0.0.1:{port}/")) {
                Ok(()) => self.status = format!("Opened http://localhost:{port}/"),
                Err(e) => self.status = format!("Open failed: {e}"),
            },
            PortToastAction::Dismiss => {}
        }
        true
    }

    /// A relay request id: the kind, this process, and a per-process counter.
    /// The relay keys its remote inbox directory by this id, so two requests
    /// raised in the same instant must not share one — the first reply would
    /// satisfy both pending pulls and delete the directory out from under the
    /// second, which then times out with its tunnel already up.
    pub(super) fn relay_request_id(kind: &str) -> String {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        // A per-run nonce as well as the pid: claims outlive the process in
        // the relay dir, and a later croft reusing this pid (after a reboot,
        // in a container) would find every one of its ids already claimed.
        static NONCE: std::sync::OnceLock<u128> = std::sync::OnceLock::new();
        let nonce = NONCE.get_or_init(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        });
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("{kind}-{}-{nonce:x}-{n}", std::process::id())
    }

    /// Ask the local launcher (via the drop relay) to forward `port` home over
    /// the live SSH master, optionally opening the local browser once it's up.
    pub(super) fn request_remote_forward(&mut self, port: u16, open: bool) {
        // Forwarded already: reuse the tunnel. Asking again found the remote
        // port held locally by croft's own first tunnel and fell back to a
        // random one, so every click on the same link opened one more tunnel
        // and one more browser tab at a new 127.0.0.1 address (#648).
        if let Some(local) = self.ports.forwarded_local_port(port) {
            if open {
                self.request_remote_url_open(format!("http://127.0.0.1:{local}/"));
                self.status = format!("Opening port {port} (forwarded to {local})");
            } else {
                self.status = format!("Port {port} is already forwarded to {local}");
            }
            return;
        }
        // And one request per port at a time: a second click before the
        // first forward answered sent a duplicate.
        if self.pending_remote_pulls.iter().any(
            |p| matches!(p.kind, RemotePullKind::Forward { remote_port } if remote_port == port),
        ) {
            self.status = format!("Port {port} is already being forwarded");
            return;
        }
        let Some(log_path) = self.relay_log_path() else {
            self.status = String::from("Forward port: drop relay vanished");
            return;
        };
        self.ensure_relay_dir();
        let request_id = Self::relay_request_id("forward");
        let line = format!("forward\t{request_id}\t{port}\t{}\n", u8::from(open));
        match append_to_relay_log(&log_path, &line) {
            Ok(()) => {
                self.pending_remote_pulls.push(PendingRemotePull {
                    request_id,
                    src_display: format!("port {port}"),
                    basename: String::new(),
                    dest_dir: PathBuf::new(),
                    started_at: std::time::Instant::now(),
                    kind: RemotePullKind::Forward { remote_port: port },
                });
                self.status = format!("Forwarding port {port}…");
            }
            Err(e) => self.status = format!("Forward port: relay write failed: {e}"),
        }
    }

    /// Tear down a previously established forward. Fire and forget — the entry
    /// is already gone from the panel, so there's no completion to await.
    pub(super) fn request_remote_unforward(&mut self, local: u16, remote: u16) {
        let Some(log_path) = self.relay_log_path() else {
            return;
        };
        self.ensure_relay_dir();
        let request_id = Self::relay_request_id("unforward");
        let line = format!("unforward\t{request_id}\t{local}\t{remote}\n");
        let _ = append_to_relay_log(&log_path, &line);
    }

    pub(super) fn request_remote_url_open(&mut self, url: String) {
        let Some(log_path) = self.relay_log_path() else {
            self.status = String::from("Open link: drop relay vanished");
            return;
        };
        self.ensure_relay_dir();
        let request_id = Self::relay_request_id("open");
        let mut line = String::from("open\t");
        line.push_str(&request_id);
        line.push('\t');
        line.push_str(&url);
        line.push('\n');
        match append_to_relay_log(&log_path, &line) {
            Ok(()) => {
                self.pending_remote_pulls.push(PendingRemotePull {
                    request_id,
                    src_display: url,
                    basename: String::new(),
                    dest_dir: PathBuf::new(),
                    started_at: std::time::Instant::now(),
                    kind: RemotePullKind::Open,
                });
            }
            Err(e) => {
                self.status = format!("Open link: relay write failed: {e}");
            }
        }
    }

    /// Handle a key while the local-browser confirmation modal is open.
    /// `Y`/`Enter` opens this URL once. `A` opens it AND remembers
    /// "always" for the rest of the session. `N`/`Esc` cancels.
    pub(super) fn handle_local_open_confirm_key(&mut self, key: KeyEvent) {
        let Some(url) = self.pending_local_open.clone() else {
            return;
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                self.pending_local_open = None;
                self.request_remote_url_open(url.clone());
                self.status = format!("Opening {url} on your local Mac");
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.pending_local_open = None;
                self.trust_local_browser = true;
                self.request_remote_url_open(url.clone());
                self.status = format!("Trusted local browser for this session. Opening {url}");
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.pending_local_open = None;
                self.status = String::from("Open link cancelled");
            }
            _ => {}
        }
    }

    pub(super) fn render_connect_dialog(&mut self, frame: &mut ratatui::Frame) {
        let visible = self.cursor_visible_phase();
        let theme = self.theme;
        let Some(dialog) = self.connect_dialog.as_mut() else {
            return;
        };
        dialog.theme = theme;
        let area = frame.area();
        use ratatui::widgets::Widget as _;
        dialog.render(area, frame.buffer_mut());
        if visible && let Some((cx, cy)) = dialog.cursor_screen_pos() {
            frame.set_cursor_position((cx, cy));
        }
    }
}

/// One compiled per-host accent rule: hostname glob, accent color, badge.
pub(super) type HostAccent = (globset::GlobMatcher, (u8, u8, u8), Option<String>);

/// Compile the prefs' host-accent rules: glob matcher + color (danger red
/// when unset) + badge. A rule with an invalid glob is skipped.
pub(super) fn compile_host_accents(rules: &[crate::prefs::HostAccentRule]) -> Vec<HostAccent> {
    rules
        .iter()
        .filter_map(|r| {
            let m = globset::Glob::new(&r.pattern).ok()?.compile_matcher();
            let color = r
                .accent
                .as_deref()
                .and_then(crate::prefs::parse_hex)
                .unwrap_or((0xf1, 0x4c, 0x4c));
            Some((m, color, r.badge.clone()))
        })
        .collect()
}

pub(super) fn append_to_relay_log(log_path: &Path, payload: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    f.write_all(payload.as_bytes())?;
    f.sync_data().ok();
    Ok(())
}

/// Status-line advisory for a remote croft session that will not survive an
/// SSH transport drop. `croft remote` launches the remote croft under dtach
/// and exports `CROFT_SESSION_PERSISTENT=1`; when that marker is absent on a
/// remote (SSH) session the host has no dtach, so croft tells the user how to
/// turn persistence on. Local sessions never see it. Returned as a status
/// string only (the bottom line is info-only; no input).
pub(super) fn remote_persistence_status(is_remote: bool, persistent: bool) -> Option<&'static str> {
    if is_remote && !persistent {
        Some("Persistence off: install dtach")
    } else {
        None
    }
}

/// How long an ssh-pane offer stays up untouched (#364) before it clears
/// itself: long enough to read and act on, short enough not to be stale
/// when the user next looks at the status bar.
pub(super) const SSH_OFFER_TTL: std::time::Duration = std::time::Duration::from_secs(90);

/// Suspend the alt-screen, run every queued scp upload with the host
/// shell's stdin / stdout / stderr inherited (so the user sees scp's
/// progress and can answer any prompt it raises), then prompt for Enter
/// and restore the alt-screen. The local source is removed only on a
/// successful upload — if scp fails, the local copy stays put so the
/// user can retry without losing data.
pub(super) fn run_pending_scp_uploads(app: &mut App, terminal: &mut CroftTerminal) -> Result<()> {
    use std::io::Write;
    let uploads = app.take_pending_scp_uploads();
    if uploads.is_empty() {
        return Ok(());
    }
    // Tear down the TUI surface so scp can use the real terminal.
    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
        DisableFocusChange,
        crossterm::cursor::SetCursorStyle::DefaultUserShape,
    )
    .ok();
    terminal.show_cursor().ok();

    let total = uploads.len();
    let mut moved = 0usize;
    let mut error_count = 0usize;
    let mut affected_dirs: Vec<PathBuf> = Vec::new();
    {
        let mut out = stdout();
        let _ = writeln!(out);
        let _ = writeln!(out, "── croft: uploading {total} item(s) via scp ──");
        let _ = writeln!(
            out,
            "(scp prompts for password / passphrase / host-key will appear below)"
        );
        let _ = writeln!(out);
        let _ = out.flush();
    }
    for (i, upload) in uploads.iter().enumerate() {
        if let Some(parent) = upload.src.parent() {
            affected_dirs.push(parent.to_path_buf());
        }
        {
            let mut out = stdout();
            let _ = writeln!(
                out,
                "[{}/{}] scp -r {} {}:",
                i + 1,
                total,
                upload.src.display(),
                upload.alias,
            );
            let _ = out.flush();
        }
        if !upload.src.exists() {
            eprintln!("  ! source no longer exists, skipping");
            error_count += 1;
            continue;
        }
        let dest = format!("{}:", upload.alias);
        let status = std::process::Command::new("scp")
            .arg("-r")
            .arg(&upload.src)
            .arg(&dest)
            .status();
        match status {
            Ok(s) if s.success() => {
                moved += 1;
            }
            Ok(s) => {
                eprintln!("  ! scp exited with {s}; local source preserved");
                error_count += 1;
            }
            Err(e) => {
                eprintln!("  ! could not spawn scp: {e}");
                error_count += 1;
            }
        }
    }
    {
        let mut out = stdout();
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "── done: {moved}/{total} uploaded, {error_count} error(s) ──"
        );
        let _ = write!(out, "Press Enter to return to croft ");
        let _ = out.flush();
    }
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);

    // Restore the TUI via the shared takeover block (same modes as startup
    // and the dtach-reattach re-assert).
    enable_raw_mode().ok();
    {
        use std::io::Write;
        let out = terminal.backend_mut();
        let _ = out.write_all(&takeover_mode_seq());
        let _ = out.flush();
    }
    // The takeover block set the steady bar; the next draw puts the
    // overwrite block back if it is wanted.
    app.caret_block_sent = false;
    terminal.clear().ok();
    app.overlays.activity.mark_dirty();
    app.overlays.welcome.mark_dirty();
    app.report_scp_results(moved, total, error_count, &affected_dirs);
    Ok(())
}
