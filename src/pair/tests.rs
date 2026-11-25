//! The pilot's own tests, moved out of pair.rs (#2062).

use super::*;
use crate::collab::{ResolvedSpan, relay_serve};
use crate::lsp::manager::is_on_path;

// Live smoke of the WHOLE local backend — the real seat_local worker,
// fence machine, collab apply, and owner convergence — against a real
// Anthropic-compatible endpoint. Gated so the normal suite/CI skips it.
//
//   CROFT_SPIKE_OLLAMA=1 \
//   CROFT_SPIKE_OLLAMA_URL=http://localhost:11434 \
//   CROFT_SPIKE_OLLAMA_MODEL=qwen3-coder:30b \
//   cargo test --release ollama_live -- --nocapture --ignored
#[test]
#[ignore = "needs a live Ollama; run explicitly with CROFT_SPIKE_OLLAMA=1"]
fn ollama_live_smoke_drives_seat_local_end_to_end() {
    if std::env::var("CROFT_SPIKE_OLLAMA").is_err() {
        eprintln!("skip: set CROFT_SPIKE_OLLAMA=1 to run the live smoke");
        return;
    }
    let url =
        std::env::var("CROFT_SPIKE_OLLAMA_URL").unwrap_or_else(|_| "http://localhost:11434".into());
    let model =
        std::env::var("CROFT_SPIKE_OLLAMA_MODEL").unwrap_or_else(|_| "qwen3-coder:30b".into());

    let harness = OwnerHarness::start("def greet(name):\n    return \"hi \" + name\n");
    let pilot = seat_local(&harness.socket, "pilot", &url, &model, None).unwrap();
    send_turn(
        &pilot.state,
        &pilot.sink,
        "@demo.txt Rewrite greet to return the f-string f\"Hello, {name}!\" \
             instead of concatenation, using an EDIT fence.",
    )
    .unwrap();
    let end = pilot
        .turn_rx
        .recv_timeout(Duration::from_secs(300))
        .expect("turn ends");
    assert!(!end.is_error, "turn failed: {}", end.text);
    harness.wait_until("the f-string edit to converge", |h| {
        h.doc().is_some_and(|d| d.contains("f\"Hello, {name}!\""))
    });
    let doc = harness.doc().unwrap();
    assert!(
        !doc.contains("+ name"),
        "the old concatenation must be gone:\n{doc}"
    );
    pilot.shutdown();
}

use std::sync::atomic::{AtomicBool, Ordering};

/// The Claude sink wraps a turn body in a stream-json user message and
/// writes it to the child's stdin, byte-identical to the pre-TurnSink
/// path (`cat` echoes its stdin back so the test can read what landed).
#[test]
fn turn_sink_claude_writes_stream_json_to_stdin() {
    let mut child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn cat");
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let sink = TurnSink::Claude(Arc::new(Mutex::new(Some(stdin))));
    sink.send_user("hello\nworld").unwrap();
    let TurnSink::Claude(w) = &sink else {
        unreachable!()
    };
    w.lock().unwrap().take(); // EOF so cat exits
    let mut line = String::new();
    std::io::BufReader::new(stdout)
        .read_line(&mut line)
        .unwrap();
    let _ = child.wait();
    let v: Value = serde_json::from_str(&line).expect("one stream-json line");
    assert_eq!(v["type"], "user");
    assert_eq!(v["message"]["role"], "user");
    assert_eq!(v["message"]["content"], "hello\nworld");
}

/// The local backend appends the whole-line-fence guidance to the shared
/// pair prompt: weaker models are steered to row ranges instead of
/// character columns (the spike's coordinate slip).
#[test]
fn local_provider_appends_line_range_guidance() {
    let prompt = system_prompt(&Provider::Local {
        base_url: String::from("http://localhost:11434"),
    });
    assert!(prompt.starts_with(PAIR_SYSTEM_PROMPT));
    assert!(
        prompt.contains("<start_row>-<end_row>"),
        "suffix must teach the two-integer whole-line header"
    );
}

/// The claude backend's prompt is byte-identical to what 0.1.635 shipped.
#[test]
fn claude_provider_prompt_is_unchanged() {
    assert_eq!(system_prompt(&Provider::Claude), PAIR_SYSTEM_PROMPT);
}

/// Bounded poll toward `expected` through a contention-lossy accessor,
/// returning the last value read. The host's UI-tick reads are
/// deliberately imprecise under momentary pump contention
/// (`notes_snapshot` serves its stale cache on a contended try_lock,
/// `is_busy` answers busy on a lost `lock_briefly` race), so a one-shot
/// read of a state-final value can flake on a starved runner. The
/// deadline only bounds a real regression.
fn settle<T: PartialEq>(expected: &T, mut read: impl FnMut() -> T) -> T {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let got = read();
        if &got == expected || Instant::now() >= deadline {
            return got;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A relay plus a pumping owner session that serves `demo.txt`,
/// collecting every owner-side event for the assertions.
struct OwnerHarness {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    owner: Arc<Mutex<CollabSession>>,
    events: Arc<Mutex<Vec<CollabEvent>>>,
    stop: Arc<AtomicBool>,
    pump: Option<std::thread::JoinHandle<()>>,
}

impl OwnerHarness {
    fn start(text: &'static str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("p.collab.sock");
        {
            let s = socket.clone();
            std::thread::spawn(move || {
                let _ = relay_serve(&s);
            });
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let owner = loop {
            if let Some(ch) = CollabChannel::connect(&socket, CollabRole::Owner) {
                break CollabSession::new(ch, "owner".into());
            }
            assert!(Instant::now() < deadline, "relay never came up");
            std::thread::sleep(Duration::from_millis(10));
        };
        let owner = Arc::new(Mutex::new(owner));
        let events: Arc<Mutex<Vec<CollabEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let pump = {
            let (owner, events, stop) = (owner.clone(), events.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    {
                        let mut o = owner.lock().unwrap();
                        let ev = o.poll(|_| Some(text.to_string()));
                        events.lock().unwrap().extend(ev);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };
        Self {
            _dir: dir,
            socket,
            owner,
            events,
            stop,
            pump: Some(pump),
        }
    }

    fn doc(&self) -> Option<String> {
        self.owner
            .lock()
            .unwrap()
            .doc_text("demo.txt")
            .map(str::to_string)
    }

    fn wait_until(&self, what: &str, mut cond: impl FnMut(&Self) -> bool) {
        // A liveness backstop, generous on purpose: the python3 helper
        // and relay round trips race the parallel suite's load (#47),
        // and a tight ceiling here measured the machine, not croft.
        let deadline = Instant::now() + Duration::from_secs(60);
        while !cond(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn stream_states(&self) -> Vec<(String, bool)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                CollabEvent::StreamState { name, active, .. } => Some((name.clone(), *active)),
                _ => None,
            })
            .collect()
    }

    fn remote_edit_count(&self) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, CollabEvent::RemoteEdit { .. }))
            .count()
    }

    fn carets(&self) -> Vec<(String, usize, usize)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                CollabEvent::Caret { name, row, col, .. } => Some((name.clone(), *row, *col)),
                _ => None,
            })
            .collect()
    }
}

impl Drop for OwnerHarness {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(p) = self.pump.take() {
            let _ = p.join();
        }
    }
}

/// A one-shot Anthropic-SSE stub: accepts one connection, drains the
/// request, and streams `deltas` as `content_block_delta`/`text_delta`
/// events followed by `message_stop`, closing to end the stream.
fn serve_sse_once(deltas: Vec<&'static str>) -> (String, std::thread::JoinHandle<()>) {
    let mut body = String::new();
    for d in deltas {
        let ev = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": d },
        });
        body.push_str(&format!("event: content_block_delta\ndata: {ev}\n\n"));
    }
    body.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
    serve_http_once(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
             connection: close\r\n\r\n{body}"
    ))
}

/// One-shot HTTP stub answering `resp` verbatim after draining the whole
/// request (headers + Content-Length body), per the SSE-stub trap.
fn serve_http_once(resp: String) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        // Drain the WHOLE request (headers + Content-Length body) before
        // responding: answering after the first read races the client's
        // body write — the close RSTs the socket and eats the response.
        let mut req = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = sock.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            req.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&req).to_ascii_lowercase();
            if let Some(head_end) = text.find("\r\n\r\n") {
                let content_length = text[..head_end]
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if req.len() >= head_end + 4 + content_length {
                    break;
                }
            }
        }
        let _ = sock.write_all(resp.as_bytes());
    });
    (base_url, server)
}

/// A guest PairState over the harness relay plus the same pump thread
/// the seated pilot runs (bootstraps only land when someone polls).
fn pumped_state(
    harness: &OwnerHarness,
) -> (
    Arc<Mutex<PairState>>,
    Arc<AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    let session = connect_session(&harness.socket, "pilot").unwrap();
    let state = Arc::new(Mutex::new(PairState::new(session, None)));
    let stop = Arc::new(AtomicBool::new(false));
    let pump = {
        let (state, stop) = (state.clone(), stop.clone());
        let sink = TurnSink::Claude(Arc::new(Mutex::new(None)));
        std::thread::spawn(move || {
            let req_id = AtomicU64::new(0);
            while !stop.load(Ordering::Relaxed) {
                pump_session(&state, &sink, &req_id);
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };
    (state, stop, pump)
}

/// The UI thread must NEVER wait on the pilot's state mutex: pilot
/// threads hold it across blocking relay writes (write_frame_blocking
/// spins on WouldBlock with no deadline), and the App draining the
/// relay is what completes those writes — so a blocking read from the
/// render/tick path closes a circular wait and hangs the editor. Every
/// UI-facing host method answers from a fallback when the mutex is
/// contended.
#[test]
fn ui_reads_answer_instantly_while_the_pilot_mutex_is_held() {
    let harness = OwnerHarness::start("hello world");
    let host = crate::pair_host::PairHost::spawn(PairConfig {
        socket: harness.socket.clone(),
        workspace: harness._dir.path().to_path_buf(),
        name: String::from("nav"),
        model: Some(String::from("m")),
        task: None,
        provider: Provider::Local {
            base_url: String::from("http://127.0.0.1:9"),
        },
    })
    .unwrap();
    let state = host.state_for_tests().expect("pilot state");
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = state.lock().unwrap();
        locked_tx.send(()).unwrap();
        let _ = release_rx.recv();
    });
    locked_rx.recv().unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let busy = host.is_busy();
        let _ = host.caret_sites();
        let _ = host.notes_snapshot("demo.txt");
        let seen = host.last_seen("demo.txt");
        let rejected = host.send_yield_turn("demo.txt", "x").is_err();
        done_tx.send((busy, seen, rejected)).unwrap();
        drop(host);
    });
    let (busy, seen, rejected) = done_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("UI reads must answer instantly while the pilot mutex is held");
    assert!(busy, "contended = the pilot is mid-work = busy");
    assert_eq!(seen, None, "no baseline is better than a blocked frame");
    assert!(rejected, "a send under contention reports mid-turn");
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    reader.join().unwrap();
}

/// is_busy() must stay true from the send until the App CONSUMES the
/// turn's end: the reader clears turn_active before it queues TurnEnd,
/// so a tick landing in that window saw an idle seat and started a new
/// turn that stole the finished turn's origin and counters.
#[test]
fn the_seat_stays_busy_until_turn_done_is_consumed() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let harness = OwnerHarness::start("hello world");
    let script = harness._dir.path().join("fake_claude.py");
    std::fs::write(&script, FAKE_CLAUDE).unwrap();
    let log = harness._dir.path().join("stdin.log");
    let mut cmd = Command::new("python3");
    cmd.arg(&script).arg(&log).arg("stream");
    let mut host =
        crate::pair_host::PairHost::spawn_cmd(&harness.socket, "nav", None, cmd).unwrap();
    host.send_yield_turn("demo.txt", "hello world").unwrap();
    let state = host.state_for_tests().expect("pilot state");
    // Wait until the PILOT finished the turn (its reader cleared the
    // latch); the App has not polled yet.
    let deadline = Instant::now() + Duration::from_secs(60);
    while state.lock().unwrap().turn_active() {
        assert!(Instant::now() < deadline, "the fake turn never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        host.is_busy(),
        "the turn's end has not been consumed yet: the seat is still busy"
    );
    // The gate must hold at the SEND paths too, not just is_busy():
    // direct callers would otherwise overwrite the unconsumed turn's
    // staging in this window.
    let refused = host
        .send_yield_turn("demo.txt", "hello world")
        .expect_err("a send in the finish window must be refused");
    assert!(
        refused.to_string().contains("mid-turn"),
        "the refusal names the reason: {refused}"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if host
            .poll()
            .iter()
            .any(|e| matches!(e, crate::pair_host::PairEvent::TurnDone { .. }))
        {
            break;
        }
        assert!(Instant::now() < deadline, "TurnDone never arrived");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !settle(&false, || host.is_busy()),
        "consumed: the seat is idle again"
    );
    drop(host);
}

