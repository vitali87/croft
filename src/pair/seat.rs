//! Seating the pilot: connecting the collab seat, the claude and local-endpoint seats and
//! their threads, what a turn's end reports, and tearing a seat down.

use super::*;

/// Kill the claude child when the pilot unwinds, however it unwinds.
pub(super) struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// What the reader thread reports at each turn's `result` event.
pub(crate) struct TurnEnd {
    pub(crate) is_error: bool,
    /// The turn was cancelled by a participant (the interrupt surfaces as
    /// an error result; the driver names the real cause instead).
    pub(crate) cancelled: bool,
    pub(crate) text: String,
}

/// Connect the pilot's collab seat, retrying briefly (the dispatch just
/// ensured the relay, but its accept loop may still be coming up).
pub(super) fn connect_session(socket: &Path, name: &str) -> Result<CollabSession> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(ch) = CollabChannel::connect(socket, CollabRole::Guest) {
            return Ok(CollabSession::new(ch, name.to_string()));
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "collab relay never came up at {}",
            socket.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A seated pilot: the transport's threads over shared state. Both drivers
/// — the debug REPL ([`run_pilot`]) and the in-process
/// [`crate::pair_host::PairHost`] — run on top of this.
pub(crate) struct Pilot {
    pub(crate) state: Arc<Mutex<PairState>>,
    pub(crate) sink: TurnSink,
    pub(crate) turn_rx: Receiver<TurnEnd>,
    stop: Arc<AtomicBool>,
    transport: Transport,
}

/// The per-backend half of a seated pilot: what owns the conversation and
/// which threads to reap at shutdown.
pub(super) enum Transport {
    /// The claude CLI child: a persistent stream-json conversation over its
    /// stdio, read by a dedicated thread.
    Claude {
        guard: ChildGuard,
        reader: std::thread::JoinHandle<()>,
        pump: std::thread::JoinHandle<()>,
    },
    /// A local Anthropic-compatible endpoint: no child; a detached worker
    /// drains the queued turn bodies through one HTTP stream each and exits
    /// when the queue closes (shutdown never joins it — a mid-stream read
    /// can take minutes).
    Local { pump: std::thread::JoinHandle<()> },
}

impl Pilot {
    /// The conversation is gone (claude's stdout hit EOF). A local seat has
    /// no child to lose: endpoint failures surface per turn instead.
    pub(crate) fn reader_finished(&self) -> bool {
        match &self.transport {
            Transport::Claude { reader, .. } => reader.is_finished(),
            Transport::Local { .. } => false,
        }
    }

    /// Tear the seat down: leave no stream badge behind, hang up, then reap.
    /// Claude: the real CLI does not reliably exit on stdin EOF (MCP
    /// teardown lingers), and the reader joins only when claude's stdout
    /// closes — so the polite exit gets a short grace before the kill. The
    /// caller's exit must never hinge on a child's manners. Local: closing
    /// the turn queue ends the worker after its current stream; it is
    /// detached, not joined, because a mid-stream HTTP read can take minutes
    /// — the armed `cancelled` flag guarantees it applies nothing more.
    pub(crate) fn shutdown(self) {
        let Pilot {
            state,
            sink,
            turn_rx: _,
            stop,
            transport,
        } = self;
        {
            let mut st = state.lock().unwrap();
            revert_region(&mut st);
        }
        stop.store(true, Ordering::Relaxed);
        match transport {
            Transport::Claude {
                mut guard,
                reader,
                pump,
            } => {
                if let TurnSink::Claude(writer) = &sink {
                    writer.lock().unwrap().take(); // EOF asks claude to end
                }
                let grace = Instant::now() + Duration::from_secs(2);
                loop {
                    match guard.0.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) if Instant::now() < grace => {
                            std::thread::sleep(Duration::from_millis(50))
                        }
                        _ => {
                            let _ = guard.0.kill();
                            break;
                        }
                    }
                }
                let _ = pump.join();
                let _ = reader.join();
            }
            Transport::Local { pump } => {
                // Nothing from a still-draining stream may land after this.
                state.lock().unwrap().cancelled = true;
                drop(sink); // closes the turn queue; the worker exits after its stream
                let _ = pump.join();
            }
        }
    }
}

