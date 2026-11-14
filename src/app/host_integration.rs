//! The host terminal: escape sequences, restoring it on exit or panic, the panic log,
//! the unsupported-terminal notice, the `croft view` and `edit --wait` socket and
//! rebase-todo editing, self-update and relaunch, and the update card.

use super::*;

/// Why Help: Update croft must not check for a release itself (#1844),
/// for the same reasons the startup check stays off: a remote-launched
/// croft is updated by the croft that launched it, and
/// `CROFT_NO_UPDATE_CHECK` turns checking off altogether.
pub(super) fn manual_update_check_refusal(remote: bool, disabled: bool) -> Option<&'static str> {
    if remote {
        Some("No croft update is waiting: this croft is updated by the croft that launched it")
    } else if disabled {
        Some("No croft update is waiting, and update checks are off (CROFT_NO_UPDATE_CHECK)")
    } else {
        None
    }
}

impl UpdateToast {
    /// The open card offering `release`.
    pub(super) fn offer(release: crate::update_check::Release) -> Self {
        UpdateToast {
            release,
            ready: false,
            failed: None,
            open: true,
            buttons: Vec::new(),
            painted: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum UpdateToastAction {
    /// Stage the new version in the background; nothing on PATH changes.
    /// Labelled Retry once an Update has failed.
    Update,
    /// Never offer this version again.
    Skip,
    /// Fold the card into its status-bar pill. Nothing is remembered, so
    /// the next launch offers the version again.
    Later,
    /// Swap the staged binary in and re-exec - the only path that updates.
    Relaunch,
    /// Copy `brew upgrade croft`, the one upgrade a Homebrew install takes.
    CopyCommand,
    /// Open the release's notes on GitHub.
    ReleaseNotes,
    /// Open a failed staging's log in an editor tab.
    InstallLog,
}

/// Which producer an [`crate::update_watch::UpdateEvent`] came from: the
/// remote install watcher, the drift rebuild, or the staged release build
/// (#333). They share one event type and one status-bar state machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum UpdateSource {
    Watch,
    SelfInstall,
    Staged,
}

/// State of a background self-update observed by a remote-launched croft.
/// `Idle` is the steady state; `InProgress` paints an "Updating" hint in
/// the status bar; `Ready` arms the re-exec into the freshly-shipped binary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum UpdateStatus {
    Idle,
    InProgress,
    Ready,
}

impl App {
    /// Serve one accepted `croft view` client: bound the socket, read the
    /// request, apply it, and answer. Returns the reply and whether anything
    /// was opened (so the caller can redraw).
    ///
    /// Split out of the accept loop so a test can hand it a stream and observe
    /// the REPLY. The previous test for the blocking-mode fix accepted a
    /// connection, called `set_nonblocking(false)` itself, and then asserted
    /// the flag it had just set - it could not fail on any platform, which is
    /// the false-green shape this repo's own guidance names. Given this seam a
    /// test can hand in a deliberately NON-blocking stream and assert the
    /// reply is `Ok`, which fails everywhere if the call below is dropped.
    pub(super) fn answer_view_client(
        &mut self,
        mut stream: std::os::unix::net::UnixStream,
        deadline: std::time::Instant,
    ) -> (crate::view_ipc::ViewReply, bool) {
        // An accepted socket's flags are not portably inherited: Linux's
        // accept(2) says the new socket does NOT inherit O_NONBLOCK and that
        // portable programs must set flags explicitly, while BSD (so macOS,
        // croft's primary platform) does inherit them. Inherited, every read
        // returns EAGAIN before any timeout applies and every `croft view`
        // comes back "unreadable request: Resource temporarily unavailable"
        // while Linux CI stays green.
        let _ = stream.set_nonblocking(false);
        // `read_request` re-arms its own read timeout from the deadline on
        // every recv, so none is set here. The WRITE bound is armed below,
        // after the read, from whatever the read left — and it exists because
        // the reply embeds the client-supplied path, so a client that sends a
        // valid long request and never reads can block `write_all` on a full
        // socket buffer. Not reproducible on Linux, whose default unix-socket
        // buffer is far larger than any legal reply, which is why it is set
        // rather than tested.
        let reply = match crate::view_ipc::read_request(&stream, deadline) {
            Ok(req) if req.probe => self.probe_view_path(&req.to_path()),
            Ok(req) => {
                let reply = self.apply_view_request(&req.to_path());
                // `croft edit file:line[:col]` (#1606).
                if let (crate::view_ipc::ViewReply::Ok, Some((row, col))) = (&reply, req.at) {
                    self.place_caret_at(row, col);
                }
                reply
            }
            Err(e) => crate::view_ipc::ViewReply::Err {
                message: format!("unreadable request: {e}"),
            },
        };
        // Armed AFTER the read, from what the read left: computing it before
        // gives a client that spent the whole budget reading a fresh window to
        // spend writing, so one client could cost the frame loop twice the
        // documented 20ms.
        //
        // The floor is 1ms and the write's result is discarded, which is worth
        // being explicit about rather than leaving as an accident. If that
        // 1ms ever fired on an `Ok`, the file is open in the editor while the
        // client's shell exits 1 saying croft closed without replying: the
        // reply is 16 bytes to a socket with a reader already waiting, so it
        // is unreachable in practice, and the alternative (dropping the reply
        // rather than the timeout) trades a wrong message for no message.
        let left = deadline
            .saturating_duration_since(std::time::Instant::now())
            .max(std::time::Duration::from_millis(1));
        let _ = stream.set_write_timeout(Some(left));
        let opened = matches!(reply, crate::view_ipc::ViewReply::Ok);
        let _ = crate::view_ipc::write_reply(&mut stream, &reply);
        (reply, opened)
    }

