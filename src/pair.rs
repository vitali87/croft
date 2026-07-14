//! `croft pair`: the AI pilot that streams a Claude conversation's tokens
//! straight into the collab seat, so a co-editing session sees the edit
//! arrive token by token instead of as one bulk insert (docs/MULTIPLAYER.md,
//! "croft pair").
//!
//! This module's pure core is the fence machine: the model is taught (via
//! the pair system prompt) to wrap edits in a fenced protocol inside its
//! streamed TEXT output:
//!
//! ```text
//! <<<EDIT file:START_ROW:START_COL-END_ROW:END_COL>>>
//! <replacement text>
//! <<<END>>>
//! ```
//!
//! Coordinates are 0-based CHARACTER positions against the buffer text the
//! pilot injected into the turn; byte offsets only ever come from
//! [`crate::collab::byte_offset`] (never treat a column as bytes). The
//! machine is fed raw `text_delta` fragments split at arbitrary points and
//! emits events; everything outside a well-formed fence is commentary and is
//! never applied to a buffer.

use std::io::{BufRead, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::collab::{
    CollabChannel, CollabEvent, CollabRole, CollabSession, ResolvedSpan, position,
};

/// Start of an edit-fence header line.
const EDIT_MARKER: &str = "<<<EDIT ";
/// The whole terminator line.
const END_MARKER: &str = "<<<END>>>";

/// What the fence machine resolved from a run of streamed text deltas, in
/// stream order. Only text between a well-formed `EditStart` and its
/// `EditEnd` ever touches a buffer.
#[derive(Debug)]
pub enum FenceEvent {
    /// Model prose outside any fence (including malformed fences): printed
    /// to the pilot's terminal, never applied.
    Commentary(String),
    /// A well-formed header opened an edit: replace the 0-based char range
    /// `start..end` of `file` with the body that streams next.
    EditStart {
        file: String,
        start: (usize, usize),
        end: (usize, usize),
    },
    /// The next fragment of the current fence's body, in stream order.
    EditBody(String),
    /// The fence closed cleanly; the streamed body is the whole replacement.
    EditEnd,
    /// The turn ended mid-fence (risk R2): the pilot reverts whatever body
    /// already streamed in.
    EditAbort,
}

/// Where the machine is between pushes.
enum FenceState {
    /// Outside a fence: complete lines classify as header or commentary.
    Outside,
    /// Inside a fence body. `held_newline`: a body '\n' was consumed but not
    /// emitted (it may turn out to be the newline that separates the body
    /// from `<<<END>>>`, which is stripped). `at_line_start`: the unprocessed
    /// tail starts a fresh line, so it could still become the END marker.
    Body {
        held_newline: bool,
        at_line_start: bool,
    },
}

/// Incremental parser for the fenced edit protocol. Fed `text_delta`
/// fragments split at arbitrary points; emits [`FenceEvent`]s as soon as
/// they are unambiguous, so a fence body streams token by token (the whole
/// point of `croft pair`). Call [`finish`](Self::finish) at end of turn.
pub struct FenceMachine {
    buf: String,
    state: FenceState,
}

impl FenceMachine {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            state: FenceState::Outside,
        }
    }

    /// Feed one streamed fragment; returns every event it completed.
    pub fn push(&mut self, delta: &str) -> Vec<FenceEvent> {
        self.buf.push_str(delta);
        let mut events = Vec::new();
        loop {
            match self.state {
                FenceState::Outside => {
                    let Some(nl) = self.buf.find('\n') else {
                        break;
                    };
                    let line: String = self.buf.drain(..=nl).collect();
                    let trimmed = &line[..line.len() - 1];
                    if let Some(start) = parse_header(trimmed) {
                        events.push(start);
                        self.state = FenceState::Body {
                            held_newline: false,
                            at_line_start: true,
                        };
                    } else {
                        events.push(FenceEvent::Commentary(line));
                    }
                }
                FenceState::Body {
                    ref mut held_newline,
                    ref mut at_line_start,
                } => {
                    if *at_line_start {
                        match self.buf.find('\n') {
                            Some(nl) => {
                                if &self.buf[..nl] == END_MARKER {
                                    // The held newline separated body from
                                    // the marker: stripped, not body.
                                    self.buf.drain(..=nl);
                                    events.push(FenceEvent::EditEnd);
                                    self.state = FenceState::Outside;
                                } else {
                                    // A complete body line: the held newline
                                    // is confirmed body, the line's own
                                    // newline is held in its place.
                                    let mut body = String::new();
                                    if *held_newline {
                                        body.push('\n');
                                    }
                                    body.push_str(&self.buf[..nl]);
                                    self.buf.drain(..=nl);
                                    events.push(FenceEvent::EditBody(body));
                                    *held_newline = true;
                                }
                            }
                            None => {
                                if END_MARKER.starts_with(self.buf.as_str()) {
                                    // Could still become the terminator (or
                                    // is it, pending its newline): wait.
                                    break;
                                }
                                // Provably not the marker: stream it now.
                                let mut body = String::new();
                                if *held_newline {
                                    body.push('\n');
                                    *held_newline = false;
                                }
                                body.push_str(&self.buf);
                                self.buf.clear();
                                events.push(FenceEvent::EditBody(body));
                                *at_line_start = false;
                                break;
                            }
                        }
                    } else {
                        match self.buf.find('\n') {
                            Some(nl) => {
                                if nl > 0 {
                                    events.push(FenceEvent::EditBody(self.buf[..nl].to_string()));
                                }
                                self.buf.drain(..=nl);
                                *held_newline = true;
                                *at_line_start = true;
                            }
                            None => {
                                if !self.buf.is_empty() {
                                    events
                                        .push(FenceEvent::EditBody(std::mem::take(&mut self.buf)));
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }
        events
    }

    /// End of turn: flush leftovers. Outside a fence the tail is commentary;
    /// a bare END marker with no trailing newline still closes the fence;
    /// anything else mid-fence aborts it (the pilot reverts). The machine is
    /// reset for the next turn either way.
    pub fn finish(&mut self) -> Vec<FenceEvent> {
        let mut events = Vec::new();
        match self.state {
            FenceState::Outside => {
                if !self.buf.is_empty() {
                    events.push(FenceEvent::Commentary(std::mem::take(&mut self.buf)));
                }
            }
            FenceState::Body { at_line_start, .. } => {
                if at_line_start && self.buf == END_MARKER {
                    events.push(FenceEvent::EditEnd);
                } else {
                    events.push(FenceEvent::EditAbort);
                }
                self.buf.clear();
            }
        }
        self.state = FenceState::Outside;
        events
    }
}

/// Parse `<<<EDIT <file>:SR:SC-ER:EC>>>` into its [`FenceEvent::EditStart`].
/// Coordinates bind rightmost so the file name may itself contain ':' or
/// '-'. None = not a well-formed header (the line degrades to commentary).
fn parse_header(line: &str) -> Option<FenceEvent> {
    let inner = line.strip_prefix(EDIT_MARKER)?.strip_suffix(">>>")?;
    let (rest, ec) = inner.rsplit_once(':')?;
    let (rest, mid) = rest.rsplit_once(':')?;
    let (sc, er) = mid.split_once('-')?;
    let (file, sr) = rest.rsplit_once(':')?;
    if file.is_empty() {
        return None;
    }
    Some(FenceEvent::EditStart {
        file: file.to_string(),
        start: (sr.parse().ok()?, sc.parse().ok()?),
        end: (er.parse().ok()?, ec.parse().ok()?),
    })
}

/// A fence range as byte offsets into the joined ('\n'-separated) text of
/// `lines`, via the shared char-coordinate bridge (risk R6: columns are
/// chars, offsets are bytes; this is the only conversion path).
pub fn range_bytes(lines: &[String], start: (usize, usize), end: (usize, usize)) -> (usize, usize) {
    (
        crate::collab::byte_offset(lines, start.0, start.1),
        crate::collab::byte_offset(lines, end.0, end.1),
    )
}

/// Transform a tracked byte offset through one remote edit span, the same
/// sequential replay the buffers do: a span entirely before the offset
/// shifts it by the size delta (an insert exactly at the offset counts as
/// before, pushing it right); a span straddling it clamps to the span's new
/// end; a span after it leaves it alone.
pub fn shift_offset(x: usize, span: &ResolvedSpan) -> usize {
    if span.at + span.deleted <= x {
        // No underflow: span.deleted <= x on this branch.
        x + span.inserted.len() - span.deleted
    } else if span.at < x {
        span.at + span.inserted.len()
    } else {
        x
    }
}

// ---------------------------------------------------------------------------
// Pilot runtime: spawn the claude CLI in stream-json mode, parse its token
// stream through the fence machine, and apply fences live through a collab
// guest seat. Cancel rides the relay (CollabMsg::StreamCancel) and lands as
// a control_request interrupt on claude's stdin.
// ---------------------------------------------------------------------------

/// The read-only toolbox the pilot's claude may use (approved product
/// decision: read-only plus the stream; the ONLY write path is the fence).
/// The `croft-collab` MCP server here is a second, read-only seat riding
/// along for live buffer queries; the pilot itself owns the writing seat.
const ALLOWED_TOOLS: &str = "mcp__croft-collab__collab_open,mcp__croft-collab__collab_read,\
     mcp__croft-collab__collab_status,Read,Grep,Glob";

/// How long an EditStart waits for its file to bootstrap, a hair past the
/// session's own 3s deadline (same reasoning as the collab agent's).
const LIVE_TIMEOUT: Duration = Duration::from_secs(4);

/// Taught to the model via --append-system-prompt: the fence protocol whose
/// streamed body croft applies token by token.
const PAIR_SYSTEM_PROMPT: &str = r#"You are pair-programming INSIDE the croft editor: your streamed text is parsed live and fenced edits are applied to the shared buffer token by token, so your human partner watches the edit appear as you write it.

To edit a file, emit an edit fence directly in your text output:

<<<EDIT <file>:<start_row>:<start_col>-<end_row>:<end_col>>>>
<replacement text>
<<<END>>>

Rules:
- <file> is the workspace-relative path. Coordinates are 0-based CHARACTER positions (not bytes) into the file's CURRENT text: the range start..end (start inclusive, end exclusive) is deleted and the fence body replaces it. To insert without deleting, use a zero-width range (start == end).
- Compute coordinates against the buffer text in the latest user message (the --- CURRENT BUFFER --- block) or from the mcp__croft-collab__collab_read tool. If you emit several fences in one reply, later fences must use coordinates that account for your earlier fences' changes.
- The header and <<<END>>> each sit alone on their own line. The body between them is applied verbatim: real code only, no markdown fences, no commentary.
- Everything outside a fence is commentary shown to your partner in the terminal; it is never applied to any buffer.
- Start the fence as soon as you know the edit; stream the body naturally rather than planning silently.

Example (replace lines 3-4 of src/lib.rs, where line 4 is 12 chars long):
<<<EDIT src/lib.rs:3:0-4:12>>>
fn renamed() -> u32 {
    41 + 1
}
<<<END>>>

A participant can cancel your stream mid-edit; the streamed text is then reverted and your next user message starts with a note saying so. When that happens, stop that approach and ask what they want instead."#;

/// Everything `croft pair` needs to sit down: the relay socket, the
/// workspace (cwd and MCP reader seat), the caret name, and the claude
/// launch knobs.
pub struct PairConfig {
    pub socket: PathBuf,
    pub workspace: PathBuf,
    pub name: String,
    pub model: Option<String>,
    pub task: Option<String>,
}

/// The fence currently streaming into a buffer: byte offsets into the
/// replica text (kept fresh against concurrent remote edits by
/// [`shift_offset`]) plus the slice the fence's range deleted, for revert.
struct StreamRegion {
    file: String,
    start: usize,
    anchor: usize,
    original: String,
}

/// Shared pilot state: the collab seat plus per-turn stream bookkeeping.
/// Locked briefly by the reader thread (apply), the pump thread (remote
/// shifts, cancel), and the REPL (turn injection); never held across a
/// sleep or a child-process write.
struct PairState {
    session: CollabSession,
    region: Option<StreamRegion>,
    /// The current fence's body is being dropped (unusable header, missing
    /// owner, or a post-cancel fence).
    discarding: bool,
    /// A cancel landed this turn: every further fence event is ignored
    /// until the turn's result, and the next turn opens with a note.
    cancelled: bool,
    /// claude's init advertised interrupt_receipt_v1.
    can_interrupt: bool,
    /// Between sending a user message and seeing its result.
    turn_active: bool,
    /// Whose buffer rides in the next turn's injection.
    target_file: Option<String>,
    /// Prepended to the next user turn (set by cancel).
    pending_note: Option<String>,
}

/// Kill the claude child when the pilot unwinds, however it unwinds.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// What the reader thread reports at each turn's `result` event.
struct TurnEnd {
    is_error: bool,
    text: String,
}

/// The claude CLI invocation for a pair session: a persistent stream-json
/// conversation over stdio, sandboxed to the read-only toolbox, with the
/// fence protocol appended to its system prompt and a read-only collab-agent
/// seat as its MCP server.
fn claude_command(cfg: &PairConfig) -> Result<Command> {
    let exe = std::env::current_exe().context("resolving croft binary path")?;
    let mcp_config = json!({
        "mcpServers": {
            "croft-collab": {
                "command": exe.to_string_lossy(),
                "args": [
                    "collab-agent",
                    "--workspace", cfg.workspace.to_string_lossy(),
                    "--name", format!("{}-reader", cfg.name),
                ],
            }
        }
    });
    let mut cmd = Command::new("claude");
    cmd.arg("-p")
        .args(["--input-format", "stream-json"])
        .args(["--output-format", "stream-json"])
        .arg("--verbose")
        .arg("--include-partial-messages")
        .args(["--permission-mode", "dontAsk"])
        .args(["--allowedTools", ALLOWED_TOOLS])
        .arg("--strict-mcp-config")
        .args(["--mcp-config", &mcp_config.to_string()])
        .args(["--append-system-prompt", PAIR_SYSTEM_PROMPT])
        .current_dir(&cfg.workspace);
    if let Some(model) = &cfg.model {
        cmd.args(["--model", model]);
    }
    Ok(cmd)
}

/// `croft pair`: join the workspace's collab relay as the pilot seat, spawn
/// claude, and run the REPL until stdin closes.
pub fn run(cfg: PairConfig) -> Result<()> {
    let cmd = claude_command(&cfg)?;
    let stdin = std::io::stdin();
    run_pilot(&cfg.socket, &cfg.name, cfg.task, cmd, &mut stdin.lock())
}

/// Connect the pilot's collab seat, retrying briefly (the dispatch just
/// ensured the relay, but its accept loop may still be coming up).
fn connect_session(socket: &Path, name: &str) -> Result<CollabSession> {
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

/// The pilot proper, claude-command-agnostic so the e2e tests can drive it
/// with a scripted fake. Blocks until `input` (the pilot's own terminal)
/// hits EOF or the claude child's stdout closes mid-turn.
fn run_pilot(
    socket: &Path,
    name: &str,
    task: Option<String>,
    mut cmd: Command,
    input: &mut dyn BufRead,
) -> Result<()> {
    let session = connect_session(socket, name)?;
    let state = Arc::new(Mutex::new(PairState {
        session,
        region: None,
        discarding: false,
        cancelled: false,
        can_interrupt: false,
        turn_active: false,
        target_file: None,
        pending_note: None,
    }));

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
    let _guard = ChildGuard(child);
    let writer: Arc<Mutex<Option<ChildStdin>>> = Arc::new(Mutex::new(Some(child_stdin)));

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
    // pilot's terminal without touching the protocol stream.
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(child_stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => eprint!("[claude] {line}"),
            }
        }
    });

    // Pump: remote spans shift the streamed region; StreamCancel cancels.
    let pump = {
        let state = Arc::clone(&state);
        let writer = Arc::clone(&writer);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let req_id = AtomicU64::new(0);
            while !stop.load(Ordering::Relaxed) {
                pump_session(&state, &writer, &req_id);
                std::thread::sleep(Duration::from_millis(25));
            }
        })
    };

    // REPL: the initial --task then the pilot's stdin, one turn per line.
    println!("croft pair: '{name}' seated; type a task ('@<file> <task>' to focus a buffer)");
    let mut pending = task;
    loop {
        let line = match pending.take() {
            Some(t) => t,
            None => {
                let mut line = String::new();
                match input.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => line.trim().to_string(),
                }
            }
        };
        if line.is_empty() {
            continue;
        }
        send_turn(&state, &writer, &line)?;
        match turn_rx.recv() {
            Ok(end) if end.is_error => println!("\n[turn failed: {}]", end.text),
            Ok(_) => println!("\n[turn done]"),
            Err(_) => {
                // Reader gone: claude hung up mid-turn.
                anyhow::bail!("claude exited mid-conversation");
            }
        }
    }

    // Cleanup: leave no stream badge behind, hang up, kill via the guard.
    {
        let mut st = state.lock().unwrap();
        revert_region(&mut st);
    }
    stop.store(true, Ordering::Relaxed);
    writer.lock().unwrap().take(); // EOF ends claude's conversation
    let _ = pump.join();
    let _ = reader.join();
    Ok(())
}