/// Connect the collab seat, spawn the claude child, and start the reader,
/// stderr, and pump threads. `events` seats the pilot inside croft (its
/// voice becomes [`crate::pair_host::PairEvent`]s); None keeps stdio.
pub(crate) fn seat_pilot(
    socket: &Path,
    name: &str,
    mut cmd: Command,
    events: Option<Sender<crate::pair_host::PairEvent>>,
) -> Result<Pilot> {
    let session = connect_session(socket, name)?;
    let state = Arc::new(Mutex::new(PairState::new(session, events.clone())));

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Own session, same reasoning as the MCP transport: a sidecar must never
    // touch croft's controlling tty; everything rides the piped stdio.
    //
    // SAFETY: `setsid` is async-signal-safe and the only call in the hook.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().context("spawning the claude CLI")?;
    let child_stdin = child.stdin.take().context("claude stdin missing")?;
    let child_stdout = child.stdout.take().context("claude stdout missing")?;
    let child_stderr = child.stderr.take().context("claude stderr missing")?;
    let guard = ChildGuard(child);
    let sink = TurnSink::Claude(Arc::new(Mutex::new(Some(child_stdin))));

    let stop = Arc::new(AtomicBool::new(false));
    let (turn_tx, turn_rx) = std::sync::mpsc::channel::<TurnEnd>();

    // Reader: claude's stdout events through the fence machine.
    let reader = {
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            let mut fence = FenceMachine::new();
            let mut decoder = crate::mcp::transport::LineDecoder::new();
            let mut stdout = child_stdout;
            let mut buf = [0u8; 8192];
            loop {
                let n = match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                decoder.feed(&buf[..n]);
                while let Some(msg) = decoder.next_message() {
                    handle_claude_event(&state, &mut fence, &msg, &turn_tx);
                }
            }
        })
    };

    // Stderr tee, same as the DAP transport: claude's diagnostics reach the
    // driver without touching the protocol stream.
    {
        let events = events.clone();
        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(child_stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    // Seated, claude's stderr chatter (MCP startup and the
                    // like) is a diagnostic for the OUTPUT channel — as a
                    // Commentary event it was anchored into the user's file
                    // as one of the navigator's own comment boxes.
                    Ok(_) => match &events {
                        Some(_) => {
                            crate::output::push(
                                "Navigator",
                                crate::output::OutputLevel::Info,
                                &format!("[claude] {}", line.trim_end()),
                            );
                        }
                        None => eprint!("[claude] {line}"),
                    },
                }
            }
        });
    }

    // Pump: remote spans shift the streamed region; StreamCancel cancels.
    let pump = {
        let state = Arc::clone(&state);
        let sink = sink.clone();
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let req_id = AtomicU64::new(0);
            while !stop.load(Ordering::Relaxed) {
                pump_session(&state, &sink, &req_id);
                std::thread::sleep(Duration::from_millis(25));
            }
        })
    };

    Ok(Pilot {
        state,
        sink,
        turn_rx,
        stop,
        transport: Transport::Claude {
            guard,
            reader,
            pump,
        },
    })
}

/// Seat the pilot on a local Anthropic-compatible endpoint: the same collab
/// guest seat and pump as the claude transport, but no child — a worker
/// owns the conversation (the endpoint is stateless) and runs one blocking
/// [`local::stream_turn`] per queued turn body.
pub(crate) fn seat_local(
    socket: &Path,
    name: &str,
    base_url: &str,
    model: &str,
    events: Option<Sender<crate::pair_host::PairEvent>>,
) -> Result<Pilot> {
    let session = connect_session(socket, name)?;
    let state = Arc::new(Mutex::new(PairState::new(session, events)));
    let stop = Arc::new(AtomicBool::new(false));
    let (turns_tx, turns_rx) = std::sync::mpsc::channel::<String>();
    let (end_tx, turn_rx) = std::sync::mpsc::channel::<TurnEnd>();
    let sink = TurnSink::Local(turns_tx);

    // Detached worker: owns the conversation, exits when the queue closes.
    {
        let state = Arc::clone(&state);
        let (base_url, model) = (base_url.to_string(), model.to_string());
        let system = system_prompt(&Provider::Local {
            base_url: base_url.clone(),
        })
        .into_owned();
        std::thread::spawn(move || {
            let mut messages: Vec<Value> = Vec::new();
            while let Ok(body) = turns_rx.recv() {
                messages.push(json!({ "role": "user", "content": body }));
                local::stream_turn(&base_url, &model, &system, &mut messages, &state, &end_tx);
            }
        });
    }

    // Pump: remote spans shift the streamed region; StreamCancel reverts
    // (no interrupt to deliver — the local stream just stops applying).
    let pump = {
        let state = Arc::clone(&state);
        let sink = sink.clone();
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let req_id = AtomicU64::new(0);
            while !stop.load(Ordering::Relaxed) {
                pump_session(&state, &sink, &req_id);
                std::thread::sleep(Duration::from_millis(25));
            }
        })
    };

    Ok(Pilot {
        state,
        sink,
        turn_rx,
        stop,
        transport: Transport::Local { pump },
    })
}