/// A yield whose send failed must not keep the look: last_seen is the
/// diff baseline (a retry would diff the file against itself and show
/// the model nothing), and the latched comment_only would silently
/// edit-suppress the next successful turn.
#[test]
fn a_failed_yield_leaves_no_look_behind() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let harness = OwnerHarness::start("hello world");
    // A child whose stdin is dead by the time Died is observable: it
    // closes stdin BEFORE stdout, so stdout's EOF (what fires Died)
    // proves the write end already has no reader. A child that merely
    // exits (`-c pass`) races process teardown: on a starved runner the
    // reader sees stdout close while the process, and with it stdin's
    // read end, is still winding down, and the send lands in the pipe
    // buffer instead of failing. The sleep keeps the pid alive for the
    // host's grace-kill teardown, like the real CLI's linger.
    let mut cmd = Command::new("python3");
    cmd.arg("-c")
        .arg("import os,time; os.close(0); os.close(1); time.sleep(60)");
    let mut host =
        crate::pair_host::PairHost::spawn_cmd(&harness.socket, "nav", None, cmd).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if host
            .poll()
            .iter()
            .any(|e| matches!(e, crate::pair_host::PairEvent::Died(_)))
        {
            break;
        }
        assert!(Instant::now() < deadline, "the child never died");
        std::thread::sleep(Duration::from_millis(10));
    }
    let err = host.send_yield_turn("demo.txt", "hello world").unwrap_err();
    // Pin the failure to the sink write AFTER staging: a pre-staging
    // rejection ("mid-turn", "pilot is gone") would pass the rollback
    // assertions below vacuously, without ever staging a look.
    assert!(
        format!("{err:#}").contains("claude"),
        "the send must fail at the claude sink, not before staging: {err:#}"
    );
    assert_eq!(
        host.last_seen("demo.txt"),
        None,
        "a failed yield keeps no baseline"
    );
    assert!(
        !settle(&false, || host.is_busy()),
        "a failed yield keeps no busy latch"
    );
    drop(host);
}

/// A send that never reached the endpoint must not wedge the seat: the
/// busy latch and the pending cancel note were committed BEFORE the
/// write, so a dead stdin left is_busy() true for the rest of the
/// seat's life and silently ate the "your edit was cancelled" preamble
/// owed to the model.
#[test]
fn a_failed_send_unlatches_the_turn_and_keeps_the_cancel_note() {
    let harness = OwnerHarness::start("hello world");
    let (state, stop, pump) = pumped_state(&harness);
    state.lock().unwrap().pending_note = Some(String::from("cancelled: f.rs"));
    let dead = TurnSink::Claude(Arc::new(Mutex::new(None)));
    let err = write_user_turn(&state, &dead, String::from("hi"))
        .expect_err("a dead stdin must fail the send");
    assert!(err.to_string().contains("stdin"), "{err}");
    {
        let st = state.lock().unwrap();
        assert!(
            !st.turn_active,
            "a turn that never left must not stay active"
        );
        assert_eq!(
            st.pending_note.as_deref(),
            Some("cancelled: f.rs"),
            "the cancel note is still owed to the model"
        );
    }
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();
}

/// An unseat moments before exit must still reap the pilot's child:
/// Drop detaches its grace-kill onto a thread, and a thread that dies
/// with the process leaves the child running (orphaned, still holding
/// its MCP subprocesses). join_teardowns() is the exit path's barrier
/// for every detached teardown.
#[test]
fn join_teardowns_reaps_a_dropped_hosts_child() {
    if !crate::lsp::manager::is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let harness = OwnerHarness::start("hello world");
    let mut cmd = std::process::Command::new("python3");
    cmd.arg("-c").arg("import time; time.sleep(120)");
    let host = crate::pair_host::PairHost::spawn_cmd(&harness.socket, "nav", None, cmd).unwrap();
    // Checked by process id, not by `pgrep -f` on a marker in the command
    // line: for a moment after spawn returns, the kernel is still putting
    // the new program's arguments in place, and a busy machine can stretch
    // that moment past pgrep's start-up, so pgrep missed a running child
    // (#2725). `kill(pid, 0)` answers from the moment the process exists
    // until it is reaped.
    let pid = host
        .child_id_for_tests()
        .expect("a claude seat has a child") as libc::pid_t;
    // SAFETY: signal 0 only checks that `pid` exists; nothing is sent.
    let exists = |pid| unsafe { libc::kill(pid, 0) } == 0;
    assert!(exists(pid), "the sleeper child must be running");
    drop(host);
    crate::pair_host::join_teardowns();
    assert!(
        !exists(pid),
        "after join_teardowns the dropped host's child must be reaped"
    );
}

/// A valid fence header arriving as the turn's LAST bytes (no trailing
/// newline) is protocol residue, not prose: flushing it as commentary
/// rendered the raw <<<NOTE ...>>> marker inside a comment box. A
/// malformed <<<-prefixed tail keeps the existing commentary behavior
/// (the malformed-header rule).
#[test]
fn an_unterminated_fence_header_never_leaks_into_commentary() {
    let mut m = FenceMachine::new();
    let mut events = m.push("thinking\n<<<NOTE src/f.rs:12>>>");
    events.extend(m.finish());
    let commentary = commentary_of(&events);
    assert!(commentary.contains("thinking"));
    assert!(
        !commentary.contains("<<<NOTE"),
        "protocol residue leaked: {commentary:?}"
    );
    let mut events = m.push("<<<EDIT src/f.rs:0:0-0:5>>>");
    events.extend(m.finish());
    assert!(
        !commentary_of(&events).contains("<<<EDIT"),
        "an EDIT header tail is the same class"
    );
    let mut events = m.push("<<<EDIT garbage");
    events.extend(m.finish());
    assert!(
        commentary_of(&events).contains("<<<EDIT garbage"),
        "a malformed tail stays commentary"
    );
}

/// The persistent AI caret: a park before the file is live resolves as
/// soon as the bootstrap lands (the pump broadcasts it), and a park on a
/// live file broadcasts immediately, clamped to the last line.
#[test]
fn park_caret_waits_for_bootstrap_then_broadcasts() {
    let harness = OwnerHarness::start("l0\nl1\nl2");
    let (state, stop, pump) = pumped_state(&harness);
    {
        let mut st = state.lock().unwrap();
        st.begin_turn("demo.txt", "l0\nl1\nl2", false); // requests the file
        st.park_caret("demo.txt", 1); // not live yet: pending
    }
    harness.wait_until("the parked caret to broadcast", |h| {
        h.carets()
            .iter()
            .any(|(n, r, c)| n == "pilot" && *r == 1 && *c == 0)
    });
    // Live now: an out-of-range park clamps to the last line.
    state.lock().unwrap().park_caret("demo.txt", 99);
    harness.wait_until("the clamped caret", |h| {
        h.carets().iter().any(|(n, r, _)| n == "pilot" && *r == 2)
    });
    stop.store(true, Ordering::Relaxed);
    let _ = pump.join();
}

/// Every landed note parks the pilot's caret at its anchor row: the AI
/// is visibly "looking" where it just commented.
#[test]
fn a_landed_note_parks_the_pilot_caret_at_its_row() {
    let harness = OwnerHarness::start("l0\nl1\nl2");
    let (state, stop, pump) = pumped_state(&harness);
    apply_fence_event(
        &state,
        FenceEvent::NoteStart {
            file: "demo.txt".into(),
            row: 2,
        },
    );
    apply_fence_event(&state, FenceEvent::NoteBody("look here".into()));
    apply_fence_event(&state, FenceEvent::NoteEnd);
    harness.wait_until("the note's caret", |h| {
        h.carets().iter().any(|(n, r, _)| n == "pilot" && *r == 2)
    });
    stop.store(true, Ordering::Relaxed);
    let _ = pump.join();
}

/// Ask and reply turns park the navigator's caret at their focus row
/// (the invoked range start / the answered note's row), so the caret is
/// visible from the first interaction even on comment-only turns.
#[test]
fn ask_and_reply_turns_park_the_navigator_caret() {
    let harness = OwnerHarness::start("l0\nl1\nl2");
    let mut cmd = Command::new("cat");
    cmd.stdin(Stdio::piped());
    let asker = crate::pair_host::PairHost::spawn_cmd(&harness.socket, "asker", None, cmd).unwrap();
    asker
        .send_ask_turn("demo.txt", (1, 1), "", "look at this", "l0\nl1\nl2")
        .unwrap();
    harness.wait_until("the ask turn's caret", |h| {
        h.carets().iter().any(|(n, r, _)| n == "asker" && *r == 1)
    });
    // The caret reaching the owner and the seat recording its site id
    // are two arrivals, not one: the assertion must wait for the state
    // it checks, not for a neighbour that usually lands first (#338).
    harness.wait_until("the seat's per-file site ids (its wire identity)", |_| {
        !asker.caret_sites().is_empty()
    });

    let mut cmd = Command::new("cat");
    cmd.stdin(Stdio::piped());
    let replier =
        crate::pair_host::PairHost::spawn_cmd(&harness.socket, "replier", None, cmd).unwrap();
    replier
        .send_reply_turn("demo.txt", 2, "seed note", "why here?", "l0\nl1\nl2")
        .unwrap();
    harness.wait_until("the reply turn's caret", |h| {
        h.carets().iter().any(|(n, r, _)| n == "replier" && *r == 2)
    });
}

/// A rollback deferred past its moment (the failed send could not get
/// the pilot mutex, so the unstage was handed to a waiting thread) must
/// land as a no-op once a NEWER turn has staged: unconditionally
/// restoring the old turn's books would flip the live turn
/// comment-only, retarget it, and rot its yield-diff baseline.
#[test]
fn a_stale_rollback_never_stomps_a_newer_turn() {
    let harness = OwnerHarness::start("hello");
    let session = connect_session(&harness.socket, "pilot").unwrap();
    let state = Mutex::new(PairState::new(session, None));
    let mut st = state.lock().unwrap();
    let staged_a = st.stage_turn("demo.txt", "one", false);
    // Turn B stages before A's deferred rollback lands.
    let _staged_b = st.stage_turn("demo.txt", "two", true);
    st.unstage_turn(staged_a); // the deferred rollback finally runs
    assert_eq!(
        st.last_seen_of("demo.txt").as_deref(),
        Some("two"),
        "a stale rollback must not replace the live turn's baseline"
    );
    assert!(
        st.comment_only,
        "a stale rollback must not unlatch the live turn's comment-only"
    );
}

/// A failed send's undo that could not get the pilot mutex is parked,
/// not handed to an untracked thread: while parked (contended lock) a
/// new send is refused, and once the lock is free the undo applies and
/// the park clears — the books are never a mix of two turns.
#[test]
fn a_parked_rollback_blocks_sends_until_it_applies() {
    let harness = OwnerHarness::start("hello");
    let session = connect_session(&harness.socket, "pilot").unwrap();
    let state = Mutex::new(PairState::new(session, None));
    let staged = state.lock().unwrap().stage_turn("demo.txt", "one", false);
    let parked = Mutex::new(Some(staged));
    // Contended: still parked, the caller must refuse to stage.
    assert!(!crate::pair_host::drain_parked(&parked, || None));
    assert!(parked.lock().unwrap().is_some(), "the undo is not lost");
    // Free: the undo applies and the park clears.
    assert!(crate::pair_host::drain_parked(&parked, || Some(
        state.lock().unwrap()
    )));
    assert!(parked.lock().unwrap().is_none());
    assert_eq!(
        state.lock().unwrap().last_seen_of("demo.txt"),
        None,
        "the parked undo restored the pre-turn baseline"
    );
}

