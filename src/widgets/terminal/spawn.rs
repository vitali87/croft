//! Starting a pane: spawning the shell or a program, attaching to a hosted pane, the reader
//! thread that parses its output, and replaying a saved transcript.

use super::*;

impl EventListener for VoidListener {
    fn send_event(&self, event: AlacEvent) {
        match event {
            AlacEvent::Bell => {
                if let Some(bell) = self.bell.as_ref() {
                    bell.store(true, Ordering::Release);
                }
            }
            // A program copying through OSC 52: Claude Code, tmux, nvim.
            // Over SSH it is their ONLY way to copy (a remote box has no
            // clipboard tool), so dropping it broke copy on a remote while
            // the same program copied fine locally through pbcopy (#678).
            // Only the clipboard, never the primary selection: X's
            // select-to-copy buffer has no counterpart on the user's side,
            // and writing it into the clipboard would clobber real copies.
            AlacEvent::ClipboardStore(ClipboardType::Clipboard, text) => {
                if let Some(clip) = self.clipboard.as_ref() {
                    *clip.lock().unwrap() = Some(text);
                }
            }
            AlacEvent::PtyWrite(text) => {
                if let Some(tx) = self.pty_response_tx.as_ref() {
                    let _ = tx.send(text);
                }
            }
            AlacEvent::TextAreaSizeRequest(cb) => {
                let (Some(tx), Some(size)) = (self.pty_response_tx.as_ref(), self.size.as_ref())
                else {
                    return;
                };
                let (cols, rows) = *size.lock().unwrap();
                let ws = alacritty_terminal::event::WindowSize {
                    num_lines: rows,
                    num_cols: cols,
                    cell_width: 1,
                    cell_height: 1,
                };
                let _ = tx.send(cb(ws));
            }
            _ => {}
        }
    }
}

/// Where the reader thread stops the parser inside one read:
/// * at a shell mark, to sample the cursor where it landed;
/// * at a wipe, to move the running command's output start past it;
/// * right after a line a `capture` trigger caught (the index of its hit),
///   to record the row the line starts on (#1450).
pub(super) enum ReadStop {
    Mark(crate::shell_integration::OscEvent),
    Wipe(Wipe),
    Capture(usize),
}