    pub(super) fn render_terminal_warning(&self, frame: &mut ratatui::Frame) {
        if !self.pending_terminal_warning {
            return;
        }
        let area = frame.area();
        // The lower clamp bound must never beat the frame itself: on a
        // terminal narrower than 50 columns the modal used to come out
        // WIDER than the buffer and the paint panicked at launch (#192).
        let width = area.width.saturating_sub(8).clamp(50, 96).min(area.width);
        let height: u16 = 11u16.min(area.height);
        let x = (area.width.saturating_sub(width)) / 2 + area.x;
        let y = (area.height.saturating_sub(height)) / 2 + area.y;
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        let rect = rect.intersection(area);
        let warn = self.theme.ui(Color::Rgb(0xff, 0xa5, 0x00));
        let accent = self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff));
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(warn))
            .style(Style::default().bg(self.theme.ui(Color::Rgb(0x1e, 0x1e, 0x1e))))
            .title(ratatui::text::Span::styled(
                " UNSUPPORTED TERMINAL ",
                Style::default()
                    .fg(Color::Black)
                    .bg(warn)
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
        let current = std::env::var("TERM_PROGRAM")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("TERM").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| String::from("this terminal"));
        let body = ratatui::text::Text::from(vec![
            Line::from(Span::styled(
                "Croft draws its icons and images with inline-image protocols",
                Style::default().fg(self.theme.ui(Color::White)),
            )),
            Line::from(Span::styled(
                format!("that {current} does not support, so the activity bar and"),
                Style::default().fg(self.theme.ui(Color::White)),
            )),
            Line::from(Span::styled(
                "file icons stay blank. Croft still runs; for the full UI use:",
                Style::default().fg(self.theme.ui(Color::White)),
            )),
            Line::from(""),
            Line::from(vec![
                Span::raw("  macOS:  "),
                Span::styled(
                    "iTerm2",
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  or  "),
                Span::styled(
                    "Ghostty",
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::raw("  Linux:  "),
                Span::styled(
                    "Ghostty",
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "[any key]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" dismiss    "),
                Span::styled(
                    "[D]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("on't show again"),
            ]),
        ]);
        frame.render_widget(ratatui::widgets::Paragraph::new(body), inner);
        crate::widgets::approval_popup::paint_warning_card(
            frame.buffer_mut(),
            rect,
            self.theme,
            warn,
        );
    }

    /// Paint the update card in the bottom-LEFT corner (the port toast
    /// owns the right), above the status-bar pill it folds into, and record
    /// its controls' rects. Click-only, like the port toast: it never
    /// captures keystrokes (#333). A screen too short for the whole card
    /// gets it without the release-notes link, then without the rows past
    /// Version.
    pub(super) fn render_update_toast(&mut self, frame: &mut ratatui::Frame) {
        let rect = self.paint_update_toast(frame);
        if rect != self.update_card_rect {
            // An icon it now covers must stop being sent, and one it left
            // must come back.
            self.update_card_rect = rect;
            self.overlays.activity.mark_dirty();
        }
    }

    /// `render_update_toast`'s painting: the card's rect, or `None` when
    /// nothing was painted.
    pub(super) fn paint_update_toast(&mut self, frame: &mut ratatui::Frame) -> Option<Rect> {
        let Some(toast) = self.update_toast.as_ref().filter(|t| t.open) else {
            if let Some(t) = self.update_toast.as_mut() {
                t.buttons.clear();
            }
            return None;
        };
        let mut card = self.update_card(toast);
        let area = frame.area();
        let width = crate::widgets::update_popup::WIDTH.min(area.width.saturating_sub(2));
        let status_h: u16 = if self.status_bar_visible { 1 } else { 0 };
        // The port toast owns the bottom-right band; at ordinary widths
        // this popup's title reaches into it, so stack above it instead.
        let stack: u16 = if self.port_toast.is_some() {
            PORT_TOAST_HEIGHT
        } else {
            0
        };
        let room = area.height.saturating_sub(status_h + 1 + stack);
        if card.height(width) > room {
            card.link = None;
            card.rows.truncate(1);
        }
        let height = card.height(width);
        if width < crate::widgets::update_popup::MIN_WIDTH || height > room {
            if let Some(t) = self.update_toast.as_mut() {
                t.buttons.clear();
            }
            return None;
        }
        let rect = Rect {
            x: area.x + 1,
            y: area.y + room - height,
            width,
            height,
        };
        let hits = card.render(frame.buffer_mut(), rect, self.theme, self.pointer_cell);
        let painted = hits
            .iter()
            .map(|(r, _)| cells_in(frame.buffer_mut(), *r))
            .collect();
        if let Some(t) = self.update_toast.as_mut() {
            t.buttons = hits;
            t.painted = painted;
        }
        Some(rect)
    }

    /// Whether the update card covers any of `at`: an inline image there
    /// would be drawn over it, so it is held back while the card is open.
    pub(super) fn update_card_covers(&self, at: Rect) -> bool {
        self.update_card_rect
            .is_some_and(|card| card.intersects(at))
    }

    /// Drop the update card's controls that a later overlay painted over: a
    /// context menu, a prompt, a confirm or a picker. The card's clicks are
    /// taken before theirs, so a covered button would answer a click on what
    /// covers it; what is still on screen is what can be clicked.
    pub(super) fn drop_covered_update_controls(&mut self, buf: &ratatui::buffer::Buffer) {
        let Some(t) = self.update_toast.as_mut() else {
            return;
        };
        let painted = std::mem::take(&mut t.painted);
        let buttons = std::mem::take(&mut t.buttons);
        t.buttons = buttons
            .into_iter()
            .zip(painted)
            .filter(|((r, _), cells)| cells_in(buf, *r) == *cells)
            .map(|(hit, _)| hit)
            .collect();
    }

    /// What the update card says for `toast`: its state's glyph and title,
    /// the release's details, a line of prose where the state needs one,
    /// and Skip / Later / the primary action.
    pub(super) fn update_card(
        &self,
        toast: &UpdateToast,
    ) -> crate::widgets::update_popup::Card<UpdateToastAction> {
        use crate::widgets::update_popup::{
            Card, ERROR_GLYPH, LOG_GLYPH, NOTES_GLYPH, PACKAGE_GLYPH, READY_GLYPH,
        };
        let release = &toast.release;
        let brew = self.install_source == crate::update_check::InstallSource::Homebrew;
        let installed = format!("v{}", env!("CARGO_PKG_VERSION"));
        let mut rows = vec![
            ("Version", format!("v{}", release.version)),
            (
                "Installed",
                if brew {
                    format!("{installed} (Homebrew)")
                } else {
                    installed
                },
            ),
        ];
        if let Some(date) = release
            .published
            .as_deref()
            .and_then(crate::update_check::date_label)
        {
            rows.push(("Released", date));
        }
        if let Some(size) = release.size {
            rows.push(("Size", crate::widgets::editor::format_bytes_human(size)));
        }
        let button = |label: &str, action| (String::from(label), action);
        let skip = button("Skip", UpdateToastAction::Skip);
        let later = button("Later", UpdateToastAction::Later);
        let accent = self.theme.accent();
        let (glyph, glyph_color, title, message, buttons) = if toast.ready {
            (
                READY_GLYPH,
                self.theme.ui(Color::Rgb(0x89, 0xd1, 0x85)),
                "Update Ready",
                Some(String::from(
                    "Relaunching keeps open files and unsaved edits; terminals restart.",
                )),
                vec![later, button("Relaunch", UpdateToastAction::Relaunch)],
            )
        } else if let Some(reason) = toast.failed {
            (
                ERROR_GLYPH,
                self.theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c)),
                "Update Failed",
                Some(format!("Not installed: {reason}.")),
                vec![skip, later, button("Retry", UpdateToastAction::Update)],
            )
        } else if brew {
            // Homebrew upgrades it (#375): the card names the command and
            // copies it.
            (
                PACKAGE_GLYPH,
                accent,
                "Update Available",
                Some(format!(
                    "Homebrew manages this install. Update it with `{}`.",
                    crate::update_check::BREW_UPGRADE
                )),
                vec![
                    skip,
                    later,
                    button("Copy Command", UpdateToastAction::CopyCommand),
                ],
            )
        } else {
            (
                PACKAGE_GLYPH,
                accent,
                "Update Available",
                None,
                vec![skip, later, button("Update", UpdateToastAction::Update)],
            )
        };
        // A failure's own story is in its log; otherwise, what the release
        // brings.
        let link = if toast.failed.is_some() {
            (
                LOG_GLYPH,
                String::from("View the Install Log"),
                UpdateToastAction::InstallLog,
            )
        } else {
            (
                NOTES_GLYPH,
                format!("What's New in v{}", release.version),
                UpdateToastAction::ReleaseNotes,
            )
        };
        Card {
            glyph,
            glyph_color,
            title: String::from(title),
            rows,
            message,
            buttons,
            link: Some(link),
        }
    }

    /// Decide whether to show the unsupported-terminal nudge. Call AFTER
    /// `init_graphics` has resolved `inline_protocol` (including the sixel
    /// promotion), so a sixel host is correctly treated as supported. No-op
    /// when the user has already silenced the nudge in their prefs.
    pub(super) fn arm_terminal_warning(&mut self) {
        if crate::prefs::Prefs::load_or_default().suppress_terminal_warning {
            return;
        }
        self.pending_terminal_warning = crate::iterm2_inline::terminal_warrants_switch_warning(
            self.inline_protocol,
            crate::iterm2_inline::detect_termux(),
        );
    }

    /// Dismiss the unsupported-terminal nudge. `dont_show_again` persists the
    /// dismissal so it never reappears; otherwise it returns next launch.
    pub(super) fn dismiss_terminal_warning(&mut self, dont_show_again: bool) {
        self.pending_terminal_warning = false;
        if dont_show_again {
            self.report_unsaved_setting(crate::prefs::save_suppress_terminal_warning(true));
        }
    }

    /// Start the background self-update watcher on a remote-launched croft.
    /// No-op unless `CROFT_REMOTE_AUTOUPDATE` is set (it is, by the remote
    /// launch command) and an install-stamp exists to compare against.
    pub(super) fn start_update_watch_if_remote(&mut self) {
        if std::env::var_os("CROFT_REMOTE_AUTOUPDATE").is_none() {
            return;
        }
        let cache_dir = croft_cache_dir();
        let launch_stamp = std::fs::read_to_string(cache_dir.join("install-stamp"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        self.update_watch = Some(crate::update_watch::UpdateWatch::start(
            cache_dir,
            launch_stamp,
        ));
    }

    /// Stop the language servers while croft compiles itself on this host,
    /// and start them again once it is done (#694).
    ///
    /// The remote source build writes a marker while it runs. On an 8 GB
    /// host a compile next to rust-analyzer exhausted memory, and the OOM
    /// killer then took whatever was largest — croft, or a terminal pane.
    /// Only a remote-launched croft watches: the build is the fallback of
    /// `croft remote`'s update, and a local croft never runs one under it.
    pub(super) fn poll_source_build_marker(&mut self) -> bool {
        if self.update_watch.is_none() {
            return false;
        }
        let now = std::time::Instant::now();
        if self
            .build_marker_checked
            .is_some_and(|t| now.duration_since(t) < std::time::Duration::from_secs(1))
        {
            return false;
        }
        self.build_marker_checked = Some(now);
        let building = crate::update_watch::source_build_running(
            &croft_cache_dir(),
            crate::update_watch::boot_id,
            crate::update_watch::boot_time,
            process_is_alive,
        );
        self.apply_source_build_state(building)
    }

    /// Pause or resume the language servers for a source build that has
    /// started (`true`) or ended (`false`). Split from the polling so tests
    /// drive it without a marker file or the 1 Hz gate.
    pub(super) fn apply_source_build_state(&mut self, building: bool) -> bool {
        match (building, self.lsp_paused_for_build) {
            (true, false) => {
                // Nothing running is nothing to pause, and nothing to restart
                // later either: a manager that failed to start stays down.
                if self.lsp.is_none() {
                    return false;
                }
                // Dropping the manager shuts its servers down, which is what
                // frees the memory. Its Drop waits up to 3 s for them, so it
                // runs off the UI thread: nothing the user did asked for a
                // freeze (the build script waits long enough for it).
                if let Some(old) = self.lsp.take() {
                    fs_watch::offload_drop(old);
                }
                self.lsp_paused_for_build = true;
                // A format-on-save in flight dies with the manager, and its
                // armed save would otherwise fire on some later, unrelated
                // format reply. Save now, unformatted, as when a reply fails.
                if self.format_request_id.take().is_some() {
                    self.complete_pending_save();
                }
                // Re-open every tab against the servers that come back. The
                // diagnostics are kept: they are still the best answer until
                // the restarted servers publish fresh ones.
                self.lsp_last_seen.clear();
                self.lsp_progress.clear();
                let msg = "Language servers stopped while croft builds its update on this host";
                crate::output::push("Language Servers", crate::output::OutputLevel::Info, msg);
                self.status = String::from(msg);
                true
            }
            (false, true) => {
                self.lsp_paused_for_build = false;
                self.lsp = match crate::lsp::LspManager::new(self.workspace_root().to_path_buf()) {
                    Ok(m) => {
                        m.set_extra_roots(
                            self.roots.iter().skip(1).map(Path::to_path_buf).collect(),
                        );
                        crate::output::push(
                            "Language Servers",
                            crate::output::OutputLevel::Info,
                            "Update build finished; language servers restarted",
                        );
                        Some(m)
                    }
                    Err(e) => {
                        let msg = format!("Language servers could not restart: {e}");
                        crate::output::push(
                            "Language Servers",
                            crate::output::OutputLevel::Error,
                            &msg,
                        );
                        self.status = msg;
                        None
                    }
                };
                self.lsp_last_seen.clear();
                self.lsp_progress.clear();
                // The new manager numbers semantic replies from 0 again; the
                // old high-water marks would reject every reply for a file
                // until the new count caught up with the old one.
                self.semantic_generation_seen.clear();
                // A tab closed during the pause was never in `lsp_last_seen`
                // again, so nothing would drop its diagnostics: drop what no
                // open buffer owns. That includes workspace-scope diagnostics
                // for files never opened, which the old servers published
                // unasked; the new servers publish those again as they index,
                // and until then keeping them would show findings nothing can
                // retract.
                let open = self.open_buffer_paths();
                self.lsp_diagnostics.retain(|p, _| open.contains(p));
                self.rebuild_problems();
                true
            }
            _ => false,
        }
    }

    /// Start the one-shot local drift probe (#242) on a locally-launched
    /// croft. A remote-launched croft skips it: its binary is re-stamped by
    /// the launching machine on every connect, and its baked manifest dir
    /// points at a tree on another box anyway.
    pub(super) fn start_drift_probe_if_local(&mut self) {
        if std::env::var_os("CROFT_REMOTE_AUTOUPDATE").is_some() {
            return;
        }
        self.drift_probe = Some(crate::update_watch::DriftProbe::start(
            String::from(env!("CARGO_MANIFEST_DIR")),
            String::from(env!("CROFT_GIT_HASH_FULL")),
        ));
    }

    /// Start the release-availability check (#333) on a locally-launched
    /// croft. A remote-launched croft is shipped by its launcher; a build
    /// with `CROFT_NO_UPDATE_CHECK` set in the environment never asks.
    pub(super) fn start_update_check_if_local(&mut self) {
        if std::env::var_os("CROFT_REMOTE_AUTOUPDATE").is_some()
            || std::env::var_os("CROFT_NO_UPDATE_CHECK").is_some()
        {
            return;
        }
        self.update_check = Some(crate::update_check::UpdateCheck::start(
            croft_cache_dir(),
            String::from(env!("CARGO_PKG_VERSION")),
        ));
    }

    /// Raise the popup when the check finds a newer, undismissed release.
    /// The check is dropped either way - it only ever answers once.
    pub(super) fn poll_update_check(&mut self) -> bool {
        let Some(check) = self.update_check.as_ref() else {
            return false;
        };
        let Some(verdict) = check.take() else {
            return false;
        };
        self.update_check = None;
        let Some(release) = verdict else {
            return false;
        };
        self.update_toast = Some(UpdateToast::offer(release));
        true
    }

    /// Kick off the staged build the popup offered. The existing update
    /// state machine takes it from here: the status bar's spinner while
    /// cargo runs, then the popup and the status bar both offer Relaunch.
    pub(super) fn start_staged_update(&mut self, version: String) {
        // Homebrew owns a Cellar binary (#375): staging and swapping it
        // would leave brew's records describing a file croft replaced.
        if self.install_source == crate::update_check::InstallSource::Homebrew {
            self.status = format!(
                "croft v{version} is available - this install is managed by Homebrew: run `{}`",
                crate::update_check::BREW_UPGRADE
            );
            return;
        }
        self.staged_status = UpdateStatus::Idle;
        // One progress indicator, not two (#1852): the status bar's spinner
        // carries the build from here, as it does every other update, so
        // the popup goes. It comes back by itself when the build ends: with
        // Relaunch when it lands, with the reason and Retry when it fails.
        self.update_toast = None;
        // Tests never download or build croft: they get a staging that has
        // only started.
        #[cfg(test)]
        {
            self.staged_install = Some(crate::update_check::StagedInstall::preloaded(
                &[],
                PathBuf::new(),
                &version,
            ));
        }
        #[cfg(not(test))]
        {
            self.staged_install = Some(crate::update_check::StagedInstall::start(
                croft_cache_dir(),
                version,
                staged_install_log_path(),
            ));
        }
    }

    /// Decline the offered version for good (until a newer one appears).
    pub(super) fn dismiss_update_offer(&mut self, version: &str) {
        if let Err(e) = crate::update_check::dismiss(&croft_cache_dir(), version) {
            self.status = format!("Could not remember the dismissed update: {e}");
        }
    }

    /// The staged binary waiting to be swapped in, once its build landed.
    pub(super) fn staged_update_binary(&self) -> Option<PathBuf> {
        let install = self.staged_install.as_ref()?;
        (self.staged_status == UpdateStatus::Ready).then(|| install.binary().to_path_buf())
    }

    /// Whether any producer has an update ready to re-exec into: the
    /// status-bar pill and the updater chord mean "any".
    pub(super) fn any_update_ready(&self) -> bool {
        self.update_status == UpdateStatus::Ready || self.staged_status == UpdateStatus::Ready
    }

    /// Whether any producer is mid-build (drives the spinner).
    pub(super) fn any_update_in_progress(&self) -> bool {
        self.update_status == UpdateStatus::InProgress
            || self.staged_status == UpdateStatus::InProgress
    }

    /// Arm the rebuild hint when the drift probe's verdict lands. The probe
    /// is dropped either way — it only ever answers once.
    pub(super) fn poll_drift_probe(&mut self) -> bool {
        let Some(probe) = self.drift_probe.as_ref() else {
            return false;
        };
        let Some(verdict) = probe.take() else {
            return false;
        };
        self.drift_probe = None;
        let Some(current) = verdict else {
            return false;
        };
        self.status = format!(
            "This croft was built at {}, but its repo is now at {current} - press {UPDATE_CROFT_CHORD} to rebuild & reinstall in the background",
            env!("CROFT_GIT_HASH"),
        );
        self.local_drift = Some(current);
        true
    }

    /// Kick off the background reinstall the drift hint offered. The
    /// existing update state machine takes it from here: spinner while
    /// `cargo install` runs, then the same "Ctrl+Shift+F9 to relaunch" flow
    /// a remote background update uses.
    pub(super) fn start_local_reinstall(&mut self) {
        // Tests take the offer too, and must never `cargo install` croft
        // over the one on PATH: they get an install that has only started.
        #[cfg(test)]
        {
            self.self_install = Some(crate::update_watch::SelfInstall::preloaded(&[]));
        }
        #[cfg(not(test))]
        {
            self.self_install = Some(crate::update_watch::SelfInstall::start(
                String::from(env!("CARGO_MANIFEST_DIR")),
                self_install_log_path(),
            ));
        }
    }

    /// True when the updater has something to do: either a landed update
    /// waiting to re-exec, or an armed drift hint waiting to rebuild.
    pub(super) fn update_armed(&self) -> bool {
        self.any_update_ready()
            || (self.local_drift.is_some()
                && self.update_status == UpdateStatus::Idle
                && self.staged_status != UpdateStatus::InProgress
                // `update_status` only leaves Idle when the InProgress event
                // is drained - once per outer loop, BEFORE the input drain -
                // so without this latch two presses in one crossterm batch
                // spawn two concurrent `cargo install`s racing cargo's locks
                // (#245). The staged release build holds the same cargo
                // lock, so it latches the updater the same way.
                && self.self_install.is_none()
                && self.staged_install.is_none())
    }

    /// croft's own updater (#865), on `Ctrl+Shift+F9` and a click on an
    /// update pill: re-exec into a landed update (`run()` swaps a staged
    /// binary in first), else start the rebuild an armed drift hint offers,
    /// else say why there is nothing to do. It had bare F9, which toggled
    /// no breakpoint while an update was on offer or building.
    pub(super) fn update_croft(&mut self) {
        if self.any_update_ready() {
            self.arm_reexec();
        } else if self.update_armed() {
            self.start_local_reinstall();
        } else if self.self_install.is_some()
            || self.staged_install.is_some()
            || self.any_update_in_progress()
        {
            self.status = String::from("croft is already updating in the background");
        } else if let Some(version) = self
            .update_toast
            .as_ref()
            .filter(|t| !t.ready)
            .map(|t| t.release.version.clone())
        {
            // The popup's Update, from the keyboard (#1844).
            self.start_staged_update(version);
        } else if self.manual_update_check.is_some() {
            self.status = String::from("Already checking for croft updates");
        } else if let Some(why) = manual_update_check_refusal(
            std::env::var_os("CROFT_REMOTE_AUTOUPDATE").is_some(),
            std::env::var_os("CROFT_NO_UPDATE_CHECK").is_some(),
        ) {
            self.status = String::from(why);
        } else {
            // Nothing on offer: ask now, and say what the answer was, a
            // missing network included (#1844).
            self.status = String::from("Checking for croft updates\u{2026}");
            self.manual_update_check = Some(crate::update_check::ManualCheck::start_with(
                croft_cache_dir(),
                String::from(env!("CARGO_PKG_VERSION")),
                self.update_fetch,
            ));
        }
    }

    /// The answer to a check Help: Update croft asked for (#1844).
    pub(super) fn poll_manual_update_check(&mut self) -> bool {
        let Some(verdict) = self.manual_update_check.as_ref().and_then(|c| c.take()) else {
            return false;
        };
        self.manual_update_check = None;
        self.status = match verdict {
            Ok(Some(release)) => {
                let status = format!("croft v{} is available", release.version);
                self.update_toast = Some(UpdateToast::offer(release));
                status
            }
            Ok(None) => format!("croft v{} is up to date", env!("CARGO_PKG_VERSION")),
            Err(reason) => format!("Could not check for croft updates: {reason}"),
        };
        true
    }

    pub fn poll_update_watch(&mut self) -> bool {
        let mut changed = self.poll_drift_probe();
        changed |= self.poll_source_build_marker();
        changed |= self.poll_update_check();
        changed |= self.poll_manual_update_check();
        // Three producers share one event vocabulary; each event keeps its
        // origin so a drift rebuild's failure is never charged to the
        // staged release build (or the reverse).
        let mut events: Vec<(UpdateSource, crate::update_watch::UpdateEvent)> = Vec::new();
        if let Some(watch) = self.update_watch.as_ref() {
            events.extend(watch.drain().into_iter().map(|e| (UpdateSource::Watch, e)));
        }
        if let Some(install) = self.self_install.as_ref() {
            events.extend(
                install
                    .drain()
                    .into_iter()
                    .map(|e| (UpdateSource::SelfInstall, e)),
            );
        }
        if let Some(staged) = self.staged_install.as_ref() {
            events.extend(
                staged
                    .drain()
                    .into_iter()
                    .map(|e| (UpdateSource::Staged, e)),
            );
        }
        for (source, ev) in events {
            changed = true;
            // Each producer writes ITS cell; the other cell is never
            // touched, so nothing one build does can strand the other's
            // landed binary (there is exactly one Ready per channel).
            let staged = source == UpdateSource::Staged;
            match ev {
                crate::update_watch::UpdateEvent::InProgress => {
                    if staged {
                        self.staged_status = UpdateStatus::InProgress;
                    } else {
                        self.update_status = UpdateStatus::InProgress;
                    }
                    // The dedicated red, spinning status-bar span renders the
                    // message now, so the generic status line stays clear.
                    self.update_spinner_start = Some(std::time::Instant::now());
                    self.status.clear();
                }
                crate::update_watch::UpdateEvent::Failed => {
                    if staged {
                        self.staged_status = UpdateStatus::Idle;
                    } else {
                        self.update_status = UpdateStatus::Idle;
                    }
                    if !self.any_update_in_progress() {
                        self.update_spinner_start = None;
                    }
                    self.status = match source {
                        UpdateSource::Staged => {
                            let staging = self.staged_install.take();
                            let reason = staging
                                .as_ref()
                                .and_then(|s| s.failure_reason())
                                .unwrap_or("the build failed");
                            let version = staging.map(|s| s.version).unwrap_or_default();
                            // The user just asked for this version, so the
                            // popup comes back saying why it failed, with
                            // Retry and Later one click away (#1844).
                            self.update_toast = Some(UpdateToast {
                                failed: Some(reason),
                                ..UpdateToast::offer(version.as_str().into())
                            });
                            format!(
                                "croft v{version} was not installed: {reason} - see {} (still on v{})",
                                staged_install_log_path().display(),
                                env!("CARGO_PKG_VERSION")
                            )
                        }
                        UpdateSource::SelfInstall => {
                            self.self_install = None;
                            format!(
                                "croft rebuild failed - see {} (still on the old binary)",
                                self_install_log_path().display()
                            )
                        }
                        UpdateSource::Watch => String::from(
                            "Background croft update failed; staying on current version",
                        ),
                    };
                }
                crate::update_watch::UpdateEvent::Ready => {
                    if staged {
                        self.staged_status = UpdateStatus::Ready;
                    } else {
                        self.update_status = UpdateStatus::Ready;
                    }
                    if !self.any_update_in_progress() {
                        self.update_spinner_start = None;
                    }
                    // Do NOT yank the user mid-work: surface a persistent
                    // "Update ready" prompt in the status bar and let them
                    // pick the moment. The re-exec only fires on the updater
                    // chord (#865), a click on that pill, or Relaunch on the
                    // popup (#333).
                    self.status = format!(
                        "Update ready - press {UPDATE_CROFT_CHORD} to relaunch croft (terminals will reset)"
                    );
                    if staged && let Some(install) = self.staged_install.as_ref() {
                        self.update_toast = Some(UpdateToast {
                            ready: true,
                            ..UpdateToast::offer(install.version.as_str().into())
                        });
                    }
                }
            }
        }
        changed
    }

    /// Current spinner glyph for an in-progress update (frame 0 when idle).
    pub(super) fn update_spinner_glyph(&self) -> &'static str {
        match self.update_spinner_start {
            Some(t) => update_spinner_frame(t.elapsed().as_millis()),
            None => UPDATE_SPINNER_FRAMES[0],
        }
    }

    /// Monotonic spinner phase the main loop watches to force a redraw each
    /// time the glyph should advance. Stays `0` whenever no update is running,
    /// so it never wakes the renderer outside an active update.
    pub(super) fn update_spinner_phase(&self) -> u128 {
        match self.update_spinner_start {
            Some(t) => t.elapsed().as_millis() / UPDATE_SPINNER_FRAME_MS,
            None => 0,
        }
    }

    /// Quit into the freshly installed binary, but only once the session,
    /// with every dirty buffer's unsaved text, is safely on disk: the
    /// relaunch used to go ahead when the save failed (a full disk, an
    /// unwritable cache) and those edits were gone.
    pub(super) fn arm_reexec(&mut self) {
        // Tests arm the relaunch too, and must not write into the real cache.
        let path = if cfg!(test) {
            std::env::temp_dir().join(format!(
                "croft-test-session-{}-{:?}.json",
                std::process::id(),
                std::thread::current().id()
            ))
        } else {
            crate::session_state::handoff_path()
        };
        if let Err(e) = self.capture_session_state().save(&path) {
            self.status = format!("Relaunch cancelled: could not save the session ({e:#})");
            return;
        }
        self.pending_reexec = true;
        self.quit = true;
    }

    /// Bind this croft's `croft view` socket and publish it to panes.
    ///
    /// Keyed by pid so two crofts on one box never contend, and published
    /// through [`crate::view_ipc::SOCK_PATH`], which `apply_pane_env` reads
    /// at spawn time so no spawn site has to remember it. Not the process
    /// env: `set_var` is unsound with threads running, and the body below
    /// says so at length. Under `cfg(test)` the bind is skipped, since a test
    /// that stood up an App would otherwise leave a live socket in the real
    /// cache dir and set the one-shot global for every OTHER test in the same
    /// process.
    pub(super) fn bind_view_socket() -> Option<std::os::unix::net::UnixListener> {
        if cfg!(test) {
            return None;
        }
        let dir = croft_cache_dir();
        // Latched, not swallowed. "A full or read-only `$HOME`" is one of the
        // causes VIEW_BIND_ERROR's own doc names, and it fails HERE, one line
        // above the `map_err` that would have reported it. Returning `None`
        // quietly left `status` on "Ready" and told the user to run from a
        // pane inside croft, which is what they were already doing.
        if let Err(e) = std::fs::create_dir_all(&dir) {
            *VIEW_BIND_ERROR.lock().unwrap() = Some(format!("{e}"));
            return None;
        }
        // ABSOLUTE, always. `croft_cache_dir` falls back to `.` when `HOME`
        // is unset, and the client resolves what it is handed against the
        // PANE's cwd - so a relative socket path published here reports "the
        // croft that opened this pane is gone" from any pane that has `cd`'d,
        // while the croft is running. The module's own invariant is that the
        // server only ever sees absolute paths; this is where that starts.
        let path = crate::view_ipc::socket_path(&dir, std::process::id());
        let path = std::path::absolute(&path).unwrap_or(path);
        sweep_dead_view_sockets(&dir);
        // A bind failure is reported, not swallowed. The reachable causes are
        // real - `AddrInUse` from the liveness check, a full or read-only
        // `$HOME`, or the 104-byte `AF_UNIX` path budget with a long home
        // directory on macOS - and the client's message for an unset socket
        // tells the user to run from a pane inside croft, which is exactly
        // what they were doing. Saying so once at startup is the difference
        // between a feature that is off and a feature that lies about why.
        let listener = prepare_view_listener(&path)
            .map_err(|e| {
                // Latched rather than dropped. The reachable causes are real -
                // `AddrInUse` from the liveness check, a full or read-only
                // `$HOME`, the 104-byte `AF_UNIX` path budget with a long home
                // on macOS - and the client's message for an unset socket
                // tells the user to run from a pane inside croft, which is
                // what they were already doing. `App::new` puts this on the
                // status line, so a feature that is off says why once instead
                // of misdirecting every time it is used.
                *VIEW_BIND_ERROR.lock().unwrap() = Some(format!("{e}"));
            })
            .ok()?;
        // Published through a `OnceLock` read at SPAWN time rather than
        // `std::env::set_var` (#362). Two things were wrong with the env:
        //
        // portable-pty snapshots `std::env::vars_os()` inside
        // `CommandBuilder::new` and then clears the child's environment before
        // applying only that snapshot, so a variable set after a pane was
        // constructed never reached its shell. This call happens while
        // `App::new` runs, which is AFTER the first pane exists, so `croft
        // view` was broken in the startup pane and working everywhere else.
        //
        // And `set_var` is unsafe in edition 2024 because a concurrent
        // `getenv` in another thread is a data race. By this point croft has
        // the highlight prewarm, the DAP reaper, a PTY reader, the git worker,
        // the MCP index refresh and the FS watcher all running, which is
        // exactly the condition `src/gui_path.rs` and `src/session.rs` both
        // document when they place their own `set_var` calls at startup.
        let _ = crate::view_ipc::SOCK_PATH.set(path);
        Some(listener)
    }

    /// Accept whatever `croft view` clients have queued and open their files.
    ///
    /// Returns whether anything changed, so the frame loop redraws. Every
    /// connection is answered before the next is accepted, which is right
    /// here: a request is one path and the work is opening a tab, so a
    /// thread per client would buy nothing and cost the editor's `&mut self`.
    pub fn drain_view_requests(&mut self) -> bool {
        // The drain is bounded by TIME rather than by a connection count.
        // A per-frame cap of N clients still bounds the frame at N times the
        // per-read timeout; a deadline bounds it at the deadline however many
        // clients stall. Whatever is left is accepted on the next frame,
        // which costs a stalled client latency and costs the user nothing.
        //
        // The per-stream timeout below is taken from what REMAINS of this
        // deadline, which is the half that actually bounds a stalled client.
        // `SO_RCVTIMEO` applies per `recv`, and `read_line` loops until it
        // sees a newline, so a flat 50ms is reset on every byte that arrives:
        // a client dribbling one byte every 40ms and never a newline holds
        // the frame loop for as long as it likes. Bounding the BYTES does not
        // fix that either - 50 bytes trickled over a second is under any cap
        // and still costs the second. Only a deadline the client cannot push
        // forward does.
        // A croft whose bind failed has no listener and never gets one, so it
        // must not pay two clock reads and a loop entry every frame forever.
        // The per-iteration binding below stays: it cannot be hoisted, since
        // holding that borrow across `answer_view_client`'s `&mut self` does
        // not compile.
        if self.view_listener.is_none() {
            return false;
        }
        let deadline = std::time::Instant::now() + self.view_drain_budget;
        let mut changed = false;
        loop {
            // Do not accept what we cannot serve. `read_line_by_deadline`
            // checks its budget BEFORE the first read, so a well-behaved
            // client accepted at deadline-minus-epsilon is refused without a
            // single read attempt, and the client does not retry. Deferring it
            // to the next frame costs that client one frame and costs the user
            // nothing, which is the same trade the deadline itself makes.
            if deadline.saturating_duration_since(std::time::Instant::now())
                < std::time::Duration::from_millis(3)
            {
                return changed;
            }
            let Some(listener) = self.view_listener.as_ref() else {
                return changed;
            };
            match listener.accept() {
                Ok((stream, _)) => {
                    // The reply is returned so a TEST can assert on it; the
                    // drain only needs to know whether a file was opened.
                    let (_, opened) = self.answer_view_client(stream, deadline);
                    changed |= opened;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return changed,
                Err(_) => return changed,
            }
        }
    }

    /// Open one requested path, answering with what the user should see.
    ///
    /// The path arrives absolute (the client resolved it against the pane's
    /// own cwd, which croft only approximates), so the checks here are about
    /// whether the file can be shown, not about where it is.
    pub(super) fn apply_view_request(&mut self, path: &Path) -> crate::view_ipc::ViewReply {
        use crate::view_ipc::ViewReply;
        // The invariant `view_ipc`'s header states, actually enforced. The
        // client resolves against ITS cwd before sending, so a relative path
        // here came from something that is not `croft view`; resolving it
        // against croft's own cwd would silently open a different file than
        // the sender named. Same-uid, so this is not a trust boundary, but
        // an invariant no code checks is a comment rather than an invariant.
        if !path.is_absolute() {
            return ViewReply::Err {
                message: format!(
                    "the request must carry an absolute path, got {}",
                    path.display()
                ),
            };
        }
        // A REGULAR file, not merely something that exists and is not a
        // directory. `File::open` on a FIFO blocks until a writer appears, so
        // `mkfifo /tmp/f && croft view /tmp/f` from any pane froze the whole
        // editor permanently; `/dev/zero` took the same route and read until
        // the machine ran out of memory. Neither is reachable through the
        // Explorer without a deliberate human gesture, and this is reachable
        // by any process in any pane.
        //
        // The 20ms drain deadline does not cover it: that bounds the READ of
        // the request, and this is the step after. Bounding one stage of a
        // pipeline says nothing about the next.
        //
        // `metadata` follows symlinks, so a symlink to a real file still
        // opens (and a symlink to a FIFO is refused on the target's kind);
        // one stat answers all three questions the old `exists` + `is_dir`
        // pair asked in two.
        //
        // This bounds the file's KIND, not the time. `metadata` is itself a
        // blocking syscall on a client-supplied path, so `croft view` naming
        // something on a hung NFS mount still parks the frame loop here,
        // before the type check runs. A real bound needs the stat off this
        // thread.
        //
        // The OPEN below is the larger half of the same problem, and it is
        // not bounded either: `open_pinned` reaches `Editor::open`, which for
        // a PDF shells out to `pdftoppm` with no timeout (#493), and walks
        // archives and documents on this thread for their own formats. A
        // build script running `croft view out.pdf` on a large document parks
        // the editor for the render. Filed rather than fixed here: moving the
        // open off the frame loop changes how every viewer reports failure,
        // which is a bigger change than the channel that exposed it.
        //
        // There is also a TOCTOU window between this and the open,
        // which is same-uid and so inside the trust boundary the module
        // already argues from - noted so a later reader does not mistake the
        // check for airtight.
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                // Not necessarily absence: EACCES and ELOOP land here too,
                // and the old text claimed "no such file" over an errno that
                // said otherwise.
                return ViewReply::Err {
                    message: format!("cannot read {}: {e}", path.display()),
                };
            }
        };
        if meta.is_dir() {
            return ViewReply::Err {
                message: format!("{} is a directory", path.display()),
            };
        }
        if !meta.is_file() {
            return ViewReply::Err {
                message: format!("{} is not a regular file", path.display()),
            };
        }
        // Staged pipe input is stamped with THIS croft's pid before it opens,
        // so the staging sweep can spare a file a live croft is showing
        // instead of deleting it on age alone. `croft view -` input cannot be
        // produced again by re-running anything, and the tab re-opens the
        // path (a SQLite page turn does it on every page), so a sweep that
        // wins that race breaks a working tab and loses the only copy.
        // Returns None for an ordinary file, which is then opened as named.
        let claimed = crate::view_ipc::claim_staged(path, std::process::id());
        let path = claimed.as_deref().unwrap_or(path);
        match self.editor.open_pinned(path) {
            Ok(()) => {
                // Same route as an Explorer click, focus included: the user
                // asked for this from a prompt and expects to be looking at
                // it, not to find it filed behind the pane they typed in.
                self.focus_pane(Pane::Editor);
                self.sync_open_file_poll_mtime();
                self.status = if crate::rebase_todo::is_todo(path) {
                    String::from(crate::rebase_todo::HINT)
                } else {
                    format!("Opened {}", path.display())
                };
                ViewReply::Ok
            }
            Err(e) => ViewReply::Err {
                message: format!("could not open {}: {e}", path.display()),
            },
        }
    }

    /// A click on one of the update popup's controls (#333). Update stages
    /// the build, whose progress the status bar shows (#1852); it, Skip
    /// and Relaunch take the popup down, Later folds it into its pill, and
    /// the link opens the release notes. A click elsewhere returns false.
    pub(super) fn handle_update_toast_click(&mut self, x: u16, y: u16) -> bool {
        let Some(toast) = self.update_toast.as_ref().filter(|t| t.open) else {
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
        let version = toast.release.version.clone();
        match action {
            UpdateToastAction::Update => {
                if self.staged_install.is_none() && self.self_install.is_none() {
                    self.start_staged_update(version);
                } else {
                    self.status = String::from("croft is already updating in the background");
                }
            }
            UpdateToastAction::Skip => {
                self.update_toast = None;
                self.dismiss_update_offer(&version);
            }
            UpdateToastAction::Later => {
                if let Some(t) = self.update_toast.as_mut() {
                    t.open = false;
                }
            }
            UpdateToastAction::CopyCommand => {
                let command = crate::update_check::BREW_UPGRADE;
                self.status = self
                    .copy_where_pasted(command)
                    .status(&format!("`{command}`"));
            }
            UpdateToastAction::ReleaseNotes => {
                self.open_url_for_user(&crate::update_check::release_notes_url(&version));
            }
            UpdateToastAction::InstallLog => {
                let log = staged_install_log_path();
                if let Err(e) = self.open_at(&log, 0, 0) {
                    self.status = format!("Could not open {}: {e:#}", log.display());
                }
            }
            UpdateToastAction::Relaunch => {
                self.update_toast = None;
                // The popup describes the staged release: only ITS
                // readiness may fire it, never a drift rebuild's.
                if self.staged_update_binary().is_some() {
                    self.arm_reexec();
                }
            }
        }
        true
    }

    /// Answer `croft edit --wait`'s poll (#620): whether `path` is still open
    /// in any tab of any group. Opens nothing.
    pub(super) fn probe_view_path(&self, path: &Path) -> crate::view_ipc::ViewReply {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let is_it = |p: &Path| p == path || p.canonicalize().is_ok_and(|c| c == canon);
        let open = self
            .editor
            .editors
            .iter()
            .chain(
                self.editor_layout
                    .inactive_groups()
                    .into_iter()
                    .flat_map(|g| g.editors.iter()),
            )
            .any(|t| t.path.as_deref().is_some_and(is_it));
        if open {
            crate::view_ipc::ViewReply::Err {
                message: String::from(crate::view_ipc::PROBE_OPEN),
            }
        } else {
            crate::view_ipc::ViewReply::Err {
                message: String::from(crate::view_ipc::PROBE_CLOSED),
            }
        }
    }

    /// In a `git-rebase-todo` tab (#620), a plain p/r/e/s/f/d on a commit
    /// line sets its action and moves to the next commit line. Off in vim mode,
    /// where those letters are vim commands. Returns whether it acted.
    pub(super) fn rebase_todo_key(&mut self, key: KeyEvent) -> bool {
        let KeyCode::Char(c) = key.code else {
            return false;
        };
        if !key.modifiers.is_empty()
            || self.vim.enabled
            || !self
                .editor
                .path
                .as_deref()
                .is_some_and(crate::rebase_todo::is_todo)
        {
            return false;
        }
        let Some(action) = crate::rebase_todo::action_for_key(c) else {
            return false;
        };
        let row = self.editor.cursor_row;
        let Some(line) = self.editor.lines.get(row).cloned() else {
            return false;
        };
        // A blank line takes no action and must not take the letter either:
        // git stops the rebase on a stray `s` (#1228).
        if line.trim().is_empty() {
            self.status = String::from(crate::rebase_todo::HINT);
            return true;
        }
        let Some(new_line) = crate::rebase_todo::set_action(&line, action) else {
            return false;
        };
        if matches!(action, "squash" | "fixup")
            && !crate::rebase_todo::has_kept_commit_above(&self.editor.lines, row)
        {
            self.status = format!("Can't {action} the first commit: no commit above it");
            return true;
        }
        self.editor
            .apply_span_edits(&[crate::widgets::editor::TextSpanEdit {
                start: (row, 0),
                end: (row, line.chars().count()),
                new_text: new_line,
                utf16: false,
            }]);
        // On to the next commit line, past comments and the trailing blank;
        // the last commit keeps the caret (#1228).
        if let Some(next) = self.editor.lines[row + 1..]
            .iter()
            .position(|l| crate::rebase_todo::is_commit_line(l))
        {
            self.editor.cursor_row = row + 1 + next;
        }
        self.editor.cursor_col = 0;
        self.status = String::from(crate::rebase_todo::HINT);
        true
    }

    /// Rebase: Abort (#620): empty the plan, save it and close its tab, which
    /// git reads as "abort the rebase".
    pub(super) fn abort_rebase_todo(&mut self) {
        if !self
            .editor
            .path
            .as_deref()
            .is_some_and(crate::rebase_todo::is_todo)
        {
            // Outside the plan tab, a rebase stopped on a conflict is the
            // one this means (#1356).
            if matches!(
                crate::git::repo_operation(&self.scm_root()),
                Some(crate::git::RepoOp::Rebase { .. })
            ) {
                self.abort_git_operation();
                return;
            }
            self.status = String::from("Rebase: Abort works in a git-rebase-todo tab");
            return;
        }
        let end = self.editor.lines.len().saturating_sub(1);
        let end_col = self
            .editor
            .lines
            .last()
            .map(|l| l.chars().count())
            .unwrap_or(0);
        self.editor
            .apply_span_edits(&[crate::widgets::editor::TextSpanEdit {
                start: (0, 0),
                end: (end, end_col),
                new_text: String::new(),
                utf16: false,
            }]);
        self.save();
        self.run_command(crate::widgets::command_palette::Command::CloseEditor);
        self.status = String::from("Rebase aborted");
    }
}

/// Kitty keyboard-protocol flags croft requests on startup.
///
/// `DISAMBIGUATE_ESCAPE_CODES` is the bit that makes terminals deliver
/// modifier keys (notably Cmd/Super on macOS in iTerm2 ≥3.5, Ghostty, kitty,
/// WezTerm) as real key events rather than swallowing them or routing them to
/// menus. We deliberately do NOT request `REPORT_ALL_KEYS_AS_ESCAPE_CODES`
/// because that re-encodes ordinary printable input and would break typing.
pub(super) fn keyboard_enhancement_flags() -> KeyboardEnhancementFlags {
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
}

/// The terminal modes croft turns on whenever it takes over a screen: alt
/// screen, mouse tracking, bracketed paste, steady-bar cursor. SINGLE SOURCE
/// OF TRUTH — the startup takeover, the post-scp TUI restore, and the
/// dtach-reattach re-assert all emit exactly this block, so the mode lists
/// can never drift apart. A mode enabled at startup but missing from the
/// reattach re-assert is the "mouse dead after reconnecting to a persisted
/// remote session" bug (0.1.611); add any new startup mode HERE, never as an
/// inline `execute!` at a call site. The kitty keyboard flags are the one
/// deliberate exception: startup pushes them (the push pairs with the
/// teardown pop), while [`mode_reassert_seq`] appends the SET form.
pub(crate) fn takeover_mode_seq() -> Vec<u8> {
    let mut seq = Vec::new();
    let _ = execute!(
        seq,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        // Focus in/out reports (#1622): auto save "onWindowChange" saves
        // when the croft window loses focus, which croft only learns from
        // the terminal.
        EnableFocusChange,
        crossterm::cursor::SetCursorStyle::SteadyBar,
    );
    seq
}

/// Escape bytes re-asserting every terminal mode croft set at startup. A
/// dtach reattach (`-r winch`) delivers only a SIGWINCH: the newly attached
/// terminal never received the startup DECSETs, so the alt screen, mouse
/// tracking, bracketed paste, and kitty keyboard flags are all off — the
/// "mouse dead after reconnecting to a persisted remote session" bug. The
/// main loop emits this on every Resize when `CROFT_SESSION_PERSISTENT` is
/// set (the dtach branch of the remote launch command); every sequence is
/// idempotent on a terminal that already has the mode enabled. The kitty
/// keyboard flags use the SET form (`CSI = flags ; 1 u`), not the push the
/// startup path uses: a push per WINCH would stack entries the single pop at
/// exit can't unwind, leaving the user's shell with enhanced keys on.
pub(super) fn mode_reassert_seq() -> Vec<u8> {
    use std::io::Write;
    let mut seq = takeover_mode_seq();
    let _ = write!(seq, "\x1b[={};1u", keyboard_enhancement_flags().bits());
    seq
}

/// Build the OSC 0 escape sequence that sets the terminal's window/icon title.
///
/// Format: `ESC ] 0 ; <title> BEL`.  Control bytes that would break the escape
/// (BEL, ESC, newlines) are stripped from `title` so untrusted input cannot
/// inject further sequences.
pub(super) fn set_title_seq(title: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(title.len() + 5);
    out.extend_from_slice(b"\x1b]0;");
    for byte in title.as_bytes() {
        match *byte {
            0x00..=0x1f | 0x7f => continue,
            b => out.push(b),
        }
    }
    out.push(0x07);
    out
}

/// iTerm2 proprietary OSC that overrides the *session* (not profile-on-disk)
/// bg colour. The `srgb:` prefix forces iTerm2 to interpret the hex bytes
/// as sRGB regardless of the user's "Use sRGB colour space" profile setting,
/// so combining this with an OSC-1337 PNG canvas filled with the same hex
/// guarantees both surfaces display the same physical pixel.
/// Format: `ESC ] 1 3 3 7 ; SetColors=bg=srgb:RRGGBB BEL`.
pub(super) fn set_session_bg_srgb_seq(rgb: (u8, u8, u8)) -> String {
    format!(
        "\x1b]1337;SetColors=bg=srgb:{:02x}{:02x}{:02x}\x07",
        rgb.0, rgb.1, rgb.2,
    )
}

/// Revert the iTerm2 session bg to the user's profile default. Emitted on
/// exit so the user's shell doesn't inherit croft's forced bg colour.
pub(crate) fn reset_session_bg_seq() -> String {
    String::from("\x1b]1337;SetColors=bg=default\x07")
}

/// Standard dynamic-color OSCs setting the HOST terminal's default
/// background (OSC 11) and foreground (OSC 10) to the theme's. iTerm2 gets
/// its proprietary sRGB variant on top, but this portable pair is what
/// recolors Ghostty, Kitty, WezTerm, xterm and friends, so a light theme
/// turns the whole session white (the issue's "including the terminal it
/// runs on") and `Color::Reset` cells inherit the theme on every host. The
/// host fg is the theme's default code foreground, the color plain text
/// renders in anyway.
pub(super) fn host_colors_seq(theme: crate::theme::Theme) -> String {
    let (br, bg, bb) = theme.editor_bg_rgb();
    let (fr, fg, fb) = theme.syntax().fg;
    format!("\x1b]11;rgb:{br:02x}/{bg:02x}/{bb:02x}\x07\x1b]10;rgb:{fr:02x}/{fg:02x}/{fb:02x}\x07")
}

/// The host terminal's default colours set to `theme`'s: the portable
/// dynamic colours on every host, then iTerm2's sRGB session background
/// where it has one. Startup, a theme switch and a reattach all send this.
pub(super) fn theme_host_colors_seq(theme: crate::theme::Theme) -> String {
    let mut seq = host_colors_seq(theme);
    if crate::iterm2_inline::detect_iterm2_inline_support() {
        seq.push_str(&set_session_bg_srgb_seq(theme.editor_bg_rgb()));
    }
    seq
}

/// What a persistent session's croft sends on every WINCH, which is how a
/// reattach reaches it: every startup mode ([`mode_reassert_seq`]), then the
/// theme's default colours. A terminal attaching for the first time never
/// had those colours, and a `croft remote` client resets them while its
/// connection is down (#1460), so they come back with the modes.
pub(crate) fn reattach_reassert_seq(theme: crate::theme::Theme) -> Vec<u8> {
    let mut seq = mode_reassert_seq();
    seq.extend_from_slice(theme_host_colors_seq(theme).as_bytes());
    seq
}

/// Reset the host terminal's default foreground and background to its own
/// configuration (OSC 110/111), undoing [`host_colors_seq`] on exit.
pub(crate) fn reset_host_colors_seq() -> &'static str {
    "\x1b]110\x07\x1b]111\x07"
}

pub(super) fn build_title(workspace: &std::path::Path) -> String {
    let name = workspace
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| workspace.display().to_string());
    format!("{APP_NAME}  {name}")
}