/// Rollbacks are undo records: applied newest-first they must unwind
/// all the way back to the original state. Turn A fails (rollback
/// parked), turn B stages over A's residue and fails too; B's undo
/// puts A's books back and must also re-admit A's undo, or the seat
/// stays latched to A's staged look, comment-only flag, and target.
#[test]
fn nested_rollbacks_unwind_to_the_original_state() {
    let harness = OwnerHarness::start("hello");
    let session = connect_session(&harness.socket, "pilot").unwrap();
    let state = Mutex::new(PairState::new(session, None));
    let mut st = state.lock().unwrap();
    let staged_a = st.stage_turn("demo.txt", "one", false);
    let staged_b = st.stage_turn("demo.txt", "two", true);
    st.unstage_turn(staged_b); // B's send failed: newest undo first
    st.unstage_turn(staged_a); // then A's parked undo lands
    assert_eq!(
        st.last_seen_of("demo.txt"),
        None,
        "both failed turns unwound: no baseline remains"
    );
    assert!(!st.comment_only, "the original comment-only state is back");
    assert_eq!(st.target_file, None, "the original target is back");
}

/// claude's stdout is an external input: a `result` line with no
/// dispatched turn behind it (a duplicate emission, or a spontaneous
/// one) must end nothing. Queuing a phantom TurnEnd would release the
/// host's busy gate under a newer turn, and the handler's flag resets
/// would clear that turn's comment-only staging.
#[test]
fn an_unsolicited_result_ends_no_turn() {
    let harness = OwnerHarness::start("hello");
    let session = connect_session(&harness.socket, "pilot").unwrap();
    let state = Mutex::new(PairState::new(session, None));
    let mut fence = FenceMachine::new();
    let (turn_tx, turn_rx) = std::sync::mpsc::channel();
    let result = json!({ "type": "result", "is_error": false, "result": "done" });

    // A dispatched turn ends exactly once.
    state.lock().unwrap().turn_active = true;
    handle_claude_event(&state, &mut fence, &result, &turn_tx);
    assert!(
        turn_rx.try_recv().is_ok(),
        "the genuine result ends the turn"
    );

    // A newer comment-only turn stages, then the duplicate arrives.
    state.lock().unwrap().stage_turn("demo.txt", "one", true);
    handle_claude_event(&state, &mut fence, &result, &turn_tx);
    assert!(
        turn_rx.try_recv().is_err(),
        "a duplicate result must not queue a phantom TurnEnd"
    );
    assert!(
        state.lock().unwrap().comment_only,
        "a duplicate result must not clear the newer turn's comment-only staging"
    );
}

/// An edit taking over the caret supersedes an unresolved pending park:
/// once the region opens (and parks the caret at its start), the stale
/// ask-row park must never fire afterwards and yank the caret back.
#[test]
fn an_open_region_supersedes_a_pending_caret_park() {
    let harness = OwnerHarness::start("l0\nl1\nl2");
    let session = connect_session(&harness.socket, "pilot").unwrap();
    let state = Mutex::new(PairState::new(session, None));
    {
        let mut st = state.lock().unwrap();
        st.begin_turn("demo.txt", "l0\nl1\nl2", false);
        st.park_caret("demo.txt", 1);
        assert!(st.pending_caret.is_some(), "not live yet: the park waits");
    }
    // Ingest the bootstrap by hand (no pump thread, so nothing can
    // resolve the pending park before the edit lands).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut st = state.lock().unwrap();
        let _ = st.session.poll(|_| None);
        if st.session.is_live("demo.txt") {
            break;
        }
        drop(st);
        assert!(Instant::now() < deadline, "demo.txt never went live");
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut st = state.lock().unwrap();
    open_region(&mut st, "demo.txt", (0, 0), (0, 0));
    assert!(
        st.pending_caret.is_none(),
        "the edit's caret supersedes the pending park"
    );
}

/// A PARSEABLE whole-line header with an inverted range (`5-3`) drops
/// the edit through the OUTPUT channel, never stderr: the resident
/// navigator runs in-process, so an eprintln! would land on the
/// alternate screen and corrupt the render. The inverted header is a
/// one-token slip for the weak local models the whole-line form serves.
#[test]
fn an_inverted_fence_range_drops_the_edit_via_output_not_stderr() {
    let harness = OwnerHarness::start("l0\nl1\nl2\nl3\nl4\nl5");
    let session = connect_session(&harness.socket, "pilot").unwrap();
    let state = Mutex::new(PairState::new(session, None));
    state
        .lock()
        .unwrap()
        .begin_turn("demo.txt", "l0\nl1\nl2\nl3\nl4\nl5", false);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut st = state.lock().unwrap();
        let _ = st.session.poll(|_| None);
        if st.session.is_live("demo.txt") {
            break;
        }
        drop(st);
        assert!(Instant::now() < deadline, "demo.txt never went live");
        std::thread::sleep(Duration::from_millis(5));
    }
    let baseline = crate::output::snapshot("Navigator")
        .unwrap_or_default()
        .len();
    let mut st = state.lock().unwrap();
    open_region(&mut st, "demo.txt", (4, 0), (2, usize::MAX));
    assert!(st.discarding, "the inverted edit's body must be discarded");
    assert_eq!(
        st.session.doc_text("demo.txt"),
        Some("l0\nl1\nl2\nl3\nl4\nl5"),
        "nothing may be applied"
    );
    let lines = crate::output::snapshot("Navigator").unwrap_or_default();
    assert!(
        lines
            .iter()
            .skip(baseline)
            .any(|l| l.text.contains("inverted")),
        "the drop is reported on the OUTPUT channel"
    );
}

/// Offline slice of the local backend: one canned /v1/messages SSE turn
/// streams a fenced edit through the REAL fence machine and apply path
/// into the owner's replica, appends the assistant message, and ends
/// the turn cleanly. No Ollama needed.
#[test]
fn stream_turn_applies_a_fenced_edit() {
    let harness = OwnerHarness::start("hello world");
    let (state, stop, pump) = pumped_state(&harness);
    let (base_url, server) = serve_sse_once(vec![
        "Let me fix that.\n",
        "<<<EDIT demo.txt:0:6-0:11>",
        ">>\n",
        "streamed",
        " edit",
        "\n<<<END>>>\nDone.\n",
    ]);

    let (turn_tx, turn_rx) = std::sync::mpsc::channel();
    let mut messages = vec![json!({ "role": "user", "content": "fix demo.txt" })];
    local::stream_turn(
        &base_url,
        "test-model",
        PAIR_SYSTEM_PROMPT,
        &mut messages,
        &state,
        &turn_tx,
    );
    server.join().unwrap();

    harness.wait_until("the streamed edit to converge", |h| {
        h.doc().as_deref() == Some("hello streamed edit")
    });
    assert!(
        harness.remote_edit_count() >= 3,
        "the edit must arrive as multiple streamed ops, got {}",
        harness.remote_edit_count()
    );
    let end = turn_rx.try_recv().expect("turn ended");
    assert!(!end.is_error && !end.cancelled);
    assert_eq!(messages.len(), 2, "assistant reply appended");
    assert_eq!(messages[1]["role"], "assistant");
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("<<<EDIT demo.txt:0:6-0:11>>>"),
        "conversation history carries the fence verbatim"
    );
    assert!(!state.lock().unwrap().turn_active());
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();
}

/// #1207: the stream is announced before its first edit (the region's
/// delete), so the host folds every edit of it, delete included, into
/// one undo step; announced after, the delete was a step of its own.
#[test]
fn a_stream_is_announced_before_its_first_edit() {
    let harness = OwnerHarness::start("hello world");
    let (state, stop, pump) = pumped_state(&harness);
    let (base_url, server) = serve_sse_once(vec![
        "<<<EDIT demo.txt:0:6-0:11>>>\n",
        "streamed",
        "\n<<<END>>>\n",
    ]);
    let (turn_tx, _turn_rx) = std::sync::mpsc::channel();
    let mut messages = vec![json!({ "role": "user", "content": "fix demo.txt" })];
    local::stream_turn(
        &base_url,
        "test-model",
        PAIR_SYSTEM_PROMPT,
        &mut messages,
        &state,
        &turn_tx,
    );
    server.join().unwrap();
    harness.wait_until("the streamed edit to converge", |h| {
        h.doc().as_deref() == Some("hello streamed")
    });
    harness.wait_until("the stream to end", |h| {
        h.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, CollabEvent::StreamState { active: false, .. }))
    });
    let events = harness.events.lock().unwrap();
    let first = |pred: &dyn Fn(&CollabEvent) -> bool| events.iter().position(pred);
    let announced = first(&|e| matches!(e, CollabEvent::StreamState { active: true, .. }))
        .expect("the stream is announced");
    let edited = first(&|e| matches!(e, CollabEvent::RemoteEdit { .. })).expect("the stream edits");
    let ended = first(&|e| matches!(e, CollabEvent::StreamState { active: false, .. }))
        .expect("the stream ends");
    assert!(
        announced < edited,
        "announced at {announced}, first edit at {edited}"
    );
    // Negative: the end still follows the last edit.
    let last_edit = events
        .iter()
        .rposition(|e| matches!(e, CollabEvent::RemoteEdit { .. }))
        .unwrap();
    assert!(
        last_edit < ended,
        "last edit at {last_edit}, ended at {ended}"
    );
    drop(events);
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();
}

/// A dead endpoint fails the turn — naming the endpoint — instead of
/// wedging turn_active or killing the seat.
#[test]
fn stream_turn_names_a_dead_endpoint() {
    let harness = OwnerHarness::start("hello world");
    let (state, stop, pump) = pumped_state(&harness);
    // A bound-then-dropped port: connection refused.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let (turn_tx, turn_rx) = std::sync::mpsc::channel();
    let mut messages = vec![json!({ "role": "user", "content": "hi" })];
    state.lock().unwrap().turn_active = true;
    local::stream_turn(
        &dead,
        "test-model",
        PAIR_SYSTEM_PROMPT,
        &mut messages,
        &state,
        &turn_tx,
    );
    let end = turn_rx.try_recv().expect("turn ended");
    assert!(end.is_error);
    assert!(
        end.text.contains(&dead),
        "the failure names the endpoint: {}",
        end.text
    );
    assert!(!state.lock().unwrap().turn_active());
    assert!(
        messages.is_empty(),
        "the unanswered user message is popped so the next ask starts clean"
    );
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();
}

/// A stream that dies before `message_stop` is a FAILED turn: the old
/// code reported success ("claude finished its turn") while the partial
/// edit had been reverted and nothing happened - and it appended the
/// partial text as an assistant message, corrupting the conversation.
#[test]
fn a_mid_stream_drop_reports_failure_and_balances_the_conversation() {
    let harness = OwnerHarness::start("hello world");
    let (state, stop, pump) = pumped_state(&harness);
    let ev = json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": { "type": "text_delta", "text": "half a rep" },
    });
    let (base_url, server) = serve_http_once(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
             connection: close\r\n\r\nevent: content_block_delta\ndata: {ev}\n\n"
    ));
    let (turn_tx, turn_rx) = std::sync::mpsc::channel();
    let mut messages = vec![json!({ "role": "user", "content": "hi" })];
    state.lock().unwrap().turn_active = true;
    local::stream_turn(
        &base_url,
        "test-model",
        PAIR_SYSTEM_PROMPT,
        &mut messages,
        &state,
        &turn_tx,
    );
    server.join().unwrap();
    let end = turn_rx.try_recv().expect("turn ended");
    assert!(
        end.is_error,
        "an unclean stream end is a failure: {}",
        end.text
    );
    assert!(
        messages.is_empty(),
        "the conversation stays balanced for the next ask"
    );
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();
}

/// The HTTP error body carries the fix ("model 'x' not found, try
/// pulling it first"); throwing it away left an opaque status code.
#[test]
fn an_http_error_body_reaches_the_surfaced_failure() {
    let harness = OwnerHarness::start("hello world");
    let (state, stop, pump) = pumped_state(&harness);
    let (base_url, server) = serve_http_once(String::from(
        "HTTP/1.1 404 Not Found\r\ncontent-type: application/json\r\n\
             connection: close\r\n\r\n{\"error\":\"model 'qwn3' not found, try pulling it\"}",
    ));
    let (turn_tx, turn_rx) = std::sync::mpsc::channel();
    let mut messages = vec![json!({ "role": "user", "content": "hi" })];
    state.lock().unwrap().turn_active = true;
    local::stream_turn(
        &base_url,
        "qwn3",
        PAIR_SYSTEM_PROMPT,
        &mut messages,
        &state,
        &turn_tx,
    );
    server.join().unwrap();
    let end = turn_rx.try_recv().expect("turn ended");
    assert!(end.is_error);
    assert!(
        end.text.contains("not found, try pulling it"),
        "the body's own words must survive: {}",
        end.text
    );
    assert!(messages.is_empty());
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();
}