/// A saved transcript rendered as bytes safe to feed a fresh parser.
///
/// Control characters are stripped rather than escaped: a transcript comes off
/// disk, and a grid should never have contained an escape in the first place,
/// so anything control-shaped here is either corruption or someone's idea of a
/// joke. TAB is the exception — it is legitimate output, and deleting it
/// silently collapses tab-aligned columns — so it becomes a space, which is
/// what the grid would have shown anyway.
pub(super) fn transcript_bytes(lines: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for l in lines {
        let cleaned: String = l
            .chars()
            .map(|c| if c == '\t' { ' ' } else { c })
            .filter(|c| !c.is_control())
            .collect();
        bytes.extend(cleaned.as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    bytes
}

impl PtyTerminal {
    pub fn new(cwd: &std::path::Path) -> Result<Self> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        Self::new_shell(&shell, cwd)
    }

    /// Spawn a shell with `transcript` already painted into the grid, for a
    /// pane restored from a saved session (#249).
    ///
    /// The preamble is painted before the reader thread starts, not after the
    /// constructor returns. Replaying afterwards races the shell: both write
    /// the same `Term` behind the same lock, and a shell that reaches its
    /// first prompt before the replay wins, so the restored output lands
    /// below or through the prompt instead of above it.
    pub fn new_with_transcript(cwd: &std::path::Path, transcript: &[String]) -> Result<Self> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut cmd = interactive_shell_command(&shell, &crate::paths::config_dir());
        cmd.cwd(cwd);
        Self::spawn_with_preamble(cmd, None, &transcript_bytes(transcript))
    }

    /// Spawn an interactive login session for a specific shell (a terminal
    /// profile), rather than `$SHELL`. The same login-flag handling as [`new`].
    pub fn new_shell(shell_path: &str, cwd: &std::path::Path) -> Result<Self> {
        let mut cmd = interactive_shell_command(shell_path, &crate::paths::config_dir());
        cmd.cwd(cwd);
        Self::spawn_with(cmd, None)
    }

    pub fn new_running(program: &str, args: &[String], cwd: &std::path::Path) -> Result<Self> {
        let mut cmd = CommandBuilder::new(program);
        for a in args {
            cmd.arg(a);
        }
        cmd.cwd(cwd);
        let label = if args.is_empty() {
            program.to_string()
        } else {
            format!("{program} {}", args.join(" "))
        };
        Self::spawn_with(cmd, Some(label))
    }

    pub(super) fn spawn_with(cmd: CommandBuilder, run_label: Option<String>) -> Result<Self> {
        Self::spawn_with_preamble(cmd, run_label, &[])
    }

    /// As [`Self::spawn_with`], but paints `preamble` into the grid before the
    /// reader thread starts, so restored output cannot interleave with the new
    /// shell's first prompt.
    pub(super) fn spawn_with_preamble(
        mut cmd: CommandBuilder,
        run_label: Option<String>,
        preamble: &[u8],
    ) -> Result<Self> {
        apply_pane_env(
            &mut cmd,
            crate::view_ipc::SOCK_PATH.get().map(|p| p.as_path()),
        );
        // Not under test: the running binary there is the test harness, and
        // its link would land in the developer's real cache dir.
        if !cfg!(test) {
            apply_pane_bin(&mut cmd, pane_bin_dir().as_deref());
        }
        // An interactive shell may live in a pane host (#694); a task or
        // run pane is croft's own.
        let hosted = run_label.is_none() && persistent_panes_enabled();
        Self::spawn_from(PaneSource::Spawn { cmd, hosted }, run_label, preamble)
    }

    /// Reattach to the pane host at `socket` (#694): a pane whose shell
    /// outlived the croft that opened it. The host redraws the screen.
    pub fn attach_hosted(socket: &std::path::Path) -> Result<Self> {
        Self::spawn_from(PaneSource::Attach(socket.to_path_buf()), None, &[])
    }

    pub(super) fn spawn_from(
        source: PaneSource,
        run_label: Option<String>,
        preamble: &[u8],
    ) -> Result<Self> {
        let cols = 80u16;
        let rows = 24u16;
        let PaneLink {
            mut writer,
            mut reader,
            poll_fd: pty_fd,
            shell_pid,
            backend,
        } = PaneLink::open(source, cols, rows)?;
        let (input_tx, input_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        // Ends once every sender is gone (the pane and its responder), or
        // when the child stops taking input.
        std::thread::spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                    break;
                }
            }
        });

        let term_size = TermSize::new(cols as usize, rows as usize);
        let cfg = Config {
            // The user's `terminal_scrollback` (config.json) when set,
            // else the built-in default. Read per spawn so a settings edit
            // applies to the next pane without a relaunch.
            scrolling_history: crate::prefs::terminal_scrollback_lines(
                crate::prefs::Prefs::load_or_default().terminal_scrollback,
                SCROLLBACK_LINES,
            ),
            ..Config::default()
        };
        let size_shared = Arc::new(std::sync::Mutex::new((cols, rows)));
        let (response_tx, response_rx) = std::sync::mpsc::channel::<String>();
        // The reader answers the OSC 7501 probe on the same channel as the
        // parser's replies, so the answer lands ahead of the reply to a
        // device-attributes query sent after it, which is how a program
        // tells "supported" from "not" without a timeout.
        let status_reply_tx = response_tx.clone();
        let bell = Arc::new(AtomicBool::new(false));
        let clipboard_store = Arc::new(std::sync::Mutex::new(None));
        let listener = VoidListener {
            pty_response_tx: Some(response_tx),
            size: Some(size_shared.clone()),
            bell: Some(bell.clone()),
            clipboard: Some(clipboard_store.clone()),
        };
        let mut term = Term::new(cfg, &term_size, listener);
        // Before the reader thread exists, so nothing the child writes can
        // land ahead of the restored output (#249).
        if !preamble.is_empty() {
            Processor::<StdSyncHandler>::new().advance(&mut term, preamble);
        }
        let term = Arc::new(FairMutex::new(term));
        let term_for_thread = term.clone();
        let input_for_responder = input_tx.clone();

        std::thread::spawn(move || {
            while let Ok(text) = response_rx.recv() {
                if input_for_responder.send(text.into_bytes()).is_err() {
                    break;
                }
            }
        });

        let pty_dirty = Arc::new(AtomicBool::new(true));
        let pty_dirty_for_thread = pty_dirty.clone();
        let pty_pending_bytes = Arc::new(AtomicUsize::new(0));
        let pty_pending_bytes_for_thread = pty_pending_bytes.clone();
        // Seeded with the spawn time, not zero: a pane that has not output
        // yet is quiet since it STARTED, so a just-launched agent reads as
        // working for its first seconds rather than as idle since 1970.
        let last_output_ms = Arc::new(std::sync::atomic::AtomicU64::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        ));
        let last_output_ms_for_thread = last_output_ms.clone();
        let (port_tx, port_rx) = std::sync::mpsc::channel::<crate::port_detect::PortHit>();

        if let Some(label) = run_label.as_deref() {
            let header = format!("\x1b[2m▶ {label}\x1b[22m\r\n");
            let mut p = Processor::<StdSyncHandler>::new();
            let mut t = term.lock();
            p.advance(&mut *t, header.as_bytes());
        }

        let script_mode = run_label.is_some();

        let marks = Arc::new(std::sync::Mutex::new(Vec::<StoredMark>::new()));
        let marks_for_thread = marks.clone();
        let images = Arc::new(std::sync::Mutex::new(Vec::<StoredImage>::new()));
        let images_for_thread = images.clone();
        let capture_rows = Arc::new(std::sync::Mutex::new(CaptureRows::default()));
        let capture_rows_for_thread = capture_rows.clone();
        // Shutdown pipe + master fd for the reader's poll gate: the reader
        // must be wakeable without depending on the pty ever reaching EOF.
        let (shutdown_r, shutdown_w) = std::io::pipe().context("shutdown pipe")?;
        // Taken BEFORE the rewind registration below, which has no `?` after
        // it: an early return past a registered buffer would leave every
        // other pane trimmed to a share nobody is using.
        // One budget shared by every pane (#694), re-read per spawn like the
        // scrollback so a settings edit applies without a relaunch. Setting
        // it re-splits the budget over the panes already open; registering
        // then trims them to the smaller share before this pane records.
        let rewind_budget = crate::rewind::budget();
        let rewind_mb = crate::prefs::Prefs::load_or_default().terminal_rewind_mb;
        // Off by default until it can be replayed (#1342), but the recorder's
        // own tests drive it through real panes, and every spawn sets the one
        // shared total: test panes all record under the same explicit figure
        // rather than one turning it off under another.
        #[cfg(test)]
        let rewind_mb = rewind_mb.or(Some(128));
        rewind_budget.set_total(crate::rewind::configured_budget_bytes(rewind_mb));
        let rewind = rewind_budget.register();
        let rewind_for_thread = rewind.clone();
        // MONOTONIC, not the wall clock. Every read the buffer offers —
        // `span_ms`, `replay_from`, the orphan-keyframe sweep — assumes the
        // frames are ordered by `at_ms`, and `SystemTime` can step backwards
        // across an NTP correction or a manual set. A frame recorded during
        // such a step lands before its predecessors and becomes unreachable
        // through the only read API there is, which is a silent loss rather
        // than a visible fault. `Instant` cannot go backwards.
        let rewind_epoch = std::time::Instant::now();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel::<FinishedCommand>();
        #[cfg(test)]
        let finished_tx_for_test = finished_tx.clone();
        let osc7_cwd = Arc::new(std::sync::Mutex::new(Option::<std::path::PathBuf>::None));
        let osc7_for_thread = osc7_cwd.clone();
        let notifications = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let notifications_for_thread = notifications.clone();
        let progress = Arc::new(std::sync::Mutex::new(None::<(u8, u8)>));
        let progress_for_thread = progress.clone();
        let program_status = Arc::new(std::sync::Mutex::new(
            crate::program_status::Records::default(),
        ));
        let program_status_for_thread = program_status.clone();
        let line_times = Arc::new(std::sync::Mutex::new(
            std::collections::BTreeMap::<i64, u64>::new(),
        ));
        let line_times_for_thread = line_times.clone();
        let clock = Arc::new(std::sync::Mutex::new(ScrollClock::new()));
        let clock_for_thread = clock.clone();
        let osc7_host = Arc::new(std::sync::Mutex::new(None::<String>));
        let osc7_host_for_thread = osc7_host.clone();
        // Trigger set shared with the reader thread; the inner Arc is swapped
        // by `set_triggers` on startup / config reload, picked up per chunk.
        let triggers = Arc::new(std::sync::Mutex::new(std::sync::Arc::new(
            crate::triggers::TriggerSet::default(),
        )));
        let triggers_for_thread = triggers.clone();
        let (trigger_tx, trigger_rx) = std::sync::mpsc::channel::<crate::triggers::TriggerHit>();
        let watch_set = Arc::new(std::sync::Mutex::new(std::sync::Arc::new(
            crate::problem_matchers::WatchSet::default(),
        )));
        let watch_set_for_thread = watch_set.clone();
        let pane_watch = Arc::new(std::sync::Mutex::new(
            None::<std::sync::Arc<crate::problem_matchers::CompiledMatcher>>,
        ));
        let pane_watch_for_thread = pane_watch.clone();
        let (watch_tx, watch_rx) = std::sync::mpsc::channel::<(
            Option<std::path::PathBuf>,
            Vec<crate::build_matchers::BuildDiag>,
        )>();

        let reader_thread = std::thread::spawn(move || {
            let mut processor = Processor::<StdSyncHandler>::new();
            let mut port_sniffer = crate::port_detect::PortSniffer::new();
            let mut osc_sniffer = crate::shell_integration::OscSniffer::default();
            let mut wipe_sniffer = WipeSniffer::default();
            let mut trigger_scanner = crate::triggers::TriggerScanner::new();
            let mut trigger_hits = Vec::new();
            let mut watch_engine = crate::problem_matchers::WatchEngine::default();
            let mut watch_lines: Vec<(usize, String)> = Vec::new();
            // Last-seen matcher sources, for detecting a swap mid-window
            // (Arc identity — the app only ever replaces them wholesale).
            let mut last_wset = watch_set_for_thread.lock().unwrap().clone();
            // The engine starts with no task matcher, whatever the slot
            // holds by the time this thread runs: a task assigned to a new
            // pane before its first read must still count as an assignment,
            // or its matcher is never armed (#1541).
            let mut last_pwatch: Option<Arc<crate::problem_matchers::CompiledMatcher>> = None;
            // Per-pane monotonic id for captured inline images; the overlay
            // layout key uses it to tell a new picture from a moved one.
            let mut image_seq = 0u64;
            // Content id of the cursor row after the previous chunk: the
            // rows this chunk touched run from there to the cursor's new
            // id, and each takes the chunk's arrival time (last touch wins,
            // so a row is stamped when its content actually landed, not
            // when the cursor first parked on it).
            let mut prev_stamp_id: i64 = 0;
            // Started by 133;C, finished by the next 133;D.
            let mut running: Option<RunningCommand> = None;
            let mut buf = [0u8; 65536];
            loop {
                // A dev-server URL that ended the last read is held back in
                // case the next read extends it; when the output goes quiet
                // instead, it is sent as it stands (#1449).
                let ready = if port_sniffer.has_pending() {
                    match poll_pty(pty_fd, &shutdown_r, PORT_URL_SETTLE_MS) {
                        PtyWait::Ready => true,
                        PtyWait::Closed => false,
                        PtyWait::Idle => {
                            port_sniffer.flush(&port_tx);
                            continue;
                        }
                    }
                } else {
                    match wait_pty_readable(pty_fd, &shutdown_r, watch_engine.deadline()) {
                        PtyWait::Ready => true,
                        PtyWait::Closed => false,
                        PtyWait::Idle => {
                            // A task matcher's grace ran out with no shell
                            // mark (#1541): its held lines are output after
                            // all.
                            let wset = watch_set_for_thread.lock().unwrap().clone();
                            let pwatch = pane_watch_for_thread.lock().unwrap().clone();
                            let batches = watch_engine.expire(
                                std::time::Instant::now(),
                                &wset,
                                pwatch.as_ref(),
                            );
                            for batch in batches {
                                let cwd = osc7_for_thread.lock().unwrap().clone();
                                let _ = watch_tx.send((cwd, batch));
                            }
                            continue;
                        }
                    }
                };
                if !ready {
                    break;
                }
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        port_sniffer.sniff(&buf[..n], &port_tx);
                        // Shell-integration marks: split the advance at each
                        // OSC 133 so the cursor can be sampled exactly where
                        // the mark landed (alacritty drops the sequences
                        // themselves as unknown OSC).
                        let osc_events = osc_sniffer.scan(&buf[..n]);
                        // Where the shell's prompt and command marks fall in
                        // the chunk, for the watch engine below (#1541): the
                        // lines before a task's `133;C` are the shell echoing
                        // its command line, not its output.
                        let watch_marks: Vec<(usize, bool)> = osc_events
                            .iter()
                            .filter_map(|(end, ev)| {
                                use crate::shell_integration::OscEvent as E;
                                match ev {
                                    E::CommandStart => Some((*end, true)),
                                    E::PromptStart | E::PromptEnd | E::CommandEnd(_) => {
                                        Some((*end, false))
                                    }
                                    _ => None,
                                }
                            })
                            .collect();
                        // Notify/bell triggers match completed output lines,
                        // never inside a full-screen app (the alt screen owns
                        // the bytes; iTerm2 skips those too). The scanner
                        // tracks the alt boundary POSITIONALLY from the
                        // stream — the chunk's final mode says nothing about
                        // where a line fell — so it must see EVERY chunk, or
                        // its alt tracking and string state machine desync
                        // and skipped-gap output splices into phantom lines.
                        // It runs BEFORE the parse, so the parser can stop
                        // right after each captured line and read the row it
                        // landed on (#1450).
                        let trig = triggers_for_thread.lock().unwrap().clone();
                        // Watch problem matchers (#252) ride the same
                        // scanner: when any are configured, the completed
                        // lines it already produces feed the per-pane watch
                        // engine (no second byte scan over the stream).
                        let wset = watch_set_for_thread.lock().unwrap().clone();
                        let pwatch = pane_watch_for_thread.lock().unwrap().clone();
                        let watching = !wset.matchers.is_empty() || pwatch.is_some();
                        if watching {
                            trigger_scanner.scan_collect(
                                &buf[..n],
                                &trig,
                                &mut trigger_hits,
                                Some(&mut watch_lines),
                            );
                        } else {
                            trigger_scanner.scan(&buf[..n], &trig, &mut trigger_hits);
                        }
                        let mut t = term_for_thread.lock();
                        let wipes =
                            wipe_sniffer.scan(&buf[..n], t.mode().contains(TermMode::ALT_SCREEN));
                        let screen_wiped = wipes.iter().any(|w| w.screen);
                        let hist_wiped = wipes.iter().any(|w| w.hist);
                        // The parser also stops at each wipe, so the running
                        // command's output start is moved right where a
                        // wipe erased its row, before the scroll clock
                        // loses count of what scrolls after it (#876).
                        let mut stops: Vec<(usize, ReadStop)> = osc_events
                            .into_iter()
                            .map(|(end, ev)| (end, ReadStop::Mark(ev)))
                            .chain(wipes.into_iter().map(|w| (w.end, ReadStop::Wipe(w))))
                            .chain(trigger_hits.iter().enumerate().filter_map(|(i, h)| {
                                (h.action == crate::triggers::TriggerAction::Capture)
                                    .then_some((h.end, ReadStop::Capture(i)))
                            }))
                            .collect();
                        stops.sort_by_key(|(end, _)| *end);
                        // Record the WHOLE read, before the OSC split below.
                        // The loop advances the parser per segment and leaves
                        // `done` at the LAST mark, so recording `buf[done..n]`
                        // afterwards kept only the tail — with shell
                        // integration on, marks arrive around every prompt and
                        // command, so most of the session was parsed to the
                        // screen and never recorded. That is the one thing
                        // this buffer exists to prevent, and the pty test
                        // covering it could not see the bug because its script
                        // emitted no marks (fixed: the OSC variant alongside
                        // it fails without this).
                        //
                        // Still recorded BEFORE the advance, so the buffer
                        // holds the bytes that PRODUCED the state a keyframe
                        // would capture. Takes only its own lock: the pane's
                        // order is term -> clock -> line_times and `t` is held
                        // here, so anything reaching back for `term` would
                        // deadlock — `push` returns a keyframe request rather
                        // than rendering one for exactly that reason.
                        // Poisons independently of `term` (std Mutex vs
                        // alacritty's FairMutex): a panic taken under this
                        // lock disables recording for the pane's life, and the
                        // `if let Ok` degrades to silence rather than
                        // propagating. Acceptable for a cache, but not the
                        // same failure mode as `term`'s.
                        if let Ok(mut rb) = rewind_for_thread.lock() {
                            let at = rewind_epoch.elapsed().as_millis() as u64;
                            // The keyframe request is dropped for now: this is
                            // the recording half, and nothing replays yet. The
                            // overlay that does will honour it here, taking the
                            // screen while it holds `t` rather than reaching
                            // back for the lock. Until then the buffer replays
                            // from the start, which is correct but unbounded.
                            let _wants_keyframe = rb.push(at, &buf[..n]);
                        }
                        let mut done = 0usize;
                        for (end, stop) in stops {
                            processor.advance(&mut *t, &buf[done..end]);
                            done = end;
                            let ev = match stop {
                                ReadStop::Mark(ev) => ev,
                                ReadStop::Wipe(w) => {
                                    // Only RIS wipes both, and a full reset
                                    // removes every OSC 7501 record.
                                    if w.screen && w.hist {
                                        program_status_for_thread.lock().unwrap().reset();
                                    }
                                    let now = clock_for_thread.lock().unwrap().tick(&mut t);
                                    if let Some(run) = running.as_mut() {
                                        run.follow_wipe(w, now);
                                    }
                                    capture_rows_for_thread.lock().unwrap().follow_wipe(w, now);
                                    continue;
                                }
                                ReadStop::Capture(i) => {
                                    let mut clock = clock_for_thread.lock().unwrap();
                                    let mut rows = capture_rows_for_thread.lock().unwrap();
                                    let anchor =
                                        rows.record(&mut t, &mut clock, &trigger_hits[i].line);
                                    trigger_hits[i].anchor = Some(anchor);
                                    continue;
                                }
                            };
                            use crate::shell_integration::OscEvent as E;
                            match ev {
                                E::Cwd(p, host) => {
                                    *osc7_for_thread.lock().unwrap() = Some(p);
                                    *osc7_host_for_thread.lock().unwrap() = host;
                                }
                                E::Notify(msg) => {
                                    notifications_for_thread.lock().unwrap().push(msg);
                                }
                                E::Progress(state, pct) => {
                                    // State 0 clears; the rest hold the latest
                                    // (state, percent) for the border gauge.
                                    *progress_for_thread.lock().unwrap() =
                                        (state != 0).then_some((state, pct));
                                }
                                E::ProgramStatus(report) => {
                                    if report == crate::program_status::Report::Query {
                                        let reply = crate::program_status::PROBE_REPLY;
                                        let _ = status_reply_tx.send(reply.to_string());
                                    }
                                    program_status_for_thread.lock().unwrap().apply(report);
                                }
                                E::InlineImage(data) => {
                                    // Anchor the picture at the cursor's grid
                                    // position, keyed on the scroll clock like
                                    // an annotation (lock order term → clock).
                                    let line_rec = t.grid().cursor.point.line.0;
                                    let clock_rec = clock_for_thread.lock().unwrap().tick(&mut t);
                                    let mut imgs = images_for_thread.lock().unwrap();
                                    if imgs.len() >= IMAGES_MAX {
                                        let drop_n = imgs.len() + 1 - IMAGES_MAX;
                                        imgs.drain(..drop_n);
                                    }
                                    image_seq += 1;
                                    imgs.push(StoredImage {
                                        seq: image_seq,
                                        data: std::sync::Arc::new(data),
                                        line_rec,
                                        clock_rec,
                                    });
                                }
                                kind @ (E::PromptStart
                                | E::PromptEnd
                                | E::CommandStart
                                | E::CommandEnd(_)) => {
                                    // The program that reported is gone once
                                    // the shell draws its prompt again.
                                    if kind == E::PromptStart {
                                        program_status_for_thread.lock().unwrap().shell_prompt();
                                    }
                                    let dur = match &kind {
                                        E::CommandEnd(exit) => {
                                            // A finished command's gauge is
                                            // stale even if the program never
                                            // sent the state-0 clear.
                                            *progress_for_thread.lock().unwrap() = None;
                                            running.take().map(|run| {
                                                let dur = run.started.elapsed();
                                                // The typed command text and the
                                                // shell's cwd travel with the
                                                // completion so the durable
                                                // history records context.
                                                let now =
                                                    clock_for_thread.lock().unwrap().tick(&mut t);
                                                let start =
                                                    run.out_line - (now - run.out_clock) as i32;
                                                let output = command_output_text(&t, start);
                                                let cwd = osc7_for_thread.lock().unwrap().clone();
                                                let host =
                                                    osc7_host_for_thread.lock().unwrap().clone();
                                                let _ = finished_tx.send(FinishedCommand {
                                                    exit: *exit,
                                                    dur,
                                                    cmd: run.typed,
                                                    cwd,
                                                    host,
                                                    output,
                                                });
                                                dur
                                            })
                                        }
                                        _ => None,
                                    };
                                    let line_rec = t.grid().cursor.point.line.0;
                                    let clock_rec = clock_for_thread.lock().unwrap().tick(&mut t);
                                    let col_rec = t.grid().cursor.point.column.0;
                                    let started = matches!(kind, E::CommandStart);
                                    let mut ms = marks_for_thread.lock().unwrap();
                                    if ms.len() >= MARKS_MAX {
                                        let drop_n = ms.len() + 1 - MARKS_MAX;
                                        ms.drain(..drop_n);
                                    }
                                    ms.push(StoredMark {
                                        id: next_mark_id(),
                                        kind,
                                        line_rec,
                                        clock_rec,
                                        col_rec,
                                        dur,
                                    });
                                    // Read the typed line now, while it is
                                    // still on screen: the command may clear
                                    // it before its 133;D (#876).
                                    if started {
                                        running = Some(RunningCommand {
                                            started: std::time::Instant::now(),
                                            typed: last_command_input_text(&t, &ms, clock_rec),
                                            out_line: line_rec,
                                            out_clock: clock_rec,
                                        });
                                    }
                                }
                            }
                        }
                        processor.advance(&mut *t, &buf[done..n]);
                        // A destructive clear erases the content the pane's
                        // captured images anchored to: ED 2 the live screen
                        // rows, ED 3 the scrollback ones, RIS both. The
                        // sniffer already excluded wipes that landed inside
                        // the alt screen (positional tracking), so no mode
                        // check here — the chunk's FINAL mode says nothing
                        // about where a wipe fell (`clear && vim`).
                        if screen_wiped || hist_wiped {
                            let now = clock_for_thread.lock().unwrap().tick(&mut t);
                            let erased = |line_rec: i32, clock_rec: i64| {
                                let line = line_rec - (now - clock_rec) as i32;
                                if line >= 0 { screen_wiped } else { hist_wiped }
                            };
                            images_for_thread
                                .lock()
                                .unwrap()
                                .retain(|m| !erased(m.line_rec, m.clock_rec));
                            // Marks anchored to erased content go the same
                            // way: a stale decoration dot's menu copies or
                            // re-runs whatever later lands on its row.
                            marks_for_thread
                                .lock()
                                .unwrap()
                                .retain(|m| !erased(m.line_rec, m.clock_rec));
                        }
                        // Stamp newly-arrived rows for the timestamps gutter:
                        // every row the cursor moved past in this chunk gets
                        // the chunk's arrival time (one read of output is
                        // sub-millisecond, so chunk granularity is honest).
                        {
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            let mut clock = clock_for_thread.lock().unwrap();
                            let mut lt = line_times_for_thread.lock().unwrap();
                            stamp_chunk(&mut t, &mut clock, &mut lt, &mut prev_stamp_id, now_ms);
                        }
                        // Matcher ownership changed since the last chunk
                        // (matchers.json reload, task (re)assignment):
                        // drop any half-open window, or its next `ends`
                        // would scan stale lines with the OLD matcher and
                        // publish a stale batch.
                        let pwatch_same = match (&pwatch, &last_pwatch) {
                            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                            (None, None) => true,
                            _ => false,
                        };
                        // A newly assigned task matcher waits for its
                        // command's output to start (#1541). A swapped
                        // global set only matters to a pane with no task
                        // matcher: the task's matcher owns its pane's
                        // windows, and dropping one here lost the task's
                        // first cycle whenever the app's first sync of the
                        // global set landed mid-build.
                        if !pwatch_same {
                            watch_engine.assign(pwatch.as_ref(), std::time::Instant::now());
                        } else if !Arc::ptr_eq(&wset, &last_wset) && pwatch.is_none() {
                            watch_engine.reset();
                        }
                        last_wset = wset.clone();
                        last_pwatch = pwatch.clone();
                        if watching {
                            // Fed up to each mark, then the mark, so the
                            // engine sees them in stream order.
                            let mut lines = watch_lines.drain(..).peekable();
                            let ends = watch_marks.iter().map(|&(end, c)| (end, Some(c)));
                            for (end, mark) in ends.chain(std::iter::once((n, None))) {
                                while let Some((_, line)) = lines.next_if(|(at, _)| *at <= end) {
                                    if let Some(batch) =
                                        watch_engine.feed(&line, &wset, pwatch.as_ref())
                                    {
                                        let cwd = osc7_for_thread.lock().unwrap().clone();
                                        let _ = watch_tx.send((cwd, batch));
                                    }
                                }
                                if let Some(command_start) = mark {
                                    watch_engine.mark(command_start, pwatch.as_ref());
                                }
                            }
                        }
                        for h in trigger_hits.drain(..) {
                            let _ = trigger_tx.send(h);
                        }
                        drop(t);
                        pty_pending_bytes_for_thread.fetch_add(n, Ordering::Relaxed);
                        last_output_ms_for_thread.store(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0),
                            Ordering::Relaxed,
                        );
                        pty_dirty_for_thread.store(true, Ordering::Release);
                    }
                    Err(_) => break,
                }
            }
            if script_mode {
                let footer = b"\r\n\x1b[2m[Process exited]\x1b[22m\r\n";
                let mut t = term_for_thread.lock();
                processor.advance(&mut *t, footer);
                drop(t);
                pty_dirty_for_thread.store(true, Ordering::Release);
            }
        });

        Ok(Self {
            term,
            pty_dirty,
            pty_pending_bytes,
            last_output_ms,
            agent: None,
            program_status,
            status_mark: 0,
            status_seated: 0,
            port_rx,
            backend,
            input_tx,
            reader_thread: Some(reader_thread),
            reader_shutdown: Some(shutdown_w),
            shell_pid,
            exit: None,
            reaped_at: None,
            uid: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
            input_seen: false,
            last_input_ms: 0,
            run_pane: script_mode,
            manual_name_seen: false,
            cols,
            rows,
            size_shared,
            focused: false,
            broadcast_excluded: false,
            collapsed: false,
            focus_gradient: false,
            theme: crate::theme::Theme::default(),
            last_area: Rect::default(),
            last_inner: Rect::default(),
            selection: None,
            copy_cursor: None,
            sel_scrolled: 0,
            clock,
            alt_sel: None,
            drag_selecting: false,
            manual_name: None,
            auto_label: String::new(),
            search_needle: None,
            search_opts: crate::widgets::search::SearchOpts::default(),
            current_match: None,
            bell,
            clipboard_store,
            marks,
            osc7_cwd,
            osc7_host,
            accent: None,
            accent_badge: None,
            notifications,
            progress,
            line_times,
            show_timestamps: false,
            reveal_redactions: false,
            redacted_on_screen: 0,
            annotations: Vec::new(),
            finished_rx,
            hints: None,
            triggers,
            trigger_rx,
            watch_set,
            pane_watch,
            watch_rx,
            palette: crate::theme::VSCODE_ANSI,
            images,
            capture_rows,
            rewind,
            #[cfg(test)]
            written_for_test: Arc::new(std::sync::Mutex::new(Vec::new())),
            #[cfg(test)]
            finished_tx_for_test,
        })
    }

    /// Replay a saved transcript into this pane's grid so a restored pane
    /// shows the output it had before a restart (#249), above the fresh
    /// prompt the new shell prints.
    ///
    /// Each line is written literally with CRLF and NO escape sequences:
    /// the transcript is plain text precisely so a replay cannot re-run a
    /// cursor move, a screen clear, or a title change that the original
    /// output contained. The shell is untouched — this only paints the
    /// grid, so the prompt sits below the restored text.
    pub fn replay_transcript(&self, lines: &[String]) {
        let bytes = transcript_bytes(lines);
        if bytes.is_empty() {
            return;
        }
        let mut p = Processor::<StdSyncHandler>::new();
        let mut term = self.term.lock();
        p.advance(&mut *term, &bytes);
    }
}

/// Whether new shell panes go in pane hosts (#694): the
/// `terminal_persistent_panes` setting, on Linux and macOS, where a hosted
/// pane can still read its foreground process group.
pub(super) fn persistent_panes_enabled() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos"))
        && crate::prefs::Prefs::load_or_default().terminal_persistent_panes
}
