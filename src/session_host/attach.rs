//! The attaching client: raw mode, pumping stdin to the socket and output to the
//! terminal, the version-mismatch banner, reconnecting across a host swap, and handing
//! the terminal back on exit.

use super::*;

/// Attach this terminal as a client: raw mode, pump stdin to the socket and
/// socket bytes to stdout, report resizes. Returns the session's exit code
/// (0 on detach). Deliberately does nothing else to the terminal: every
/// mode change (alt screen, mouse, Kitty flags) belongs to the inner croft
/// and passes through as bytes, exactly as under dtach.
/// How one attach ended: the session finished (or the client detached),
/// or the user chose to restart a version-skewed session (#53).
#[derive(Debug)]
pub enum PumpOutcome {
    Exit(i32),
    RestartRequested,
    /// The host announced a self-exec into an updated binary (#238): the
    /// connection is about to drop while the socket stays bound. Internal
    /// to the attach loop, which reconnects; callers never see it.
    HostSwapped,
    /// This client detached itself with the detach chord (#679); the
    /// session keeps running. [`attach_client`] restores the terminal and
    /// reports it as `Exit(0)`.
    Detached,
    /// The host closed the connection without an exit code: this client
    /// was disconnected (Session: Detach, Session: Participants) or the
    /// host died (#679). [`attach_client`] restores the terminal and
    /// reports it as `Exit(0)`.
    Disconnected,
}

/// How long the client waits for [`Control::ServerHello`] after its Hello.
/// A matching server answers in the same scheduling quantum (the reply is
/// written before the client is even registered for broadcasts); only a
/// pre-0.1.698 server stays silent, so expiry means "old server", not
/// "slow server" — and the attach still proceeds after the banner, so the
/// cost of a false positive is one keypress, never a refused attach.
pub(super) const SERVER_HELLO_DEADLINE: Duration = Duration::from_secs(2);

/// What the user chose on the version-mismatch banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MismatchAction {
    Continue,
    Restart,
    Detach,
}

pub(super) fn mismatch_action(key: u8) -> MismatchAction {
    match key {
        b'c' | b'C' => MismatchAction::Continue,
        b'r' | b'R' => MismatchAction::Restart,
        _ => MismatchAction::Detach,
    }
}

/// The plain-text banner for a version-skewed attach: visible, actionable,
/// never a silent blank screen (#53). Raw mode is active, hence the CRLFs.
pub(super) fn mismatch_banner(server: Option<&str>, client: &str) -> String {
    let server = match server {
        Some(v) => format!("croft {v}"),
        None => String::from("pre-0.1.698 (reports no version)"),
    };
    // Joined with explicit CRLFs: raw mode disables ONLCR, so a bare LF
    // staircases — and a multi-line string literal carries the source
    // file's LFs, which is exactly what rustfmt turns escapes into.
    [
        "",
        "croft session version mismatch",
        &format!("  session server: {server}"),
        &format!("  this croft:     croft {client}"),
        "The session may render incorrectly until the server is restarted.",
        "[C] continue anyway   [R] restart session (its terminals die)   [any other key] detach",
        "",
    ]
    .join("\r\n")
}

pub fn attach_client(socket: &Path) -> Result<PumpOutcome> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    crossterm::terminal::enable_raw_mode().context("enabling raw mode")?;
    let result = attach_client_loop(socket, &mut stream);
    let _ = crossterm::terminal::disable_raw_mode();
    match result {
        // The inner croft never ran its own teardown for THIS terminal: it
        // is still running, or died without one. Undo its modes here so the
        // shell prompt comes back usable instead of stuck in the alternate
        // screen with mouse reporting and enhanced keys on (#679).
        Ok(outcome @ (PumpOutcome::Detached | PumpOutcome::Disconnected)) => {
            let alive = crate::session::is_alive(socket);
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&client_teardown_seq());
            let _ = writeln!(out, "{}", attach_end_note(&outcome, alive));
            let _ = out.flush();
            Ok(PumpOutcome::Exit(0))
        }
        other => other,
    }
}

/// The escape bytes that hand a terminal back to the shell after the inner
/// croft owned it: the same modes croft's own exit undoes (keyboard flags,
/// cursor shape, alternate screen, mouse capture, bracketed paste, host
/// colours), plus a visible cursor and plain SGR. Every sequence is a no-op
/// on a terminal that never had the mode on.
pub(super) fn client_teardown_seq() -> Vec<u8> {
    let mut seq = b"\x1b[0m\x1b]110\x07\x1b]111\x07\x1b[=0;1u".to_vec();
    let _ = crossterm::execute!(
        seq,
        crossterm::event::PopKeyboardEnhancementFlags,
        crossterm::cursor::SetCursorStyle::DefaultUserShape,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
        crossterm::event::DisableFocusChange,
        crossterm::cursor::Show,
    );
    seq
}