/// `stop_reason: max_tokens` means the reply (and any fence in it) was
/// cut off; reporting success would pretend the half-edit was the turn.
#[test]
fn a_token_limit_truncation_is_a_failed_turn() {
    let harness = OwnerHarness::start("hello world");
    let (state, stop, pump) = pumped_state(&harness);
    let delta = json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": { "type": "text_delta", "text": "prose" },
    });
    let md = json!({
        "type": "message_delta",
        "delta": { "stop_reason": "max_tokens" },
    });
    let (base_url, server) = serve_http_once(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
             connection: close\r\n\r\n\
             event: content_block_delta\ndata: {delta}\n\n\
             event: message_delta\ndata: {md}\n\n\
             event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
    ));
    let (turn_tx, turn_rx) = std::sync::mpsc::channel();
    let mut messages = vec![json!({ "role": "user", "content": "hi" })];
    state.lock().unwrap().turn_active = true;
    local::stream_turn(
        &base_url,
        "test-model",
        PAIR_SYSTEM_PROMPT,
        &mut messages,
        &state,
        &turn_tx,
    );
    server.join().unwrap();
    let end = turn_rx.try_recv().expect("turn ended");
    assert!(end.is_error, "truncation must not read as success");
    assert!(end.text.contains("truncated"), "{}", end.text);
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();
}

/// The environment credential only ever travels to https or loopback
/// destinations; a cleartext remote hop gets the harmless placeholder.
#[test]
fn the_credential_never_rides_cleartext_to_a_remote_host() {
    use local::auth_for;
    assert_eq!(
        auth_for("http://box:8080", Some("secret")),
        (String::from("croft"), None),
    );
    assert_eq!(
        auth_for("http://localhost.evil.com:80", Some("secret")),
        (String::from("croft"), None),
    );
    let bearer = Some(String::from("Bearer t"));
    assert_eq!(
        auth_for("http://localhost:11434", Some("t")),
        (String::from("t"), bearer.clone()),
    );
    assert_eq!(
        auth_for("http://127.0.0.1:11434", Some("t")),
        (String::from("t"), bearer.clone()),
    );
    assert_eq!(
        auth_for("http://[::1]:11434", Some("t")),
        (String::from("t"), bearer.clone()),
    );
    assert_eq!(
        auth_for("https://gw.example.com", Some("t")),
        (String::from("t"), bearer),
    );
    assert_eq!(
        auth_for("http://localhost:11434", None),
        (String::from("croft"), None)
    );
    // Userinfo must not fool the gate: the destination here is the
    // remote host, not localhost.
    assert_eq!(
        auth_for("http://localhost:pass@evil.example", Some("secret")),
        (String::from("croft"), None),
    );
    assert_eq!(
        auth_for("http://127.0.0.2:11434", Some("t")),
        (String::from("t"), Some(String::from("Bearer t"))),
        "any 127.x address is loopback"
    );
}

/// The wall-clock ceiling is enforced per CHUNK: a stream of non-newline
/// bytes never completes a line, so a per-line check would have let a
/// trickling endpoint hold the seat busy indefinitely.
#[test]
fn a_trickling_stream_is_cut_at_the_deadline() {
    let harness = OwnerHarness::start("hello world");
    let (state, stop, pump) = pumped_state(&harness);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let stop_trickle = Arc::new(AtomicBool::new(false));
    let server = {
        let stop_trickle = Arc::clone(&stop_trickle);
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req = [0u8; 4096];
            let _ = sock.read(&mut req);
            let _ = sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n");
            // Non-newline bytes, forever (until the test releases us).
            while !stop_trickle.load(Ordering::Relaxed) {
                if sock.write_all(b".").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };
    let (turn_tx, turn_rx) = std::sync::mpsc::channel();
    let mut messages = vec![json!({ "role": "user", "content": "hi" })];
    state.lock().unwrap().turn_active = true;
    let started = Instant::now();
    local::stream_turn_until(
        &base_url,
        "test-model",
        PAIR_SYSTEM_PROMPT,
        &mut messages,
        &state,
        &turn_tx,
        Instant::now() + Duration::from_millis(300),
    );
    let elapsed = started.elapsed();
    stop_trickle.store(true, Ordering::Relaxed);
    let _ = server.join();
    let end = turn_rx.try_recv().expect("turn ended");
    assert!(end.is_error, "a deadline cut is a failure: {}", end.text);
    assert!(
        elapsed < Duration::from_secs(3),
        "the ceiling must interrupt the trickle promptly, took {elapsed:?}"
    );
    assert!(messages.is_empty(), "the conversation stays balanced");
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();
}

/// The deadline used to be polled only BETWEEN reads: a peer that sent
/// headers and then held the body open without a single byte parked the
/// loop inside the blocking `read` (up to the 300s socket timeout) past
/// the ceiling. Each wait is bounded by the remaining turn time instead.
#[test]
fn a_silent_stream_body_is_cut_at_the_deadline() {
    let harness = OwnerHarness::start("hello world");
    let (state, stop, pump) = pumped_state(&harness);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    // The thread ends on its own after the silence window; dropping the
    // handle detaches it.
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut req = [0u8; 4096];
        let _ = sock.read(&mut req);
        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n");
        // Not one body byte: hold the connection open in silence, long
        // enough that only the deadline can explain a prompt return.
        std::thread::sleep(Duration::from_secs(4));
    });
    let (turn_tx, turn_rx) = std::sync::mpsc::channel();
    let mut messages = vec![json!({ "role": "user", "content": "hi" })];
    state.lock().unwrap().turn_active = true;
    let started = Instant::now();
    local::stream_turn_until(
        &base_url,
        "test-model",
        PAIR_SYSTEM_PROMPT,
        &mut messages,
        &state,
        &turn_tx,
        Instant::now() + Duration::from_millis(300),
    );
    let elapsed = started.elapsed();
    let end = turn_rx.try_recv().expect("turn ended");
    assert!(end.is_error, "a deadline cut is a failure: {}", end.text);
    assert!(
        elapsed < Duration::from_secs(3),
        "the ceiling must interrupt a silent body promptly, took {elapsed:?}"
    );
    assert!(messages.is_empty(), "the conversation stays balanced");
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();
}

/// A deadline cut must also RELEASE the connection. Returning on time
/// while a detached reader stayed blocked in `read` kept the socket and
/// a thread alive for up to the 300s socket timeout per failed turn, so
/// repeated failures accumulated both. The server observing EOF right
/// after the cut is the proof the transport let go.
#[test]
fn a_deadline_cut_releases_the_connection() {
    let harness = OwnerHarness::start("hello world");
    let (state, stop, pump) = pumped_state(&harness);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (eof_tx, eof_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut req = [0u8; 4096];
        let _ = sock.read(&mut req);
        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n");
        // Silent body: block until the CLIENT hangs up. EOF or a reset
        // is the release; draining any leftover request bytes first.
        let mut probe = [0u8; 64];
        loop {
            match sock.read(&mut probe) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = eof_tx.send(());
    });
    let (turn_tx, turn_rx) = std::sync::mpsc::channel();
    let mut messages = vec![json!({ "role": "user", "content": "hi" })];
    state.lock().unwrap().turn_active = true;
    local::stream_turn_until(
        &base_url,
        "test-model",
        PAIR_SYSTEM_PROMPT,
        &mut messages,
        &state,
        &turn_tx,
        Instant::now() + Duration::from_millis(300),
    );
    let end = turn_rx.try_recv().expect("turn ended");
    assert!(end.is_error, "a deadline cut is a failure: {}", end.text);
    eof_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the deadline cut must close the connection, not park it until the socket timeout");
    assert!(messages.is_empty(), "the conversation stays balanced");
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();
}

/// Malformed SSE data mid-stream silently dropped a chunk of the reply
/// and still reported success; a broken endpoint must fail the turn.
#[test]
fn malformed_sse_data_fails_the_turn() {
    let harness = OwnerHarness::start("hello world");
    let (state, stop, pump) = pumped_state(&harness);
    let (base_url, server) = serve_http_once(String::from(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
             connection: close\r\n\r\ndata: {not json\n\n\
             event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ));
    let (turn_tx, turn_rx) = std::sync::mpsc::channel();
    let mut messages = vec![json!({ "role": "user", "content": "hi" })];
    state.lock().unwrap().turn_active = true;
    local::stream_turn(
        &base_url,
        "test-model",
        PAIR_SYSTEM_PROMPT,
        &mut messages,
        &state,
        &turn_tx,
    );
    server.join().unwrap();
    let end = turn_rx.try_recv().expect("turn ended");
    assert!(end.is_error);
    assert!(end.text.contains("malformed"), "{}", end.text);
    assert!(messages.is_empty());
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();
}

/// A 3xx from the endpoint must NOT be followed: ureq replays the
/// x-api-key header to the redirect target, so a loopback endpoint
/// answering 302 could bounce the credential to an arbitrary host. The
/// redirect surfaces as a failed turn naming the status.
#[test]
fn a_redirect_is_refused_not_followed() {
    let harness = OwnerHarness::start("hello world");
    let (state, stop, pump) = pumped_state(&harness);
    let (base_url, server) = serve_http_once(String::from(
        "HTTP/1.1 302 Found\r\nlocation: http://127.0.0.1:1/v1/messages\r\n\
             connection: close\r\n\r\n",
    ));
    let (turn_tx, turn_rx) = std::sync::mpsc::channel();
    let mut messages = vec![json!({ "role": "user", "content": "hi" })];
    state.lock().unwrap().turn_active = true;
    local::stream_turn(
        &base_url,
        "test-model",
        PAIR_SYSTEM_PROMPT,
        &mut messages,
        &state,
        &turn_tx,
    );
    server.join().unwrap();
    let end = turn_rx.try_recv().expect("turn ended");
    assert!(end.is_error);
    assert!(
        end.text.contains("302"),
        "the redirect is refused with its status, not followed: {}",
        end.text
    );
    assert!(messages.is_empty());
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();
}

/// The childless local seat end to end: seat_local connects the guest
/// seat, the queued turn streams the stub endpoint's fenced edit into
/// the owner's replica, the turn ends cleanly, and shutdown returns
/// promptly (no child, no grace-kill).
#[test]
fn seat_local_drives_a_turn_end_to_end() {
    let harness = OwnerHarness::start("hello world");
    let (base_url, server) = serve_sse_once(vec![
        "Let me fix that.\n",
        "<<<EDIT demo.txt:0:6-0:11>",
        ">>\n",
        "streamed",
        " edit",
        "\n<<<END>>>\nDone.\n",
    ]);
    let pilot = seat_local(&harness.socket, "pilot", &base_url, "test-model", None).unwrap();
    assert!(!pilot.reader_finished(), "a local seat never loses a child");

    send_turn(&pilot.state, &pilot.sink, "fix demo.txt").unwrap();
    let end = pilot
        .turn_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("turn ends");
    assert!(!end.is_error && !end.cancelled, "{}", end.text);
    server.join().unwrap();

    harness.wait_until("the streamed edit to converge", |h| {
        h.doc().as_deref() == Some("hello streamed edit")
    });
    harness.wait_until("stream-state inactive", |h| {
        h.stream_states().contains(&("pilot".to_string(), false))
    });

    let start = Instant::now();
    pilot.shutdown();
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "local shutdown must not block on a grace-kill"
    );
}

/// PairHost::spawn dispatches on the provider: a Local config refuses to
/// seat without a model (there is no CLI default to fall back to), and
/// with one it seats childless and drives a whole-line ask turn from the
/// endpoint into the owner's replica.
#[test]
fn pair_host_spawns_local_backend_without_a_child() {
    let harness = OwnerHarness::start("hello world");
    let no_model = PairConfig {
        socket: harness.socket.clone(),
        workspace: harness._dir.path().to_path_buf(),
        name: String::from("claude"),
        model: None,
        task: None,
        provider: Provider::Local {
            base_url: String::from("http://127.0.0.1:9"),
        },
    };
    let err = match crate::pair_host::PairHost::spawn(no_model) {
        Err(e) => e,
        Ok(_) => panic!("a local provider without a model must refuse to seat"),
    };
    assert!(
        err.to_string().contains("--model"),
        "the refusal must hint at --model: {err}"
    );

    let (base_url, server) = serve_sse_once(vec![
        "<<<EDIT demo.txt:0-0>",
        ">>\nhello streamed edit\n",
        "<<<END>>>\n",
    ]);
    let mut host = crate::pair_host::PairHost::spawn(PairConfig {
        socket: harness.socket.clone(),
        workspace: harness._dir.path().to_path_buf(),
        name: String::from("claude"),
        model: Some(String::from("test-model")),
        task: None,
        provider: Provider::Local { base_url },
    })
    .unwrap();
    assert!(!host.is_busy());
    host.send_ask_turn("demo.txt", (0, 0), "", "rewrite line 0", "hello world")
        .unwrap();
    server.join().unwrap();
    harness.wait_until("the whole-line edit to converge", |h| {
        h.doc().as_deref() == Some("hello streamed edit")
    });
    harness.wait_until("the turn to end", |h| {
        let _ = h; // the turn end arrives on the host's own channel
        host.poll().iter().any(|e| {
            matches!(
                e,
                crate::pair_host::PairEvent::TurnDone {
                    cancelled: false,
                    failed: None
                }
            )
        })
    });
}