/// One inbound claude event: init capabilities, token deltas through the
/// fence machine, and each turn's result.
fn handle_claude_event(
    state: &Mutex<PairState>,
    fence: &mut FenceMachine,
    msg: &Value,
    turn_tx: &Sender<TurnEnd>,
) {
    match msg.get("type").and_then(Value::as_str) {
        Some("system") if msg.get("subtype").and_then(Value::as_str) == Some("init") => {
            let can = msg
                .get("capabilities")
                .and_then(Value::as_array)
                .is_some_and(|caps| {
                    caps.iter()
                        .any(|c| c.as_str() == Some("interrupt_receipt_v1"))
                });
            state.lock().unwrap().can_interrupt = can;
        }
        Some("stream_event") => {
            let delta = msg.pointer("/event/delta");
            if delta.and_then(|d| d.get("type")).and_then(Value::as_str) == Some("text_delta")
                && let Some(text) = delta.and_then(|d| d.get("text")).and_then(Value::as_str)
            {
                for event in fence.push(text) {
                    apply_fence_event(state, event);
                }
            }
        }
        Some("result") => {
            for event in fence.finish() {
                apply_fence_event(state, event);
            }
            let mut st = state.lock().unwrap();
            st.turn_active = false;
            st.cancelled = false;
            st.discarding = false;
            drop(st);
            let _ = turn_tx.send(TurnEnd {
                is_error: msg
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                text: msg
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        _ => {} // hook noise, control_response acks, message_start/stop, ...
    }
}

/// Apply one fence event to the collab seat. Commentary prints; edits apply
/// at the tracked anchor; abort reverts. Never holds the state lock across
/// the EditStart bootstrap wait.
fn apply_fence_event(state: &Mutex<PairState>, event: FenceEvent) {
    match event {
        FenceEvent::Commentary(text) => {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        FenceEvent::EditStart { file, start, end } => {
            {
                let mut st = state.lock().unwrap();
                if st.cancelled {
                    st.discarding = true;
                    return;
                }
                st.target_file = Some(file.clone());
                st.session.request_file(&file);
            }
            let deadline = Instant::now() + LIVE_TIMEOUT;
            loop {
                let mut st = state.lock().unwrap();
                if st.session.is_live(&file) {
                    open_region(&mut st, &file, start, end);
                    return;
                }
                if !st.session.is_bootstrapping(&file) || Instant::now() >= deadline {
                    st.discarding = true;
                    drop(st);
                    eprintln!(
                        "[pair] no live croft session serves {file}; edit dropped \
                         (start croft in this workspace first)"
                    );
                    return;
                }
                drop(st);
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        FenceEvent::EditBody(delta) => {
            let mut st = state.lock().unwrap();
            if st.cancelled || st.discarding {
                return;
            }
            let Some((file, anchor)) = st.region.as_ref().map(|r| (r.file.clone(), r.anchor))
            else {
                return;
            };
            let Some(doc) = st.session.doc_text(&file).map(str::to_string) else {
                return;
            };
            let anchor = anchor.min(doc.len());
            let new = format!("{}{}{}", &doc[..anchor], delta, &doc[anchor..]);
            st.session.local_change(&file, &new);
            let next = anchor + delta.len();
            if let Some(r) = st.region.as_mut() {
                r.anchor = next;
            }
            let lines: Vec<String> = new.split('\n').map(String::from).collect();
            let (row, col) = position(&lines, next);
            st.session.send_caret(&file, row, col);
        }
        FenceEvent::EditEnd => {
            let mut st = state.lock().unwrap();
            st.discarding = false;
            if st.cancelled {
                return; // already reverted by the cancel
            }
            if let Some(r) = st.region.take() {
                st.session.send_stream_state(&r.file, false);
            }
        }
        FenceEvent::EditAbort => {
            let mut st = state.lock().unwrap();
            st.discarding = false;
            if st.cancelled {
                st.region = None;
                return;
            }
            revert_region(&mut st);
        }
    }
}

/// EditStart's second half, once the file is live: delete the fence's range
/// in one replica change and open the streamed region at its start.
fn open_region(st: &mut PairState, file: &str, start: (usize, usize), end: (usize, usize)) {
    let Some(doc) = st.session.doc_text(file).map(str::to_string) else {
        st.discarding = true;
        return;
    };
    let lines: Vec<String> = doc.split('\n').map(String::from).collect();
    let (s, e) = range_bytes(&lines, start, end);
    let (s, e) = (s.min(doc.len()), e.min(doc.len()));
    if s > e {
        st.discarding = true;
        eprintln!("[pair] fence range is inverted; edit dropped");
        return;
    }
    let original = doc[s..e].to_string();
    let new = format!("{}{}", &doc[..s], &doc[e..]);
    st.session.local_change(file, &new);
    st.region = Some(StreamRegion {
        file: file.to_string(),
        start: s,
        anchor: s,
        original,
    });
    st.discarding = false;
    st.session.send_stream_state(file, true);
    let lines: Vec<String> = new.split('\n').map(String::from).collect();
    let (row, col) = position(&lines, s);
    st.session.send_caret(file, row, col);
}

/// Put the streamed region's original slice back (cancel, abort, or exit)
/// and broadcast the stream as inactive. No-op without an open region.
fn revert_region(st: &mut PairState) {
    let Some(r) = st.region.take() else {
        return;
    };
    if let Some(doc) = st.session.doc_text(&r.file).map(str::to_string) {
        let start = r.start.min(doc.len());
        let anchor = r.anchor.clamp(start, doc.len());
        let new = format!("{}{}{}", &doc[..start], r.original, &doc[anchor..]);
        st.session.local_change(&r.file, &new);
    }
    st.session.send_stream_state(&r.file, false);
}

/// One pump tick: drain the relay; remote spans shift the streamed region's
/// offsets (concurrent human edits), StreamCancel interrupts and reverts.
fn pump_session(state: &Mutex<PairState>, writer: &Mutex<Option<ChildStdin>>, req_id: &AtomicU64) {
    let mut interrupt = false;
    {
        let mut st = state.lock().unwrap();
        let events = st.session.poll(|_| None);
        for event in events {
            match event {
                CollabEvent::RemoteEdit { file, spans } => {
                    if let Some(r) = st.region.as_mut()
                        && r.file == file
                    {
                        for span in &spans {
                            r.start = shift_offset(r.start, span);
                            r.anchor = shift_offset(r.anchor, span);
                        }
                    }
                }
                CollabEvent::StreamCancel => {
                    if !st.turn_active || st.cancelled {
                        continue;
                    }
                    st.cancelled = true;
                    st.pending_note = Some(
                        "Note: your previous streamed edit was cancelled and reverted by a \
                         participant. Stop that approach and ask what they want instead."
                            .to_string(),
                    );
                    revert_region(&mut st);
                    interrupt = st.can_interrupt;
                    println!("\n[stream cancelled by a participant; reverted]");
                }
                _ => {}
            }
        }
    }
    // The claude write happens outside the state lock (lock order: state
    // then writer, same as send_turn; never both held).
    if interrupt && let Some(w) = writer.lock().unwrap().as_mut() {
        let id = req_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({
            "type": "control_request",
            "request_id": format!("croft-pair-{id}"),
            "request": { "subtype": "interrupt" },
        });
        let _ = writeln!(w, "{msg}");
        let _ = w.flush();
    }
}

/// `@<file> <task>` focuses a buffer; everything else is just the task.
fn parse_task_line(line: &str) -> (Option<String>, String) {
    let Some(rest) = line.strip_prefix('@') else {
        return (None, line.to_string());
    };
    match rest.split_once(char::is_whitespace) {
        Some((file, task)) => (Some(file.to_string()), task.trim().to_string()),
        None => (Some(rest.to_string()), String::new()),
    }
}

/// Compose and send one user turn: pending cancel note, the task, and the
/// target file's current buffer (so the model's fence coordinates have a
/// ground truth).
fn send_turn(
    state: &Mutex<PairState>,
    writer: &Mutex<Option<ChildStdin>>,
    line: &str,
) -> Result<()> {
    let (file, task) = parse_task_line(line);
    if let Some(file) = &file {
        let mut st = state.lock().unwrap();
        st.target_file = Some(file.clone());
        st.session.request_file(file);
        drop(st);
        // Bounded wait so the injection below can carry the buffer.
        let deadline = Instant::now() + LIVE_TIMEOUT;
        while Instant::now() < deadline {
            let st = state.lock().unwrap();
            if st.session.is_live(file) || !st.session.is_bootstrapping(file) {
                break;
            }
            drop(st);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let content = {
        let mut st = state.lock().unwrap();
        let mut content = String::new();
        if let Some(note) = st.pending_note.take() {
            content.push_str(&note);
            content.push_str("\n\n");
        }
        content.push_str(&task);
        if let Some(file) = st.target_file.clone()
            && let Some(text) = st.session.doc_text(&file)
        {
            content.push_str(&format!(
                "\n\n--- CURRENT BUFFER ({file}) ---\n{text}\n--- END BUFFER ---"
            ));
        }
        st.turn_active = true;
        content
    };
    let msg = json!({
        "type": "user",
        "message": { "role": "user", "content": content },
    });
    let mut w = writer.lock().unwrap();
    let w = w.as_mut().context("claude stdin already closed")?;
    writeln!(w, "{msg}").context("writing user turn to claude")?;
    w.flush().context("flushing claude stdin")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::ResolvedSpan;

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
                    FenceEvent::EditStart { file, start, end } => {
                        Some((file.clone(), *start, *end))
                    }
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
}