/// The line a client prints once it has left a session (#679).
pub(super) fn attach_end_note(outcome: &PumpOutcome, session_alive: bool) -> &'static str {
    match (outcome, session_alive) {
        (PumpOutcome::Detached, _) => {
            "Detached from the croft session; it keeps running. Run the same command to reattach."
        }
        (_, true) => {
            "Disconnected from the croft session; it keeps running. Run the same command to reattach."
        }
        (_, false) => "The croft session ended.",
    }
}

/// Whether stdin has input within `wait` (false on timeout or error).
pub(super) fn stdin_readable_within(wait: Duration) -> bool {
    let mut pfd = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = wait.as_millis().clamp(0, i32::MAX as u128) as i32;
    (unsafe { libc::poll(&mut pfd, 1, ms) }) > 0
}

/// Pump the attach, reconnecting across host swaps (#238): a HostSwap frame
/// means the host is re-execing into the updated binary and the socket
/// stays bound, so the EOF that follows is a handover, not a session end.
/// The stdin/resize forwarders are spawned once (first pump) and write
/// through a shared handle that reconnection re-points at the new stream.
pub(super) fn attach_client_loop(socket: &Path, stream: &mut UnixStream) -> Result<PumpOutcome> {
    let tx = Arc::new(Mutex::new(stream.try_clone().context("cloning socket")?));
    let detaching = Arc::new(AtomicBool::new(false));
    let mut first_attach = true;
    loop {
        match attach_client_pump(stream, &tx, &detaching, first_attach)? {
            // A detach that raced the swap wins: the stdin forwarder has
            // already stopped, so a successor connection would only hang.
            PumpOutcome::HostSwapped if detaching.load(Ordering::SeqCst) => {
                return Ok(PumpOutcome::Detached);
            }
            PumpOutcome::HostSwapped => {
                let Some(fresh) = reconnect_after_swap(socket) else {
                    // The successor never came up: from here the session is
                    // as gone as a killed server, which is an Exit(0) detach.
                    return Ok(PumpOutcome::Exit(0));
                };
                *tx.lock().unwrap() = fresh.try_clone().context("cloning socket")?;
                *stream = fresh;
                first_attach = false;
            }
            outcome => return Ok(outcome),
        }
    }
}

/// The successor host inherits the bound socket fd, so a live handover
/// answers within milliseconds - the window covers exec + adopt, not a
/// rebind; connect attempts during the gap queue in the listener backlog.
pub(super) fn reconnect_after_swap(socket: &Path) -> Option<UnixStream> {
    for _ in 0..25 {
        std::thread::sleep(Duration::from_millis(200));
        if let Ok(stream) = UnixStream::connect(socket) {
            return Some(stream);
        }
    }
    None
}

/// Writes the session's screen bytes to this client's terminal, removing
/// underline colours on the way when the terminal cannot read them (#2661).
pub(super) struct ScreenWriter {
    strip: Option<(crate::color_depth::UnderlineColorFilter, Vec<u8>)>,
}

impl ScreenWriter {
    /// For the terminal this process writes to: what its environment says,
    /// including the answer a local croft carried over SSH.
    pub(super) fn for_this_terminal() -> Self {
        Self::new(crate::color_depth::underline_colors_supported(|name| {
            std::env::var(name).ok()
        }))
    }

    pub(super) fn new(underline_colors: bool) -> Self {
        Self {
            strip: (!underline_colors).then(Default::default),
        }
    }

    pub(super) fn write(&mut self, out: &mut impl Write, bytes: &[u8]) -> Result<()> {
        match &mut self.strip {
            None => out.write_all(bytes),
            Some((filter, scratch)) => {
                scratch.clear();
                filter.feed(bytes, scratch);
                out.write_all(scratch)
            }
        }
        .context("writing to terminal")
    }
}