/// The badge title names who is actually typing: bare caret name on the
/// claude backend, "name (model)" on a local one.
#[test]
fn local_navigator_badge_names_the_model() {
    let harness = OwnerHarness::start("hello world");
    let host = crate::pair_host::PairHost::spawn(PairConfig {
        socket: harness.socket.clone(),
        workspace: harness._dir.path().to_path_buf(),
        name: String::from("claude"),
        model: Some(String::from("test-model")),
        task: None,
        provider: Provider::Local {
            base_url: String::from("http://127.0.0.1:9"),
        },
    })
    .unwrap();
    assert_eq!(host.title(), "claude (test-model)");

    let mut cmd = Command::new("cat");
    cmd.stdin(Stdio::piped());
    let host =
        crate::pair_host::PairHost::spawn_cmd(&harness.socket, "navigator", None, cmd).unwrap();
    assert_eq!(host.title(), "navigator");
}

/// Spawn the pilot against the scripted claude on a background thread
/// (it blocks until the turn ends and its input hits EOF).
fn spawn_pilot(
    socket: &Path,
    script: &Path,
    log: &Path,
    mode: &str,
) -> std::thread::JoinHandle<Result<()>> {
    let mut cmd = Command::new("python3");
    cmd.arg(script).arg(log).arg(mode);
    let socket = socket.to_path_buf();
    std::thread::spawn(move || {
        let pilot = seat_pilot(&socket, "pilot", cmd, None)?;
        run_pilot(
            pilot,
            "pilot",
            Some("fix demo.txt".into()),
            &mut std::io::Cursor::new(Vec::new()),
        )
    })
}

/// End to end: the fake claude's fenced edit streams through the pilot
/// into the owner's replica as MULTIPLE ops (token streaming, not one
/// bulk insert), bracketed by StreamState active/inactive.
#[test]
fn pilot_streams_a_fenced_edit_into_the_owner() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let harness = OwnerHarness::start("hello world");
    let script = harness._dir.path().join("fake_claude.py");
    std::fs::write(&script, FAKE_CLAUDE).unwrap();
    let log = harness._dir.path().join("stdin.log");

    let pilot = spawn_pilot(&harness.socket, &script, &log, "stream");

    harness.wait_until("the streamed edit to converge", |h| {
        h.doc().as_deref() == Some("hello streamed edit")
    });
    harness.wait_until("stream-state inactive", |h| {
        h.stream_states().contains(&("pilot".to_string(), false))
    });
    assert!(
        harness
            .stream_states()
            .contains(&("pilot".to_string(), true)),
        "owner saw the stream start"
    );
    assert!(
        harness.remote_edit_count() >= 3,
        "the edit must arrive as multiple streamed ops, got {}",
        harness.remote_edit_count()
    );
    pilot.join().unwrap().expect("pilot exits cleanly");
}

/// A claude child that never exits on stdin EOF (the real CLI's MCP
/// teardown can linger) must not hang the pilot's exit: cleanup kills
/// it after a short grace instead of joining the reader forever.
#[test]
fn pilot_exit_does_not_hang_on_a_lingering_claude() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let harness = OwnerHarness::start("hello world");
    let script = harness._dir.path().join("fake_claude.py");
    std::fs::write(&script, FAKE_CLAUDE).unwrap();
    let log = harness._dir.path().join("stdin.log");

    let pilot = spawn_pilot(&harness.socket, &script, &log, "linger");
    harness.wait_until("the streamed edit to converge", |h| {
        h.doc().as_deref() == Some("hello streamed edit")
    });
    // The pilot's REPL input is already at EOF; once the turn ends it
    // must return promptly despite the child sleeping for 60s — half
    // that sleep keeps the discriminant while absorbing suite load.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !pilot.is_finished() {
        assert!(
            Instant::now() < deadline,
            "pilot exit hung on the lingering claude child"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    pilot.join().unwrap().expect("pilot exits cleanly");
}

/// Cancel mid-stream: the owner broadcasts StreamCancel while the fake
/// claude is blocked mid-body; the pilot reverts the streamed text,
/// writes a control_request interrupt to claude's stdin, and drops the
/// post-cancel deltas. The conversation still ends its turn cleanly.
#[test]
fn pilot_cancel_reverts_and_interrupts() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let harness = OwnerHarness::start("hello world");
    let script = harness._dir.path().join("fake_claude.py");
    std::fs::write(&script, FAKE_CLAUDE).unwrap();
    let log = harness._dir.path().join("stdin.log");

    let pilot = spawn_pilot(&harness.socket, &script, &log, "cancel");

    // Wait until part of the body landed (stream mid-flight).
    harness.wait_until("the partial body to converge", |h| {
        h.doc().is_some_and(|d| d.contains("streamed"))
    });
    harness.owner.lock().unwrap().send_stream_cancel();

    // The pilot reverts and the turn still completes.
    harness.wait_until("the revert to converge", |h| {
        h.doc().as_deref() == Some("hello world")
    });
    pilot.join().unwrap().expect("pilot exits cleanly");

    assert_eq!(
        harness.doc().as_deref(),
        Some("hello world"),
        "post-cancel deltas must never land"
    );
    harness.wait_until("stream-state inactive", |h| {
        h.stream_states().contains(&("pilot".to_string(), false))
    });
    let log_text = std::fs::read_to_string(&log).unwrap();
    assert!(
        log_text.contains("control_request") && log_text.contains("interrupt"),
        "the interrupt must land on claude's stdin: {log_text:?}"
    );
}

/// Feed `input` to a fresh machine in `chunk`-sized pieces plus finish,
/// collecting every event.
fn run_chunks(input: &str, chunk: usize) -> Vec<FenceEvent> {
    let mut m = FenceMachine::new();
    let mut events = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    for piece in chars.chunks(chunk) {
        let s: String = piece.iter().collect();
        events.extend(m.push(&s));
    }
    events.extend(m.finish());
    events
}

/// Concatenated body text of every EditBody event.
fn body_of(events: &[FenceEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            FenceEvent::EditBody(s) => Some(s.as_str()),
            _ => None,
        })
        .collect()
}

/// Concatenated commentary of every Commentary event.
fn commentary_of(events: &[FenceEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            FenceEvent::Commentary(s) => Some(s.as_str()),
            _ => None,
        })
        .collect()
}

/// Concatenated body of every NoteBody event.
fn note_body_of(events: &[FenceEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            FenceEvent::NoteBody(s) => Some(s.as_str()),
            _ => None,
        })
        .collect()
}

/// A NOTE fence parses across arbitrary delta splits: the row binds
/// rightmost (file names may carry ':'), the body accumulates, and the
/// fence closes on END without leaking markers into commentary.
#[test]
fn note_fence_parses_across_delta_splits() {
    let input = "Look at this.\n\
                     <<<NOTE src/f.rs:12>>>\n\
                     the caller in cli.rs:88\nstill expects Config\n\
                     <<<END>>>\n\
                     trailing\n";
    for chunk in [1, 3, 7, input.len()] {
        let events = run_chunks(input, chunk);
        let start = events
            .iter()
            .find_map(|e| match e {
                FenceEvent::NoteStart { file, row } => Some((file.clone(), *row)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no NoteStart at chunk {chunk}"));
        assert_eq!(start, ("src/f.rs".to_string(), 12), "chunk {chunk}");
        assert_eq!(
            note_body_of(&events),
            "the caller in cli.rs:88\nstill expects Config",
            "chunk {chunk}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, FenceEvent::NoteEnd))
                .count(),
            1,
            "chunk {chunk}"
        );
        let commentary = commentary_of(&events);
        assert!(commentary.contains("Look at this."), "chunk {chunk}");
        assert!(commentary.contains("trailing"), "chunk {chunk}");
        assert!(!commentary.contains("<<<NOTE"), "chunk {chunk}");
    }
}

/// Malformed NOTE headers degrade to commentary, exactly like EDIT ones.
#[test]
fn malformed_note_headers_become_commentary() {
    for bad in [
        "<<<NOTE src/f.rs>>>\nbody\n<<<END>>>\n",
        "<<<NOTE src/f.rs:x>>>\nbody\n<<<END>>>\n",
        "<<<NOTE :3>>>\nbody\n<<<END>>>\n",
        "<<<NOTE>>>\nbody\n<<<END>>>\n",
    ] {
        let events = run_chunks(bad, 5);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FenceEvent::NoteStart { .. })),
            "{bad:?} must not start a note"
        );
    }
}

/// A turn ending mid-note aborts it so nothing anchors.
#[test]
fn unterminated_note_aborts_at_finish() {
    let mut m = FenceMachine::new();
    let mut events = m.push("<<<NOTE f.rs:3>>>\nhalf a thought");
    events.extend(m.finish());
    assert!(events.iter().any(|e| matches!(e, FenceEvent::NoteAbort)));
    assert!(!events.iter().any(|e| matches!(e, FenceEvent::NoteEnd)));
}

/// Interop guard: EDIT and NOTE fences coexist in one turn and neither
/// disturbs the other's events (the 0.1.634 edit protocol is unchanged).
#[test]
fn edit_and_note_fences_coexist_in_one_turn() {
    let input = "<<<EDIT a.rs:0:0-0:0>>>\nnew\n<<<END>>>\n\
                     <<<NOTE b.rs:4>>>\nremark\n<<<END>>>\n";
    let events = run_chunks(input, 2);
    assert_eq!(body_of(&events), "new");
    assert_eq!(note_body_of(&events), "remark");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, FenceEvent::EditEnd))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, FenceEvent::NoteEnd))
            .count(),
        1
    );
}