/// How hints name the updater chord: with the modifier each platform's
/// keyboard reaches for, Cmd on macOS and Ctrl elsewhere.
pub(super) const UPDATE_CROFT_CHORD: &str = if cfg!(target_os = "macos") {
    "Cmd+Shift+F9"
} else {
    "Ctrl+Shift+F9"
};

/// Bind the `croft view` socket and put it in the mode the frame loop needs.
///
/// Split out so the tests seat a listener through the SAME function rather
/// than building one themselves. A test that set `set_nonblocking` itself
/// would be asserting against its own setup: production could drop the call
/// and the "the drain does not block" test would still pass, which is a
/// check that cannot fail.
pub(super) fn prepare_view_listener(
    path: &Path,
) -> std::io::Result<std::os::unix::net::UnixListener> {
    let listener = crate::session::bind_socket_0600(path)?;
    // Polled from the frame loop, so a blocking accept would stall every
    // redraw until somebody happened to run `croft view`.
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Why the `croft view` socket could not be bound, if it could not.
///
/// A `static` because `bind_view_socket` runs before an `App` exists to hold
/// it: `App::new` reads it once, right after the call, and puts it on the
/// status line.
pub(super) static VIEW_BIND_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The status line a fresh `App` opens on, given whatever `bind_view_socket`
/// latched (#362).
///
/// Split out so it can be tested at all: `bind_view_socket` returns early
/// under `cfg(test)`, so nothing in the suite reaches the latch, and the arm
/// that reports a bind failure was the half that had never once run.
pub(super) fn view_status(view_bind_error: Option<&str>) -> String {
    match view_bind_error {
        // The one startup failure that would otherwise be invisible until a
        // user ran `croft view` and got told to do what they were doing.
        Some(e) => format!("croft view is unavailable: {e}"),
        None => String::from("Ready"),
    }
}

/// The status line when the user's `config.json` was skipped whole (#923),
/// given the warning that says why: every setting is then at its default,
/// which, said only in OUTPUT · Settings, looked like croft forgetting them.
pub(super) fn settings_not_loaded_status(error: &str) -> String {
    format!("Settings not loaded: {error}")
}

/// What the startup status says when the agent hook socket could not be
/// bound (#1368), so a hook that is never answered is not a mystery.
/// `None` when another croft already serves this workspace: that one
/// answers its agents, by design.
pub(super) fn hook_bind_status(e: &std::io::Error) -> Option<String> {
    (e.kind() != std::io::ErrorKind::AddrInUse)
        .then(|| format!("Agent edit approvals are unavailable: {e}"))
}

/// Remove `view-<pid>-<nonce>.sock` files whose croft is gone (#362).
///
/// A Unix socket is not auto-unlinked, so every croft that dies leaves its
/// file behind. An `impl Drop` would clean up the tidy exits and none of the
/// others (a SIGKILL, an OOM kill, a panic in another thread), and those are
/// exactly the cases that litter, so the sweep is the mechanism that has to
/// exist and Drop would only duplicate its easy half.
///
/// Liveness is judged from the PID IN THE NAME rather than by connecting.
/// A connect probe races a concurrent bind: the other croft can be between
/// `bind_socket_0600`'s stale-file removal and its `bind`, where the probe
/// reads "dead" correctly and the unlink then lands on the socket it creates
/// a moment later. `kill(pid, 0)` has no such window. The cost is that a
/// recycled PID keeps one dead file indefinitely, which is a stale byte
/// rather than a stolen socket.
pub(crate) fn sweep_dead_view_sockets(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // Listed once. `sweep_dead_bind_stages` runs per dead socket and used to
    // re-read the directory each time.
    let names: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.path())))
        .collect();
    let me = std::process::id();
    for (name, entry_path) in &names {
        // A croft that died between `bind_socket_0600`'s lock creation and its
        // rename leaves a `.bind.lock` with no socket beside it. The
        // socket-keyed pass below can never reach one, because
        // `pid_of_socket` requires the `.sock` suffix, so those leaked
        // permanently. Judging the lock by the pid in its own name closes it.
        let sock_name = match name.as_str().strip_suffix(".bind.lock") {
            Some(stem) => std::borrow::Cow::Owned(format!("{stem}.sock")),
            None => std::borrow::Cow::Borrowed(name),
        };
        let Some(pid) = crate::view_ipc::pid_of_socket(&sock_name) else {
            continue;
        };
        // Signal 0 checks existence and permission without delivering
        // anything; ESRCH is the only answer that means "gone".
        let gone = pid != me
            && unsafe { libc::kill(pid as libc::pid_t, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if gone {
            let _ = std::fs::remove_file(entry_path);
            // `bind_socket_0600` binds inside a `.s<pid>-<n>` dir and removes
            // it on both paths, but a croft killed inside that window strands
            // one, and nothing else ever looks at it again. Same argument as
            // the orphan lock below: it is litter only this sweep can reach.
            sweep_dead_bind_stages(&names, pid);
            // Derived from the SOCKET's name rather than from this entry's,
            // so sweeping a lock entry does not compose a second suffix onto
            // itself. Removing both from either entry is idempotent.
            let _ = std::fs::remove_file(dir.join(sock_name.as_ref()).with_extension("bind.lock"));
        }
    }
    sweep_staged_stdin(dir);
}

/// Remove `.s<pid>-<n>` bind staging directories belonging to a dead croft.
///
/// `crate::session::bind_socket_0600` creates one, binds inside it, renames
/// the socket out and removes the directory on both the success and failure
/// paths. A croft killed between the create and the remove strands it, and
/// the counter in that function restarts with the process, so the name is
/// reused rather than accumulating without bound - but the stranded directory
/// is never revisited by anything else.
///
/// Keyed on the same pid the socket sweep just judged dead, so a LIVE croft's
/// staging directory is never touched: removing one mid-bind would take the
/// socket it is about to rename out with it. "Never" modulo the pid-recycling
/// window the socket unlink above already accepts, which needs the pid space
/// to wrap between the `kill` and the removal.
///
/// Takes the directory listing rather than reading it: this is called once per
/// dead socket, and a cache dir holding N stale sockets was costing N full
/// listings at launch.
pub(super) fn sweep_dead_bind_stages(names: &[(String, PathBuf)], pid: u32) {
    let prefix = format!(".s{pid}-");
    for (name, path) in names {
        if name.starts_with(&prefix) && name[prefix.len()..].parse::<u64>().is_ok() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

/// How long a staged stdin file outlives the pipe that wrote it (#362).
///
/// Generous, because the cost of keeping a file too long is disk and the cost
/// of dropping one too early is a broken tab. It still bounds the pile: the
/// module's own rationale names `vault read … | croft view -`, and without any
/// sweep every such pipe left a permanent copy on disk the user was never told
/// about.
pub(super) const STAGED_STDIN_RETENTION: std::time::Duration =
    std::time::Duration::from_secs(60 * 60 * 24);

/// Drop staged stdin that no live croft holds and that has aged out (#362).
///
/// TWO signals, and the order matters: a live holder spares a file at any age,
/// and age only ever reaches files nobody holds.
///
/// Age alone was the whole mechanism, for a reason that still stands as far as
/// it goes. `stage_stdin` runs inside [`crate::view_ipc::run`] - the
/// short-lived `croft view` CLIENT process, which exits milliseconds later —
/// so the pid baked into every staged name belongs to something that is always
/// already dead. A liveness check therefore matched EVERY staged file,
/// including one a running croft had open in a tab, and launching a second
/// croft deleted it. That is not litter: `sqlite_view::table_page` re-opens the
/// file by path on every page turn, so the tab breaks on the next page.
///
/// But age alone still deleted a file a croft was showing, once that session
/// outlived the window: `sqlite_view::table_page` re-opens by path on every
/// page turn, so the tab breaks, and piped input cannot be regenerated by
/// re-running anything. So the SERVER stamps its pid into the name when it
/// opens the file ([`crate::view_ipc::claim_staged`]), which is the one point
/// where a live holder is known, and this reads it back.
///
/// Runs at launch, from `sweep_dead_view_sockets`, so a file staged inside a
/// long-lived session survives until the next croft starts, and now survives
/// that too for as long as its holder is running.
pub(super) fn sweep_staged_stdin(dir: &Path) {
    let staged = dir.join("view-stdin");
    let Ok(entries) = std::fs::read_dir(&staged) else {
        return;
    };
    for entry in entries.flatten() {
        // Only croft's own staging names, so a file dropped in this directory
        // by anything else is left alone rather than deleted on a guess.
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("stdin-") && !name.starts_with(crate::view_ipc::HELD_PREFIX) {
            continue;
        }
        // A file a LIVE croft is showing is never swept, whatever its age.
        // `apply_view_request` stamps its own pid in at open time, which is
        // the one moment a live holder is known; the name a client staged
        // carries a pid that is dead by construction, which is why age was
        // the only signal here before. Signal 0 checks existence and
        // permission without delivering anything, and ESRCH is the only
        // answer that means gone, exactly as the socket sweep judges it.
        if let Some(holder) = crate::view_ipc::holder_of_staged(name) {
            let alive = holder == std::process::id()
                || unsafe { libc::kill(holder as libc::pid_t, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
            if alive {
                continue;
            }
        }
        // Age still bounds the pile, but now it only ever reaches files no
        // live croft holds: a client that staged bytes nobody opened, and a
        // holder that has since exited.
        let too_old = entry
            .metadata()
            .and_then(|m| m.modified())
            .and_then(|t| t.elapsed().map_err(std::io::Error::other))
            .is_ok_and(|age| age > STAGED_STDIN_RETENTION);
        if too_old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The per-process self-install log: two drift-armed croft instances on one
/// machine must not overwrite each other's build diagnostics (#245).
pub(super) fn self_install_log_path() -> PathBuf {
    croft_cache_dir().join(format!("self-install-{}.log", std::process::id()))
}

/// The staged release build's log (#333): its own file, since a drift
/// rebuild may run in the same process and must not overwrite it.
pub(super) fn staged_install_log_path() -> PathBuf {
    croft_cache_dir().join(format!("staged-install-{}.log", std::process::id()))
}

/// Resolve the binary to re-exec into after an update. A local self-install
/// re-execs the exact path cargo reported writing (which honors an
/// `install.root` from cargo config that env resolution can't see), falling
/// back to cargo's env-level root (`CARGO_INSTALL_ROOT` / `CARGO_HOME` /
/// `~/.cargo`); the remote installer always rsyncs to the literal
/// `~/.cargo/bin/croft`. Either way the fresh file is preferred over
/// `current_exe`, which may resolve to the now-replaced inode.
pub(super) fn reexec_binary_path(
    local_install: Option<&crate::update_watch::SelfInstall>,
) -> PathBuf {
    let installed = if let Some(install) = local_install {
        install
            .installed_path()
            .or_else(crate::update_watch::cargo_install_bin)
    } else {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".cargo").join("bin").join("croft"))
    };
    if let Some(p) = installed
        && p.exists()
    {
        return p;
    }
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("croft"))
}

/// Re-send the terminal-restore escape sequences after a remote ssh
/// session ends. The remote croft enables alt-screen, mouse tracking,
/// and bracketed paste on the local tty; if it crashed or exited
/// abnormally those modes can leak back into the user's shell. Re-
/// emitting the disable sequences here ensures the host shell is
/// always returned to a clean state.
pub(super) fn restore_host_terminal_state() {
    use std::io::Write;
    let mut out = stdout();
    let _ = out.write_all(TERMINAL_RESTORE_SEQ);
    let _ = out.flush();
}

// Disable every mouse-tracking mode crossterm knows about (1000/1002/1003
// normal+button+any-motion, 1015/1006 urxvt+SGR encodings), bracketed
// paste (2004) and focus reports (1004), exit any leftover alt-screen
// (1049l), reset the cursor style, and reset the host's default fg/bg
// (OSC 110/111, undoing the theme's dynamic colors). Written as raw bytes because the callers that
// need it (the panic hook, the post-remote-ssh restore) no longer hold a
// crossterm Backend handle for `execute!`. Both callers are done with the
// tty (a return from remote re-enters `run`, which re-applies the theme's
// host colors), so the color reset can't strand a live session.
pub(super) const TERMINAL_RESTORE_SEQ: &[u8] =
    b"\x1b[?2026l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l\x1b[?2004l\x1b[?1004l\x1b[?1049l\x1b[ q\x1b]110\x07\x1b]111\x07";

pub(super) fn install_terminal_restore_panic_hook() {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            use std::io::Write;
            // Persist the panic BEFORE touching the terminal: the TUI teardown
            // can garble or scroll away the message printed to stderr, so a file
            // is the only reliable record (read with `cat ~/.cache/croft/panic.log`).
            log_panic_to_file(info);
            let _ = disable_raw_mode();
            let mut out = stdout();
            let _ = out.write_all(TERMINAL_RESTORE_SEQ);
            let _ = out.write_all(b"\x1b[?25h");
            let _ = out.flush();
            original(info);
        }));
    });
}

/// Append a panic record (version, thread, location, message, backtrace) to
/// `~/.cache/croft/panic.log`. Best-effort and silent on any I/O failure.
pub(super) fn log_panic_to_file(info: &std::panic::PanicHookInfo<'_>) {
    use std::io::Write;
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let dir = std::path::PathBuf::from(home).join(".cache").join("croft");
    let _ = std::fs::create_dir_all(&dir);
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("panic.log"))
    else {
        return;
    };
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| String::from("<non-string panic payload>"));
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| String::from("<unknown>"));
    let thread = std::thread::current()
        .name()
        .unwrap_or("<unnamed>")
        .to_string();
    let backtrace = std::backtrace::Backtrace::force_capture();
    let _ = writeln!(
        f,
        "=== croft {} panicked ===\nthread: {thread}\nlocation: {location}\nmessage: {message}\nbacktrace:\n{backtrace}\n",
        env!("CARGO_PKG_VERSION"),
    );
}