pub(super) fn attach_client_pump(
    stream: &mut UnixStream,
    tx: &Arc<Mutex<UnixStream>>,
    detaching: &Arc<AtomicBool>,
    first_attach: bool,
) -> Result<PumpOutcome> {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    tx.lock()
        .unwrap()
        .write_all(&encode_control_frame(&Control::Hello {
            name: client_name(),
            cols,
            rows,
            version: String::from(env!("CARGO_PKG_VERSION")),
            client_id: client_identity(),
        }))
        .context("sending hello")?;

    // Version phase (#53): wait briefly for the server's ServerHello. PTY
    // bytes racing it are buffered and replayed after the decision so
    // nothing is dropped. This runs BEFORE the stdin thread spawns, so a
    // banner keypress is read here, not swallowed by the forwarder.
    let mut pending: Vec<Vec<u8>> = Vec::new();
    let mut reader = FrameReader::new();
    let mut server_version: Option<String> = None;
    {
        let mut buf = [0u8; 65536];
        let phase_deadline = Instant::now() + SERVER_HELLO_DEADLINE;
        'phase: while server_version.is_none() {
            let remaining = phase_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            // Best-effort for the same reason as the clear below: Darwin
            // EINVALs setsockopt once the peer closed. Buffered frames and
            // the EOF still arrive through read().
            let _ = stream.set_read_timeout(Some(remaining));
            match stream.read(&mut buf) {
                Ok(0) => return Ok(PumpOutcome::Exit(0)),
                Ok(n) => {
                    for frame in reader.push(&buf[..n]) {
                        match frame {
                            Frame::Control(Control::ServerHello { version }) => {
                                server_version = Some(version);
                            }
                            Frame::Control(Control::Exit { code }) => {
                                return Ok(PumpOutcome::Exit(code));
                            }
                            // A swap can race an attach: coalesced with the
                            // ServerHello in one read, the HostSwap would
                            // fall to the catch-all and the following EOF
                            // would read as session end.
                            Frame::Control(Control::HostSwap) => {
                                return Ok(PumpOutcome::HostSwapped);
                            }
                            Frame::Bytes(bytes) => pending.push(bytes),
                            _ => {}
                        }
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break 'phase;
                }
                Err(_) => return Ok(PumpOutcome::Exit(0)),
            }
        }
        // Best-effort: Darwin refuses setsockopt (EINVAL) once the peer
        // has closed - which is exactly the state a host swap (#238) leaves
        // this socket in when the HostSwap frame raced the version phase.
        // Any buffered frames still drain below; a timeout left armed is
        // handled by the read loop tolerating WouldBlock.
        let _ = stream.set_read_timeout(None);
    }
    let client_version = env!("CARGO_PKG_VERSION");
    // A reconnect across a host swap writes nothing even when the server
    // is now NEWER than this still-running attach client: the screen
    // belongs to the inner croft, and text printed over it survives in
    // every cell that croft's next frames leave blank (#626, #652). The
    // inner croft says it instead, from this client's version in the
    // roster (`App::poll_session_presence`). Only a first attach, before
    // the inner croft's attach repaint, may show the banner.
    if first_attach && server_version.as_deref() != Some(client_version) {
        let mut out = std::io::stdout().lock();
        out.write_all(mismatch_banner(server_version.as_deref(), client_version).as_bytes())
            .context("writing banner")?;
        out.flush().context("flushing banner")?;
        drop(out);
        let mut key = [0u8; 1];
        let n = std::io::stdin().lock().read(&mut key).unwrap_or(0);
        // A closed or non-tty stdin (piped attach, headless CI) cannot
        // answer: continue, the pre-banner behavior, rather than silently
        // exiting 0 as if the session ended.
        if n == 0 {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(b"stdin closed; continuing\r\n");
            let _ = out.flush();
        }
        match mismatch_action(if n == 1 { key[0] } else { b'c' }) {
            MismatchAction::Continue => {}
            MismatchAction::Restart => {
                let _ = tx
                    .lock()
                    .unwrap()
                    .write_all(&encode_control_frame(&Control::Detach));
                return Ok(PumpOutcome::RestartRequested);
            }
            MismatchAction::Detach => {
                let _ = tx
                    .lock()
                    .unwrap()
                    .write_all(&encode_control_frame(&Control::Detach));
                return Ok(PumpOutcome::Exit(0));
            }
        }
    }

    // stdin -> socket. The thread parks on a blocking read; it dies with the
    // process when the session ends (the CLI exits right after we return).
    // Spawned once: on a swap reconnect (#238) the shared handle is
    // re-pointed at the new stream, and a second forwarder would race the
    // first for stdin bytes.
    if first_attach {
        let tx = Arc::clone(tx);
        let detaching = Arc::clone(detaching);
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 16384];
            let mut chord = DetachChord::default();
            // A write error is not fatal: mid-swap the socket is dead only
            // for the beat the successor host takes to adopt, and the handle
            // is re-pointed on reconnect. The dropped bytes are what any
            // dead transport would lose.
            let forward = |bytes: &[u8]| {
                if !bytes.is_empty() {
                    let _ = tx.lock().unwrap().write_all(&encode_bytes_frame(bytes));
                }
            };
            loop {
                // A withheld Cmd+K goes through after a pause, so a user
                // who stops after the leader still gets its chord hint.
                if chord.holding() && !stdin_readable_within(DETACH_CHORD_HOLD) {
                    forward(&chord.flush());
                    continue;
                }
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx
                            .lock()
                            .unwrap()
                            .write_all(&encode_control_frame(&Control::Detach));
                        break;
                    }
                    Ok(n) => {
                        let (bytes, detach) = chord.feed(&buf[..n]);
                        forward(&bytes);
                        if detach {
                            detaching.store(true, Ordering::SeqCst);
                            let mut stream = tx.lock().unwrap();
                            let _ = stream.write_all(&encode_control_frame(&Control::Detach));
                            // Leave without waiting for the host to hang up:
                            // a wedged host never would. The pump's read sees
                            // the EOF and reports the detach.
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                            break;
                        }
                    }
                }
            }
        });
    }

    // Terminal size -> resize frames. A 200ms poll instead of a SIGWINCH
    // handler keeps the client free of signal plumbing; the delay is
    // imperceptible against the terminal's own resize animation.
    // ponytail: poll, swap for signal_hook if 200ms ever reads as lag.
    if first_attach {
        let tx = Arc::clone(tx);
        let mut last = (cols, rows);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(200));
                let Ok(size) = crossterm::terminal::size() else {
                    continue;
                };
                if size != last {
                    last = size;
                    let frame = encode_control_frame(&Control::Resize {
                        cols: size.0,
                        rows: size.1,
                    });
                    // Swap-tolerant like the stdin forwarder: the handle is
                    // re-pointed on reconnect, and the next size change
                    // resends through it.
                    let _ = tx.lock().unwrap().write_all(&frame);
                }
            }
        });
    }

    // socket -> stdout, until the host reports the inner croft's exit or the
    // connection drops (server killed; treated as a detach). The version
    // phase's reader carries any partial frame; its buffered PTY bytes
    // replay first so nothing raced away.
    let mut out = std::io::stdout().lock();
    // The inner croft draws underline colours for every client (#2661);
    // this client removes them when its own terminal cannot read them.
    let mut screen = ScreenWriter::for_this_terminal();
    for bytes in pending.drain(..) {
        screen.write(&mut out, &bytes)?;
    }
    out.flush().context("flushing terminal")?;
    // The connection closing without an Exit frame: this client detached
    // itself, was disconnected, or the host died (#679).
    let closed = || {
        if detaching.load(Ordering::SeqCst) {
            PumpOutcome::Detached
        } else {
            PumpOutcome::Disconnected
        }
    };
    let mut buf = [0u8; 65536];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) => return Ok(closed()),
            Ok(n) => n,
            // A read timeout can survive the version phase when clearing it
            // failed (see above): an expiry on a quiet session is "nothing
            // yet", never "session over".
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => return Ok(closed()),
        };
        for frame in reader.push(&buf[..n]) {
            match frame {
                Frame::Bytes(bytes) => {
                    screen.write(&mut out, &bytes)?;
                    out.flush().context("flushing terminal")?;
                }
                Frame::Control(Control::Exit { code }) => return Ok(PumpOutcome::Exit(code)),
                // The host is re-execing into an updated binary (#238): the
                // EOF about to follow is a handover, not the session ending.
                Frame::Control(Control::HostSwap) => return Ok(PumpOutcome::HostSwapped),
                // Alone on a roster with no control holder = the #234
                // lockout (observed live 2026-08-22: one attached
                // participant, control false, and no way to type). Claim
                // control — the same right a detach/reattach already
                // grants, without asking the user to know that trick.
                // Bounded by arrival: one claim per matching Presence
                // frame, and a refused claim produces no new Presence.
                Frame::Control(Control::Presence { participants })
                    if sole_participant_lacks_control(&participants) =>
                {
                    let _ = tx
                        .lock()
                        .unwrap()
                        .write_all(&encode_control_frame(&Control::Claim));
                }
                // Other roster changes surface inside the inner croft (which
                // polls the presence sidecar), not in this thin pump.
                _ => {}
            }
        }
    }
}