/// The core streaming property: a full fenced edit parses identically no
/// matter how the deltas split it, down to one char at a time, and the
/// body arrives across multiple EditBody events (streaming), not one.
#[test]
fn fence_parses_identically_across_arbitrary_delta_splits() {
    let input = "I will fix the loop.\n\
                     <<<EDIT src/f.rs:3:0-5:10>>>\n\
                     for x in xs {\n    go(x);\n}\n\
                     <<<END>>>\n\
                     Done.\n";
    for chunk in [1, 2, 3, 7, input.len()] {
        let events = run_chunks(input, chunk);
        let start = events
            .iter()
            .find_map(|e| match e {
                FenceEvent::EditStart { file, start, end } => Some((file.clone(), *start, *end)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no EditStart at chunk {chunk}"));
        assert_eq!(
            start,
            ("src/f.rs".to_string(), (3, 0), (5, 10)),
            "chunk {chunk}"
        );
        assert_eq!(
            body_of(&events),
            "for x in xs {\n    go(x);\n}",
            "chunk {chunk}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, FenceEvent::EditEnd))
                .count(),
            1,
            "chunk {chunk}"
        );
        let commentary = commentary_of(&events);
        assert!(
            commentary.contains("I will fix the loop."),
            "chunk {chunk}: {commentary:?}"
        );
        assert!(commentary.contains("Done."), "chunk {chunk}");
        assert!(
            !commentary.contains("<<<EDIT"),
            "header must not leak into commentary at chunk {chunk}"
        );
    }
    // Char-at-a-time must still stream the body incrementally.
    let events = run_chunks(input, 1);
    let bodies = events
        .iter()
        .filter(|e| matches!(e, FenceEvent::EditBody(_)))
        .count();
    assert!(bodies > 1, "body must stream, got {bodies} event(s)");
}

/// The whole-line header form `<file>:SR-ER` (two integers) opens an
/// edit covering those entire rows, inclusive: start (SR,0) and an end
/// column that clamps to the end of row ER.
#[test]
fn parse_header_accepts_whole_line_range() {
    let event = parse_header("<<<EDIT demo.txt:3-5>>>").expect("whole-line header parses");
    let FenceEvent::EditStart { file, start, end } = event else {
        panic!("not an EditStart: {event:?}");
    };
    assert_eq!(file, "demo.txt");
    assert_eq!(start, (3, 0));
    assert_eq!(end, (5, usize::MAX));
}

/// The spike's coordinate slip, fixed: a whole-line fence on the exact
/// spike buffer replaces the row entirely — no `+ name` leftover — using
/// the same range_bytes clamping the pilot's open_region applies.
#[test]
fn whole_line_edit_replaces_entire_rows() {
    let buffer = "def greet(name):\n    return \"hi \" + name\n";
    let input = "<<<EDIT greet.py:1-1>>>\n    return f\"Hello, {name}!\"\n<<<END>>>\n";
    let events = run_chunks(input, 3);
    let (start, end) = events
        .iter()
        .find_map(|e| match e {
            FenceEvent::EditStart { start, end, .. } => Some((*start, *end)),
            _ => None,
        })
        .expect("whole-line fence opens an edit");
    let lines: Vec<String> = buffer.split('\n').map(String::from).collect();
    let (s, e) = range_bytes(&lines, start, end);
    let result = format!("{}{}{}", &buffer[..s], body_of(&events), &buffer[e..]);
    assert_eq!(result, "def greet(name):\n    return f\"Hello, {name}!\"\n");
}

/// A truncated four-int header (`file:SR:SC-ER`, missing its end column)
/// must NOT fall back to the whole-line form: its "file" would be the
/// bogus `file:SR`. It stays commentary.
#[test]
fn truncated_four_int_header_stays_commentary() {
    assert!(parse_header("<<<EDIT src/f.rs:3:0-5>>>").is_none());
}

/// A header that fails to parse is commentary, never an edit.
#[test]
fn malformed_headers_become_commentary() {
    for bad in [
        "<<<EDIT src/f.rs:3:0-5>>>\nbody\n<<<END>>>\n",
        "<<<EDIT src/f.rs>>>\nbody\n<<<END>>>\n",
        "<<<EDIT src/f.rs:a:0-5:10>>>\nbody\n<<<END>>>\n",
        "<<<EDIT>>>\nbody\n<<<END>>>\n",
    ] {
        let events = run_chunks(bad, 5);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FenceEvent::EditStart { .. })),
            "{bad:?} must not start an edit"
        );
        assert!(
            commentary_of(&events).contains("body"),
            "{bad:?}: the un-fenced body is commentary"
        );
    }
}

/// Exactly one trailing newline is stripped: the one that separates the
/// body from the END marker. A body that ends in a blank line keeps it.
#[test]
fn end_marker_strips_exactly_one_trailing_newline() {
    let input = "<<<EDIT f:0:0-0:0>>>\nabc\n\n<<<END>>>\n";
    for chunk in [1, 4, input.len()] {
        assert_eq!(body_of(&run_chunks(input, chunk)), "abc\n", "chunk {chunk}");
    }
}

/// A body line that merely contains the END marker text with a suffix is
/// body, not a terminator.
#[test]
fn end_marker_with_trailing_chars_is_body() {
    let input = "<<<EDIT f:0:0-0:0>>>\nkeep <<<END>>>ish\n<<<END>>>\n";
    for chunk in [1, 3, input.len()] {
        assert_eq!(
            body_of(&run_chunks(input, chunk)),
            "keep <<<END>>>ish",
            "chunk {chunk}"
        );
    }
}

/// Two fences in one turn each produce their own start/body/end, with
/// the commentary between them intact.
#[test]
fn multiple_fences_in_one_turn() {
    let input = "<<<EDIT a:0:0-0:0>>>\nfirst\n<<<END>>>\n\
                     between\n\
                     <<<EDIT b:1:2-3:4>>>\nsecond\n<<<END>>>\n";
    let events = run_chunks(input, 2);
    let starts: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            FenceEvent::EditStart { file, .. } => Some(file.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(starts, ["a", "b"]);
    assert_eq!(body_of(&events), "firstsecond");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, FenceEvent::EditEnd))
            .count(),
        2
    );
    assert!(commentary_of(&events).contains("between"));
}

/// The model stopping mid-fence (turn ends before <<<END>>>) aborts the
/// edit so the pilot can revert what already streamed in.
#[test]
fn unterminated_fence_aborts_at_finish() {
    let mut m = FenceMachine::new();
    let mut events = m.push("<<<EDIT f:0:0-0:0>>>\npartial body");
    events.extend(m.finish());
    assert!(
        events.iter().any(|e| matches!(e, FenceEvent::EditAbort)),
        "{events:?}"
    );
    // The machine is reusable for the next turn afterwards.
    let events = {
        let mut e = m.push("plain text\n");
        e.extend(m.finish());
        e
    };
    assert!(commentary_of(&events).contains("plain text"));
}

/// An END marker arriving exactly at end-of-turn (no trailing newline)
/// still closes the fence.
#[test]
fn end_marker_at_end_of_turn_closes_the_fence() {
    let mut m = FenceMachine::new();
    let mut events = m.push("<<<EDIT f:0:0-0:0>>>\nbody\n<<<END>>>");
    events.extend(m.finish());
    assert!(events.iter().any(|e| matches!(e, FenceEvent::EditEnd)));
    assert_eq!(body_of(&events), "body");
}

/// File names with colons and dashes still parse (coords bind rightmost).
#[test]
fn header_files_with_colons_and_dashes_parse() {
    let input = "<<<EDIT a-b:c.rs:10:2-11:0>>>\nx\n<<<END>>>\n";
    let events = run_chunks(input, input.len());
    match events
        .iter()
        .find(|e| matches!(e, FenceEvent::EditStart { .. }))
    {
        Some(FenceEvent::EditStart { file, start, end }) => {
            assert_eq!(file, "a-b:c.rs");
            assert_eq!((*start, *end), ((10, 2), (11, 0)));
        }
        other => panic!("expected EditStart, got {other:?}"),
    }
}

/// Anchor-shift math: spans entirely before an offset shift it by the
/// span's size delta; spans straddling it clamp to the span's new end;
/// spans after leave it alone.
#[test]
fn shift_offset_handles_before_straddle_and_after() {
    // Insert of 3 bytes at 2, entirely before offset 10.
    let ins = ResolvedSpan {
        at: 2,
        deleted: 0,
        inserted: "abc".into(),
    };
    assert_eq!(shift_offset(10, &ins), 13);
    // Delete of 4 bytes ending exactly at the offset: still "before".
    let del = ResolvedSpan {
        at: 6,
        deleted: 4,
        inserted: String::new(),
    };
    assert_eq!(shift_offset(10, &del), 6);
    // Straddle: delete [8, 14) around offset 10 with 1 byte inserted.
    let straddle = ResolvedSpan {
        at: 8,
        deleted: 6,
        inserted: "Z".into(),
    };
    assert_eq!(shift_offset(10, &straddle), 9);
    // Entirely after: change at the offset itself or beyond.
    let after = ResolvedSpan {
        at: 10,
        deleted: 3,
        inserted: "wxyz".into(),
    };
    assert_eq!(shift_offset(10, &after), 10);
    let insert_at = ResolvedSpan {
        at: 10,
        deleted: 0,
        inserted: "Q".into(),
    };
    // An insert exactly at the offset counts as before (pushes it right).
    assert_eq!(shift_offset(10, &insert_at), 11);
}

/// A sequence of spans replays through both region offsets like the
/// pilot's pump does, keeping start <= anchor.
#[test]
fn shift_offset_sequences_track_a_region() {
    let mut start = 20usize;
    let mut anchor = 30usize;
    let spans = [
        // 5 bytes inserted at 0: both shift right.
        ResolvedSpan {
            at: 0,
            deleted: 0,
            inserted: "aaaaa".into(),
        },
        // Delete [40, 45): after both, no change.
        ResolvedSpan {
            at: 40,
            deleted: 5,
            inserted: String::new(),
        },
        // Delete [23, 28): straddles start (clamps it to the span's new
        // end, 23) and sits entirely before anchor (shifts it left 5).
        ResolvedSpan {
            at: 23,
            deleted: 5,
            inserted: String::new(),
        },
    ];
    for span in &spans {
        start = shift_offset(start, span);
        anchor = shift_offset(anchor, span);
    }
    assert_eq!(start, 23);
    assert_eq!(anchor, 30);
    assert!(start <= anchor);
}

/// The char-coordinate bridge: fence coords convert to byte offsets via
/// collab::byte_offset, including multibyte lines.
#[test]
fn fence_range_converts_to_byte_offsets() {
    let lines: Vec<String> = ["let s = \"héllo\";", "next"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (start, end) = range_bytes(&lines, (0, 10), (1, 2));
    // 'é' is 2 bytes: char col 10 sits after the quote + h + é.
    assert_eq!(start, crate::collab::byte_offset(&lines, 0, 10));
    assert_eq!(end, crate::collab::byte_offset(&lines, 1, 2));
    assert!(start < end);
}

/// The in-process host: a NOTE fence from the scripted claude surfaces
/// as a PairEvent::NoteAdded, the snapshot converts its offset back to a
/// row against the live replica, and the turn's end is observable.
#[test]
fn pair_host_emits_note_added_and_snapshots_rows() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let harness = OwnerHarness::start("hello world\nsecond line");
    let script = harness._dir.path().join("fake_claude.py");
    std::fs::write(&script, FAKE_CLAUDE).unwrap();
    let log = harness._dir.path().join("stdin.log");

    let mut cmd = Command::new("python3");
    cmd.arg(&script).arg(&log).arg("notes");
    let mut host =
        crate::pair_host::PairHost::spawn_cmd(&harness.socket, "navigator", None, cmd).unwrap();
    host.send_task("@demo.txt look this over").unwrap();

    // The reader queues NoteAdded (events channel) before TurnEnd (turn
    // channel), but poll() drains events FIRST: the poll that surfaces
    // TurnDone may have drained events before the note was queued, so
    // wait until BOTH landed; the deadline bounds a real regression.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut events: Vec<crate::pair_host::PairEvent> = Vec::new();
    while !(events
        .iter()
        .any(|e| matches!(e, crate::pair_host::PairEvent::TurnDone { .. }))
        && events
            .iter()
            .any(|e| matches!(e, crate::pair_host::PairEvent::NoteAdded { .. })))
    {
        assert!(
            Instant::now() < deadline,
            "no TurnDone + NoteAdded; saw {events:?}"
        );
        events.extend(host.poll());
        std::thread::sleep(Duration::from_millis(10));
    }
    let note = events
        .iter()
        .find_map(|e| match e {
            crate::pair_host::PairEvent::NoteAdded {
                file, row, body, ..
            } => Some((file.clone(), *row, body.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no NoteAdded; saw {events:?}"));
    assert_eq!(note.0, "demo.txt");
    assert_eq!(note.1, 1, "anchored to the fenced row");
    assert!(note.2.contains("second line could be tighter"));
    let expected = vec![(1, 1, "second line could be tighter".to_string())];
    assert_eq!(
        settle(&expected, || host.notes_snapshot("demo.txt")),
        expected,
        "(id, row, body): the first note of the seat gets id 1"
    );
    drop(host); // must not hang: the shutdown grace-kill path
}

/// A turn cannot be sent while one is still streaming: the host rejects
/// the overlapping turn (before begin_turn runs) so the shared
/// comment-only flag is never clobbered mid-stream. Without the guard, a
/// yield fired during an ask would flip comment_only and the yield's
/// edits could reach the buffer.
#[test]
fn a_turn_is_rejected_while_another_is_streaming() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let harness = OwnerHarness::start("line zero\nline one");
    let script = harness._dir.path().join("fake_claude.py");
    std::fs::write(&script, FAKE_CLAUDE).unwrap();
    let log = harness._dir.path().join("stdin.log");
    let mut cmd = Command::new("python3");
    cmd.arg(&script).arg(&log).arg("hang"); // reads one turn, never answers
    let host = crate::pair_host::PairHost::spawn_cmd(&harness.socket, "nav", None, cmd).unwrap();

    // First turn: an ask (edits allowed). turn_active is set synchronously
    // when the turn is written, so the host is busy immediately.
    host.send_ask_turn("demo.txt", (0, 0), "", "do a thing", "line zero\nline one")
        .unwrap();
    assert!(host.is_busy(), "the ask turn is streaming");

    // A yield fired now must be rejected, and must NOT have flipped the
    // in-flight turn into comment-only (begin_turn never ran).
    let err = host
        .send_yield_turn("demo.txt", "line zero\nline one")
        .unwrap_err();
    assert!(
        err.to_string().contains("mid-turn"),
        "expected a mid-turn rejection, got: {err}"
    );
    assert!(host.is_busy(), "the original ask turn is untouched");
    drop(host);
}

/// Comment boxes need stable identity: the host can inject a note
/// (commentary landing as a box), append the driver's reply to it, and
/// remove exactly one note by id (the box's Ignore button). Ids are
/// distinct and survive removal of a sibling.
#[test]
fn host_adds_appends_and_removes_notes_by_id() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let harness = OwnerHarness::start("hello world\nsecond line");
    let script = harness._dir.path().join("fake_claude.py");
    std::fs::write(&script, FAKE_CLAUDE).unwrap();
    let log = harness._dir.path().join("stdin.log");
    let mut cmd = Command::new("python3");
    cmd.arg(&script).arg(&log).arg("notes");
    let host = crate::pair_host::PairHost::spawn_cmd(&harness.socket, "nav", None, cmd).unwrap();

    let id_a = host
        .add_note("demo.txt", 0, "turn summary")
        .expect("demo.txt is served by the owner");
    let id_b = host.add_note("demo.txt", 1, "second thought").unwrap();
    assert_ne!(id_a, id_b, "every note gets its own id");

    // Each read polls to the resolved value: `notes_snapshot` serves its
    // cache on a contended try_lock, which a loaded runner hits (see
    // `settle`).
    let expected = vec![
        (id_a, 0, "turn summary".to_string()),
        (id_b, 1, "second thought".to_string()),
    ];
    assert_eq!(
        settle(&expected, || host.notes_snapshot("demo.txt")),
        expected
    );

    host.append_to_note(id_a, "you: tell me more");
    let expected = vec![
        (id_a, 0, "turn summary\nyou: tell me more".to_string()),
        (id_b, 1, "second thought".to_string()),
    ];
    assert_eq!(
        settle(&expected, || host.notes_snapshot("demo.txt")),
        expected
    );

    host.remove_note(id_a);
    let expected = vec![(id_b, 1, "second thought".to_string())];
    assert_eq!(
        settle(&expected, || host.notes_snapshot("demo.txt")),
        expected,
        "only the ignored note is gone"
    );
    drop(host);
}

/// Boxes persist until the driver ignores them: a new turn on the same
/// file must NOT wipe its existing notes (the old supersession rule is
/// gone — under the box model it would close every open conversation on
/// each ask or reply).
#[test]
fn notes_survive_a_new_turn_on_the_same_file() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let harness = OwnerHarness::start("hello world\nsecond line");
    let script = harness._dir.path().join("fake_claude.py");
    std::fs::write(&script, FAKE_CLAUDE).unwrap();
    let log = harness._dir.path().join("stdin.log");
    let mut cmd = Command::new("python3");
    cmd.arg(&script).arg(&log).arg("notes");
    let mut host =
        crate::pair_host::PairHost::spawn_cmd(&harness.socket, "nav", None, cmd).unwrap();
    host.send_task("@demo.txt look this over").unwrap();

    let deadline = Instant::now() + Duration::from_secs(60);
    while !host
        .poll()
        .iter()
        .any(|e| matches!(e, crate::pair_host::PairEvent::TurnDone { .. }))
    {
        assert!(Instant::now() < deadline, "no TurnDone");
        std::thread::sleep(Duration::from_millis(10));
    }
    // The reader applies NoteEnd before it sends the TurnEnd, so note
    // state is final here; the snapshot accessor is not exact under
    // contention (see `settle`), so poll it to the resolved value.
    let deadline = Instant::now() + Duration::from_secs(60);
    let before = loop {
        let snap = host.notes_snapshot("demo.txt");
        if snap.len() == 1 {
            break snap;
        }
        assert!(
            Instant::now() < deadline,
            "the scripted note landed: {snap:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };

    // A follow-up turn targeting the same file: begin_turn runs
    // synchronously inside send_yield_turn.
    host.send_yield_turn("demo.txt", "hello world\nsecond line")
        .unwrap();
    assert_eq!(
        settle(&before, || host.notes_snapshot("demo.txt")),
        before,
        "open boxes survive the next turn"
    );
    drop(host);
}

/// The reply composer: the driver answered a note's box. The turn names
/// the note (file, 0-based row, its body), carries the reply, lets the
/// model edit when the reply asks for a change (#1965), and grounds the
/// model with the numbered buffer.
#[test]
fn compose_reply_turn_carries_note_context_and_buffer() {
    let text = compose_reply_turn(
        "demo.txt",
        3,
        "this loop allocates per iteration",
        "is that actually hot?",
        "a\nb",
    );
    assert!(
        text.contains("replied to your note on demo.txt at line 3 (0-based"),
        "names the note's anchor: {text}"
    );
    assert!(
        text.contains("this loop allocates per iteration"),
        "quotes the note body: {text}"
    );
    assert!(
        text.contains("is that actually hot?"),
        "carries the reply: {text}"
    );
    assert!(
        text.contains("If their reply asks you to make a change, make it with EDIT fences"),
        "a reply asking for the change may edit: {text}"
    );
    assert!(
        !text.contains("COMMENT-ONLY") && !text.contains("will be discarded"),
        "a reply is not a comment-only turn: {text}"
    );
    assert!(
        text.contains("0|a\n1|b"),
        "grounds the model with the numbered buffer: {text}"
    );
}

/// Dropping the host (unseat / toggle-off) must not block the UI thread
/// on the 2s grace-kill: teardown runs on a detached thread. The claude
/// CLI lingers on stdin EOF, so the synchronous path would freeze the TUI
/// for the full grace period on every deactivation.
#[test]
fn dropping_the_host_does_not_block_the_ui_thread() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let harness = OwnerHarness::start("line zero\nline one");
    let script = harness._dir.path().join("fake_claude.py");
    std::fs::write(&script, FAKE_CLAUDE).unwrap();
    let log = harness._dir.path().join("stdin.log");
    let mut cmd = Command::new("python3");
    cmd.arg(&script).arg(&log).arg("linger"); // streams a turn, then sleeps 60s
    let mut host =
        crate::pair_host::PairHost::spawn_cmd(&harness.socket, "nav", None, cmd).unwrap();
    // Drive one turn so the child reaches its sleep (ignoring stdin EOF).
    host.send_ask_turn("demo.txt", (0, 0), "", "do a thing", "line zero\nline one")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !host
        .poll()
        .iter()
        .any(|e| matches!(e, crate::pair_host::PairEvent::TurnDone { .. }))
    {
        assert!(Instant::now() < deadline, "turn never finished");
        std::thread::sleep(Duration::from_millis(10));
    }
    let t = Instant::now();
    drop(host);
    assert!(
        t.elapsed() < Duration::from_secs(1),
        "Drop blocked on the grace-kill for {:?}; teardown must be detached",
        t.elapsed()
    );
}

/// claude resolves by bare name when on PATH, but a stripped GUI PATH
/// falls back to probing install dirs and pinning the absolute path so
/// the navigator can still seat on Croft.app launches.
#[test]
fn claude_resolves_to_an_absolute_path_off_a_stripped_path() {
    // On PATH: spawn by bare name (PATH resolves it).
    assert_eq!(
        resolve_claude_in(true, &[]).as_deref(),
        Some("claude"),
        "an on-PATH claude spawns by bare name"
    );
    // Off PATH, not in any probed dir: fall back to the bare name so the
    // spawn error surfaces (caller uses unwrap_or("claude")).
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        resolve_claude_in(false, &[empty.path().to_path_buf()]),
        None
    );
    // Off PATH but present in a probed dir: pin the absolute path.
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join("claude");
    std::fs::write(&claude, "#!/bin/sh\n").unwrap();
    let got = resolve_claude_in(false, &[dir.path().to_path_buf()]).unwrap();
    assert_eq!(got, claude.to_string_lossy());
    assert!(
        Path::new(&got).is_absolute(),
        "GUI launch must get an absolute path, got {got}"
    );
}

/// The system prompt must teach the NOTE fence (with a concrete
/// example), the numbered-buffer convention, and the yield rules the
/// host enforces.
#[test]
fn system_prompt_teaches_note_fence_and_yield_rules() {
    assert!(PAIR_SYSTEM_PROMPT.contains("<<<NOTE <file>:<row>>>>"));
    assert!(PAIR_SYSTEM_PROMPT.contains("<<<NOTE demo.txt:4>>>"));
    assert!(PAIR_SYSTEM_PROMPT.contains("COMMENT-ONLY"));
    assert!(
        PAIR_SYSTEM_PROMPT.contains("- A reply turn answers one of your notes"),
        "the reply turn kind is taught (#1965)"
    );
    assert!(PAIR_SYSTEM_PROMPT.contains("N|"));
    assert!(PAIR_SYSTEM_PROMPT.contains("0-based row"));
}

/// #1926: the navigator's claude loads only the user's own settings, so
/// a workspace's `.claude/settings.json` hooks never run; its built-in
/// tools are exactly Read, Grep and Glob; and those carry no allow rule,
/// so `dontAsk` keeps them inside the workspace.
#[test]
fn the_navigator_claude_cannot_reach_past_the_workspace() {
    let cfg = PairConfig {
        socket: PathBuf::from("/nonexistent/relay.sock"),
        workspace: std::env::temp_dir(),
        name: String::from("claude"),
        model: None,
        task: None,
        provider: Provider::Claude,
    };
    let cmd = claude_command(&cfg).expect("claude command");
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let value_of = |flag: &str| {
        let at = args
            .iter()
            .position(|a| a == flag)
            .unwrap_or_else(|| panic!("{flag} missing from {args:?}"));
        args[at + 1].clone()
    };
    assert_eq!(value_of("--setting-sources"), "user");
    assert_eq!(value_of("--tools"), "Read,Grep,Glob");
    assert_eq!(value_of("--permission-mode"), "dontAsk");
    let allowed: Vec<String> = value_of("--allowedTools")
        .split(',')
        .map(|t| t.trim().to_string())
        .collect();
    assert!(
        allowed.iter().all(|t| t.starts_with("mcp__croft-collab__")),
        "a bare built-in allow rule matches every path: {allowed:?}"
    );
    assert!(args.iter().any(|a| a == "--strict-mcp-config"));
}

/// The ask-turn composer: instruction, invoked range, selected text,
/// and the numbered buffer all ride the turn.
#[test]
fn compose_ask_turn_numbers_content_and_includes_selection() {
    let text = compose_ask_turn(
        "src/a.rs",
        (3, 5),
        "let x = 1;\nlet y = 2;",
        "simplify these",
        "l0\nl1\nl2\nlet x = 1;\nmid\nlet y = 2;",
    );
    assert!(text.contains("simplify these"));
    assert!(text.contains("lines 3-5"));
    assert!(text.contains("0-based"));
    assert!(text.contains("--- SELECTED LINES ---"));
    assert!(text.contains("let y = 2;"));
    assert!(text.contains("0|l0"));
    assert!(text.contains("3|let x = 1;"));

    // Single line, no selection: the range names one line and the
    // selected block is omitted.
    let one = compose_ask_turn("src/a.rs", (2, 2), "", "why is this here", "a\nb\nc");
    assert!(one.contains("line 2"));
    assert!(!one.contains("--- SELECTED LINES ---"));
    assert!(one.contains("2|c"));
}

/// #1966: the gutter numbers lines from 1, so every line a turn names
/// carries that number beside its 0-based row, and the bare 0-based row
/// never stands alone where the model would quote it to its partner.
#[test]
fn turns_name_the_gutter_line_beside_the_row() {
    let one = compose_ask_turn("src/a.rs", (4, 4), "", "fix this", "a\nb\nc\nd\ne");
    assert!(
        one.contains("at line 4 (0-based; line 5 in your partner's gutter)"),
        "{one}"
    );
    assert!(
        !one.contains("line 4 (0-based)."),
        "bare 0-based row: {one}"
    );

    let range = compose_ask_turn("src/a.rs", (0, 2), "a\nb\nc", "tidy", "a\nb\nc");
    assert!(
        range.contains("at lines 0-2 (0-based; lines 1-3 in your partner's gutter)"),
        "{range}"
    );
    assert!(
        !range.contains("lines 0-2 (0-based)."),
        "bare 0-based rows: {range}"
    );

    let reply = compose_reply_turn("main.py", 4, "a note", "correct this", "a\nb\nc\nd\ne");
    assert!(
        reply.contains("at line 4 (0-based; line 5 in your partner's gutter)"),
        "{reply}"
    );
    assert!(
        !reply.contains("line 4 (0-based)."),
        "bare 0-based row: {reply}"
    );
    assert!(
        reply.contains("a NOTE re-anchored to row 4 continues this box"),
        "the re-anchor target is a row: {reply}"
    );
    assert!(
        !reply.contains("re-anchoring to line 4"),
        "the re-anchor row called a line: {reply}"
    );

    // The prose rule the model follows when it writes a line number.
    assert!(
        PAIR_SYSTEM_PROMPT.contains("row N is line N+1"),
        "the system prompt teaches gutter numbers for prose"
    );
}

/// #1966: every numbered "line" the prompt shows is a gutter line. Fence
/// coordinates are rows, so an example such as "pins a remark to line 4"
/// for `<<<NOTE demo.txt:4>>>` cannot teach the model to quote the
/// 0-based row to its partner. The local prompt carries the shared one.
#[test]
fn system_prompt_numbers_lines_only_by_the_gutter() {
    let prompt = system_prompt(&Provider::Local {
        base_url: String::new(),
    });
    for text in prompt.lines() {
        for (at, _) in text.match_indices("line") {
            let after = &text[at + "line".len()..];
            let after = after.strip_prefix('s').unwrap_or(after);
            let numbered = after
                .strip_prefix(' ')
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
            if numbered {
                assert!(
                    text[..at].ends_with("gutter "),
                    "a 0-based row called a line: {text}"
                );
            }
        }
    }
    // The examples still pair each row with its gutter line.
    assert!(prompt.contains("row 0 of demo.txt, your partner's gutter line 1"));
    assert!(prompt.contains("rows 3-4 of src/lib.rs, gutter lines 4-5"));
    assert!(prompt.contains("row 4 of demo.txt, gutter line 5"));
    assert!(prompt.contains("row 1 (gutter line 2)"));
}

/// The yield-turn composer: comment-only rule, the diff since the
/// navigator last saw the file, and the numbered buffer.
#[test]
fn compose_yield_turn_carries_diff_and_forbids_edits() {
    let with_diff = compose_yield_turn(
        "src/a.rs",
        "line one\nline 2\n",
        Some("@@ -1,2 +1,2 @@\n line one\n-line two\n+line 2\n"),
    );
    assert!(with_diff.contains("COMMENT-ONLY"));
    assert!(with_diff.contains("--- CHANGES SINCE YOUR LAST LOOK ---"));
    assert!(with_diff.contains("+line 2"));
    assert!(with_diff.contains("0|line one"));

    let first_look = compose_yield_turn("src/a.rs", "just this\n", None);
    assert!(first_look.contains("COMMENT-ONLY"));
    assert!(!first_look.contains("--- CHANGES SINCE YOUR LAST LOOK ---"));
}

/// Host-enforced comment-only: the scripted claude streams an EDIT fence
/// during a yielded turn; the owner's buffer must stay byte-identical
/// and the suppression must be spoken.
#[test]
fn yield_turn_suppresses_edits() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let harness = OwnerHarness::start("hello world");
    let script = harness._dir.path().join("fake_claude.py");
    std::fs::write(&script, FAKE_CLAUDE).unwrap();
    let log = harness._dir.path().join("stdin.log");

    let mut cmd = Command::new("python3");
    cmd.arg(&script).arg(&log).arg("stream"); // streams an EDIT fence
    let mut host =
        crate::pair_host::PairHost::spawn_cmd(&harness.socket, "navigator", None, cmd).unwrap();
    host.send_yield_turn("demo.txt", "hello world").unwrap();

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut events: Vec<crate::pair_host::PairEvent> = Vec::new();
    while !events
        .iter()
        .any(|e| matches!(e, crate::pair_host::PairEvent::TurnDone { .. }))
    {
        assert!(Instant::now() < deadline, "no TurnDone; saw {events:?}");
        events.extend(host.poll());
        std::thread::sleep(Duration::from_millis(10));
    }
    // The owner never saw an edit land.
    assert_eq!(harness.remote_edit_count(), 0, "edit must be suppressed");
    // Diagnostics NEVER ride Commentary: the App anchors that channel
    // as the navigator's own comment boxes, so the suppression notice
    // (and claude's stderr chatter) would be rendered as the model's
    // remark in the user's file and counted as a comment.
    assert!(
        !events.iter().any(|e| matches!(
            e,
            crate::pair_host::PairEvent::Commentary(c)
                if c.contains("suppressed") || c.contains("[claude]")
        )),
        "diagnostics must not become comment boxes; saw {events:?}"
    );
    // Whole-snapshot search, no baseline index: OUTPUT is a shared
    // evicting ring, so a length captured earlier can drift under
    // concurrent tests and a skip() would step over the lines.
    let wait_output = |needle: &str| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let lines = crate::output::snapshot("Navigator").unwrap_or_default();
            if lines.iter().any(|l| l.text.contains(needle)) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "OUTPUT never carried {needle:?}: {lines:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    wait_output("suppressed");
    wait_output("[claude] mcp server starting");
    drop(host);
}

/// Poll `host` until a turn ends, returning every event seen on the way.
fn wait_turn_done(host: &mut crate::pair_host::PairHost) -> Vec<crate::pair_host::PairEvent> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut events = Vec::new();
    while !events
        .iter()
        .any(|e| matches!(e, crate::pair_host::PairEvent::TurnDone { .. }))
    {
        assert!(Instant::now() < deadline, "no TurnDone; saw {events:?}");
        events.extend(host.poll());
        std::thread::sleep(Duration::from_millis(10));
    }
    events
}

/// #1965: a reply in a comment box is the driver's own words, so it is
/// an invited turn like an ask. "correct this" under a note proposing a
/// fix must be able to land that fix in the buffer.
#[test]
fn a_reply_turn_can_edit_the_buffer() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let harness = OwnerHarness::start("hello world");
    let script = harness._dir.path().join("fake_claude.py");
    std::fs::write(&script, FAKE_CLAUDE).unwrap();
    let log = harness._dir.path().join("stdin.log");

    let mut cmd = Command::new("python3");
    cmd.arg(&script).arg(&log).arg("stream"); // streams an EDIT fence
    let mut host =
        crate::pair_host::PairHost::spawn_cmd(&harness.socket, "navigator", None, cmd).unwrap();
    host.send_reply_turn("demo.txt", 0, "rename world", "correct this", "hello world")
        .unwrap();
    wait_turn_done(&mut host);
    harness.wait_until("the reply's edit on the owner", |h| {
        h.doc().as_deref() == Some("hello streamed edit")
    });
    drop(host);
}

/// #1965, the gate that must survive it: an edit-capable reply must not
/// leave the next yielded turn able to edit. One seated model edits on
/// every turn: reply 1 lands, yield 2 is discarded, reply 3 lands.
#[test]
fn a_yield_after_a_reply_still_discards_edits() {
    if !is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let harness = OwnerHarness::start("hello world");
    let script = harness._dir.path().join("fake_claude.py");
    std::fs::write(&script, FAKE_CLAUDE).unwrap();
    let log = harness._dir.path().join("stdin.log");

    let mut cmd = Command::new("python3");
    cmd.arg(&script).arg(&log).arg("repeat"); // inserts the turn number
    let mut host =
        crate::pair_host::PairHost::spawn_cmd(&harness.socket, "navigator", None, cmd).unwrap();
    host.send_reply_turn("demo.txt", 0, "a note", "correct this", "hello world")
        .unwrap();
    wait_turn_done(&mut host);
    harness.wait_until("reply 1's edit on the owner", |h| {
        h.doc().as_deref() == Some("1hello world")
    });

    host.send_yield_turn("demo.txt", "1hello world").unwrap();
    wait_turn_done(&mut host);
    // Presence: the yield reached the model, which streamed edit 2.
    let seen = std::fs::read_to_string(&log).unwrap();
    assert!(
        seen.contains("yielded the turn"),
        "the yield turn never reached the model: {seen}"
    );

    // Barrier: one pilot's edits reach the owner in order, so once
    // edit 3 has landed, a leaked edit 2 would already be in the doc.
    host.send_reply_turn("demo.txt", 0, "a note", "and again", "1hello world")
        .unwrap();
    wait_turn_done(&mut host);
    harness.wait_until("reply 3's edit on the owner", |h| {
        h.doc().is_some_and(|d| d.starts_with('3'))
    });
    assert_eq!(
        harness.doc().as_deref(),
        Some("31hello world"),
        "the yield's edit must be suppressed"
    );
    drop(host);
}

/// Remote-edit spans shift every note anchored in the edited file, and
/// only that file (the same replay the stream region does).
#[test]
fn note_offsets_shift_on_remote_edit() {
    let mut notes = vec![
        Note {
            id: 1,
            file: "a.rs".into(),
            offset: 10,
            body: "n1".into(),
        },
        Note {
            id: 2,
            file: "b.rs".into(),
            offset: 10,
            body: "n2".into(),
        },
    ];
    let spans = [
        // 3 bytes inserted at 0: before the offset, shifts it right.
        ResolvedSpan {
            at: 0,
            deleted: 0,
            inserted: "abc".into(),
        },
        // Delete far after: no effect.
        ResolvedSpan {
            at: 40,
            deleted: 2,
            inserted: String::new(),
        },
    ];
    shift_notes(&mut notes, "a.rs", &spans);
    assert_eq!(notes[0].offset, 13);
    assert_eq!(notes[1].offset, 10, "other file's note must not move");
}

/// Aborting a streamed edit must also un-shift the notes that edit
/// moved: open_region shifts notes for its delete and every EditBody
/// shifts them for its insert, so the revert has to apply the inverse
/// span. Without it, every cancel or abort left the turn's notes
/// permanently drifted from the rows the model anchored them to.
#[test]
fn an_aborted_edit_un_shifts_the_notes_it_moved() {
    let harness = OwnerHarness::start("hello world");
    let (state, stop, pump) = pumped_state(&harness);
    {
        let mut st = state.lock().unwrap();
        st.begin_turn("demo.txt", "hello world", false);
        // Anchors below, AT, and INSIDE the edited span [0, 5): the
        // below one is shifted and must shift back; the at/inside ones
        // are collapsed by the delete and ride the streamed inserts,
        // so only their recorded pre-edit offsets can restore them.
        for offset in [8usize, 0, 2] {
            let id = st.take_note_id();
            st.notes.push(Note {
                id,
                file: String::from("demo.txt"),
                offset,
                body: String::from("remark"),
            });
        }
    }
    apply_fence_event(
        &state,
        FenceEvent::EditStart {
            file: String::from("demo.txt"),
            start: (0, 0),
            end: (0, 5),
        },
    );
    apply_fence_event(
        &state,
        FenceEvent::EditBody(String::from("hi there friend")),
    );
    apply_fence_event(&state, FenceEvent::EditAbort);
    let offsets: Vec<usize> = state
        .lock()
        .unwrap()
        .notes
        .iter()
        .map(|n| n.offset)
        .collect();
    assert_eq!(
        offsets,
        vec![8, 0, 2],
        "the revert must restore every note anchor the edit had moved"
    );
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();
}

/// A note row just past EOF clamps to the last line instead of panicking
/// or anchoring nowhere.
#[test]
fn note_anchor_row_clamps_past_eof() {
    let lines: Vec<String> = ["ab", "cd"].iter().map(|s| s.to_string()).collect();
    assert_eq!(note_offset(&lines, 0), 0);
    assert_eq!(note_offset(&lines, 1), 3);
    assert_eq!(note_offset(&lines, 99), 3);
}

/// Every composed turn names its target file, even when the buffer never
/// went live (field bug 2026-07-15: parse_task_line had stripped the
/// @file prefix, so a dead bootstrap left claude with no file at all).
#[test]
fn compose_turn_always_names_target_file() {
    let live = compose_turn_text("fix the error", Some("src/a.rs"), Some("body"));
    assert!(live.contains("--- CURRENT BUFFER (src/a.rs) ---"));
    assert!(live.contains("body"));

    let dead = compose_turn_text("fix the error", Some("src/a.rs"), None);
    assert!(
        dead.contains("src/a.rs"),
        "file must be named without a live buffer: {dead}"
    );
    assert!(dead.contains("fix the error"));

    let noted = with_pending_note(Some("stream was cancelled".into()), "task".into());
    assert!(noted.starts_with("stream was cancelled"));
    assert!(noted.contains("task"));
}
