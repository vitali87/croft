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

mod caret;
mod fence;
pub(crate) mod local;
mod notes;
mod offsets;
pub(crate) mod proactive;
mod provider;
mod state;
mod voice;
use fence::*;
#[cfg(test)]
pub(crate) use notes::inject_note;
pub(crate) use notes::inject_note_now;
use notes::*;
use offsets::*;
pub use provider::PairConfig;
pub use provider::Provider;
use provider::*;
pub(crate) use state::StagedTurn;
pub(crate) use state::lock_briefly;
use voice::*;

use std::io::{BufRead, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::collab::{
    CollabChannel, CollabEvent, CollabRole, CollabSession, ResolvedSpan, position,
};

// ---------------------------------------------------------------------------
// Pilot runtime: spawn the claude CLI in stream-json mode, parse its token
// stream through the fence machine, and apply fences live through a collab
// guest seat. Cancel rides the relay (CollabMsg::StreamCancel) and lands as
// a control_request interrupt on claude's stdin.
// ---------------------------------------------------------------------------

/// The tools the pilot's claude may call without a prompt (approved product
/// decision: read-only plus the stream; the ONLY write path is the fence).
/// The `croft-collab` MCP server here is a second, read-only seat riding
/// along for live buffer queries; the pilot itself owns the writing seat.
/// `Read`, `Grep` and `Glob` are deliberately not listed: a bare rule
/// matches every path (`~/.ssh`, `~/.aws`), while with no rule `dontAsk`
/// runs them inside the workspace and denies them outside it (#1926).
const ALLOWED_TOOLS: &str = "mcp__croft-collab__collab_open,mcp__croft-collab__collab_read,\
     mcp__croft-collab__collab_status";

/// The built-in tools the pilot's claude has at all: no settings file can
/// allow a tool outside this set, so there is no shell and no write tool
/// whatever the user's or the workspace's settings say (#1926).
const BUILTIN_TOOLS: &str = "Read,Grep,Glob";

/// The settings the pilot's claude loads: the user's own (sign-in, model,
/// their own hooks). The workspace's `.claude/settings.json` belongs to
/// whoever committed it, and its hooks would run the moment the navigator
/// starts, before anyone asked it anything (#1926).
const SETTING_SOURCES: &str = "user";

/// How long an EditStart waits for its file to bootstrap, a hair past the
/// session's own 3s deadline (same reasoning as the collab agent's).
const LIVE_TIMEOUT: Duration = Duration::from_secs(4);

/// The fence currently streaming into a buffer: byte offsets into the
/// replica text (kept fresh against concurrent remote edits by
/// [`shift_offset`]) plus the slice the fence's range deleted, for revert.
struct StreamRegion {
    file: String,
    start: usize,
    anchor: usize,
    original: String,
    /// Notes whose anchors sat at or inside the fence's range when it
    /// opened, as `(note id, offset - start)`: the delete collapses them
    /// and the streamed inserts drag them along, so the revert can only
    /// restore them from these recorded deltas (`start` keeps tracking
    /// remote edits, so `start + delta` stays exact in the restored text).
    displaced: Vec<(u64, usize)>,
}

/// Shared pilot state: the collab seat plus per-turn stream bookkeeping.
/// Locked briefly by the reader thread (apply), the pump thread (remote
/// shifts, cancel), and the driver (turn injection); never held across a
/// sleep or a child-process write.
pub(crate) struct PairState {
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
    /// Bumped by every [`Self::stage_turn`]; pairs with
    /// [`StagedTurn::stage_gen`] so a deferred rollback can recognise that
    /// a newer turn has staged and drop itself.
    stage_gen: u64,
    /// Anchored navigator notes; offsets kept fresh by the pump.
    notes: Vec<Note>,
    /// Next note id (monotonic; ids are never reused within a seat).
    next_note_id: u64,
    /// The NOTE fence currently streaming: (file, 0-based row, body so far).
    note_in_flight: Option<(String, usize, String)>,
    /// The in-process host's sink: when seated inside croft the pilot's
    /// voice (commentary, notes, diagnostics) goes here instead of
    /// stdout/stderr, which belong to the TUI. None = terminal REPL.
    events: Option<Sender<crate::pair_host::PairEvent>>,
    /// This turn is a yielded one: the navigator may only comment. EDIT
    /// fences are discarded at the host, not just discouraged in the
    /// prompt. Reset at each turn's result.
    comment_only: bool,
    /// Where the seat wants its visible caret parked (file, 0-based row)
    /// once the file's bootstrap lands; the pump resolves it. The caret is
    /// the navigator's persistent presence between edits.
    pending_caret: Option<(String, usize)>,
    /// What the navigator last saw of each file (yield turns diff against
    /// this instead of resending history).
    last_seen: std::collections::HashMap<String, String>,
}

/// Bounded wait for a requested file's bootstrap ([`LIVE_TIMEOUT`]). True
/// when the file went live; false when nobody answered (no owner).
fn wait_live(state: &Mutex<PairState>, file: &str) -> bool {
    let deadline = Instant::now() + LIVE_TIMEOUT;
    loop {
        let st = state.lock().unwrap();
        if st.session.is_live(file) {
            return true;
        }
        if !st.session.is_bootstrapping(file) || Instant::now() >= deadline {
            return false;
        }
        drop(st);
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Where a composed user turn goes. The claude backend wraps it in a
/// stream-json user message on the child's stdin; the local backend queues
/// the raw body to its HTTP worker instead. Everything upstream of the sink
/// (turn composition, pending notes, turn_active) is shared.
#[derive(Clone)]
pub(crate) enum TurnSink {
    Claude(Arc<Mutex<Option<ChildStdin>>>),
    Local(Sender<String>),
}

impl TurnSink {
    /// Deliver one composed user-turn body to the model conversation.
    fn send_user(&self, content: &str) -> Result<()> {
        match self {
            TurnSink::Claude(writer) => {
                let msg = json!({
                    "type": "user",
                    "message": { "role": "user", "content": content },
                });
                let mut w = writer.lock().unwrap();
                let w = w.as_mut().context("claude stdin already closed")?;
                writeln!(w, "{msg}").context("writing user turn to claude")?;
                w.flush().context("flushing claude stdin")
            }
            TurnSink::Local(turns) => {
                anyhow::ensure!(
                    turns.send(content.to_string()).is_ok(),
                    "the local turn worker is gone"
                );
                Ok(())
            }
        }
    }
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
pub(crate) struct TurnEnd {
    pub(crate) is_error: bool,
    /// The turn was cancelled by a participant (the interrupt surfaces as
    /// an error result; the driver names the real cause instead).
    pub(crate) cancelled: bool,
    pub(crate) text: String,
}

/// The claude CLI invocation for a pair session: a persistent stream-json
/// conversation over stdio, sandboxed to the read-only toolbox, with the
/// Install dirs to probe for the claude CLI when it is not on PATH (npm
/// global bin, the native installer's `~/.claude/local`, homebrew).
fn claude_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".claude").join("local"));
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".npm-global").join("bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

/// Resolve the claude CLI to a spawnable program string. A terminal launch
/// has claude on PATH and spawns it by bare name; a macOS GUI launch
/// (Croft.app) inherits the stripped launchd PATH, so `Command::new("claude")`
/// fails with ENOENT and the navigator can never seat. When claude is absent
/// from PATH we probe the usual install dirs and pin the absolute path,
/// mirroring the LSP path-only resolution. `None` means fall back to the bare
/// name so the spawn error still surfaces to the user.
fn resolve_claude_in(on_path: bool, dirs: &[PathBuf]) -> Option<String> {
    if on_path {
        return Some(String::from("claude"));
    }
    dirs.iter()
        .map(|d| d.join("claude"))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

/// The claude program to spawn, resolved to an absolute path on a stripped
/// GUI PATH (see [`resolve_claude_in`]).
fn claude_program() -> String {
    resolve_claude_in(
        crate::lsp::manager::is_on_path("claude"),
        &claude_search_dirs(),
    )
    .unwrap_or_else(|| String::from("claude"))
}

/// fence protocol appended to its system prompt and a read-only collab-agent
/// seat as its MCP server.
pub(crate) fn claude_command(cfg: &PairConfig) -> Result<Command> {
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
    let mut cmd = Command::new(claude_program());
    cmd.arg("-p")
        .args(["--input-format", "stream-json"])
        .args(["--output-format", "stream-json"])
        .arg("--verbose")
        .arg("--include-partial-messages")
        .args(["--permission-mode", "dontAsk"])
        .args(["--setting-sources", SETTING_SOURCES])
        .args(["--tools", BUILTIN_TOOLS])
        .args(["--allowedTools", ALLOWED_TOOLS])
        .arg("--strict-mcp-config")
        .args(["--mcp-config", &mcp_config.to_string()])
        .args(["--append-system-prompt", &system_prompt(&cfg.provider)])
        .current_dir(&cfg.workspace);
    if let Some(model) = &cfg.model {
        cmd.args(["--model", model]);
    }
    Ok(cmd)
}

/// `croft pair --repl`: join the workspace's collab relay as the pilot seat
/// on the configured provider and run the REPL until stdin closes.
pub fn run(cfg: PairConfig) -> Result<()> {
    let pilot = match &cfg.provider {
        Provider::Claude => seat_pilot(&cfg.socket, &cfg.name, claude_command(&cfg)?, None)?,
        Provider::Local { base_url } => {
            let model = cfg
                .model
                .as_deref()
                .context("a local provider needs --model (there is no CLI default)")?;
            seat_local(&cfg.socket, &cfg.name, base_url, model, None)?
        }
    };
    let stdin = std::io::stdin();
    run_pilot(pilot, &cfg.name, cfg.task, &mut stdin.lock())
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
enum Transport {
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

/// The debug REPL driver (hidden `--repl`), transport-agnostic so the e2e
/// tests can drive it with a scripted fake. Blocks until `input` (the
/// pilot's own terminal) hits EOF or the model conversation hangs up
/// mid-turn.
fn run_pilot(
    pilot: Pilot,
    name: &str,
    task: Option<String>,
    input: &mut dyn BufRead,
) -> Result<()> {
    // REPL: the initial --task then the pilot's stdin, one turn per line.
    println!("croft pair: '{name}' seated; type a task ('@<file> <task>' to focus a buffer)");
    let mut pending = task;
    let mut hung_up = false;
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
        send_turn(&pilot.state, &pilot.sink, &line)?;
        match pilot.turn_rx.recv() {
            Ok(end) if end.cancelled => println!("\n[turn cancelled]"),
            Ok(end) if end.is_error => println!("\n[turn failed: {}]", end.text),
            Ok(_) => println!("\n[turn done]"),
            Err(_) => {
                // Reader gone: claude hung up mid-turn.
                hung_up = true;
                break;
            }
        }
    }
    pilot.shutdown();
    anyhow::ensure!(!hung_up, "claude exited mid-conversation");
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
            // claude's stdout is an external input: a result with no
            // dispatched turn behind it (every dispatch sets turn_active,
            // and the arm below clears it exactly once) is a duplicate or
            // spontaneous emission. Ending a turn it does not own would
            // queue a phantom TurnEnd — releasing the host's busy gate
            // under a newer turn — and the finish()/flag resets would
            // revert that turn's open fence and staging.
            if !state.lock().unwrap().turn_active {
                return;
            }
            for event in fence.finish() {
                apply_fence_event(state, event);
            }
            let mut st = state.lock().unwrap();
            let cancelled = st.cancelled;
            st.turn_active = false;
            st.cancelled = false;
            st.discarding = false;
            st.comment_only = false;
            drop(st);
            let _ = turn_tx.send(TurnEnd {
                is_error: msg
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                cancelled,
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
            say(state, &text);
        }
        FenceEvent::EditStart { file, start, end } => {
            {
                let mut st = state.lock().unwrap();
                if st.cancelled {
                    st.discarding = true;
                    return;
                }
                if st.comment_only {
                    // The host gate, not just a prompt rule: a yielded turn
                    // may never touch a buffer, whatever the model says.
                    st.discarding = true;
                    drop(st);
                    diag(
                        state,
                        crate::output::OutputLevel::Warn,
                        "[pair] edit suppressed: this is a yielded, comment-only turn",
                    );
                    return;
                }
                st.target_file = Some(file.clone());
                st.session.request_file(&file);
            }
            if wait_live(state, &file) {
                let mut st = state.lock().unwrap();
                open_region(&mut st, &file, start, end);
            } else {
                state.lock().unwrap().discarding = true;
                diag(
                    state,
                    crate::output::OutputLevel::Warn,
                    &format!(
                        "[pair] no live croft session serves {file}; edit dropped \
                         (start croft in this workspace first)"
                    ),
                );
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
            // Our own insert moves any note anchored below it in this file
            // (notes and edits can both land in one ask turn).
            let span = ResolvedSpan {
                at: anchor,
                deleted: 0,
                inserted: delta.clone(),
            };
            shift_notes(&mut st.notes, &file, std::slice::from_ref(&span));
            let next = anchor + delta.len();
            if let Some(r) = st.region.as_mut() {
                r.anchor = next;
            }
            let lines: Vec<String> = new.split('\n').map(String::from).collect();
            let (row, col) = position(&lines, next);
            st.caret_now(&file, row, col);
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
        FenceEvent::NoteStart { file, row } => {
            let mut st = state.lock().unwrap();
            if st.cancelled {
                return;
            }
            // Non-blocking: the offset resolves lazily at NoteEnd, so a
            // bootstrap kicked off here has the body's stream time to land.
            st.session.request_file(&file);
            st.note_in_flight = Some((file, row, String::new()));
        }
        FenceEvent::NoteBody(delta) => {
            let mut st = state.lock().unwrap();
            if let Some((_, _, body)) = st.note_in_flight.as_mut() {
                body.push_str(&delta);
            }
        }
        FenceEvent::NoteEnd => {
            let pending = {
                let mut st = state.lock().unwrap();
                let pending = st.note_in_flight.take();
                if st.cancelled { None } else { pending }
            };
            let Some((file, row, body)) = pending else {
                return;
            };
            // NoteStart's request_file may still be bootstrapping (a fast
            // fence outruns the 25ms pump); give it the same bounded wait
            // an edit gets. Still no live doc = the owner never served the
            // file: nothing to anchor to (mirrors the edit-drop path).
            if !wait_live(state, &file) {
                diag(
                    state,
                    crate::output::OutputLevel::Warn,
                    &format!(
                        "[pair] no live croft session serves {file}; note dropped \
                         (start croft in this workspace first)"
                    ),
                );
                return;
            }
            let mut st = state.lock().unwrap();
            let Some(lines) = st.doc_lines(&file) else {
                // The file left the live session between wait_live releasing
                // the lock and this reacquisition: same outcome, same warning.
                drop(st);
                diag(
                    state,
                    crate::output::OutputLevel::Warn,
                    &format!(
                        "[pair] no live croft session serves {file}; note dropped \
                         (start croft in this workspace first)"
                    ),
                );
                return;
            };
            let offset = note_offset(&lines, row);
            let row_now = position(&lines, offset).0;
            // The AI is visibly "looking" where it just commented.
            st.caret_now(&file, row_now, 0);
            let id = st.take_note_id();
            let sink = st.events.clone();
            // Pushed under the SAME lock as the offset computation: pushing
            // after the REPL's say() left a window where a concurrent
            // remote edit shifted every note EXCEPT this not-yet-pushed
            // one, stranding its anchor below the edit.
            st.notes.push(Note {
                id,
                file: file.clone(),
                offset,
                body: body.clone(),
            });
            match sink {
                Some(tx) => {
                    let _ = tx.send(crate::pair_host::PairEvent::NoteAdded {
                        file,
                        row: row_now,
                        body,
                    });
                }
                None => {
                    // REPL driver: no event channel, so print the note or it
                    // would be silently swallowed (say re-locks internally,
                    // hence the drop).
                    drop(st);
                    say(state, &format!("[note {file}:{}] {body}\n", row_now + 1));
                }
            }
        }
        FenceEvent::NoteAbort => {
            state.lock().unwrap().note_in_flight = None;
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
        // The OUTPUT channel, never stderr: the resident navigator runs
        // in-process, so an eprintln! lands on the alternate screen and
        // corrupts the render (the pdftoppm/DAP capture class). The
        // whole-line header form makes an inverted range a one-token slip
        // for a weak local model.
        crate::output::push(
            "Navigator",
            crate::output::OutputLevel::Warn,
            "fence range is inverted; edit dropped",
        );
        return;
    }
    let original = doc[s..e].to_string();
    let new = format!("{}{}", &doc[..s], &doc[e..]);
    // Announce the stream before its first edit, the region's delete: the
    // host groups the stream's edits into one undo step from the moment it
    // sees the announcement, so a delete sent first was a step of its own
    // (#1207).
    st.session.send_stream_state(file, true);
    st.session.local_change(file, &new);
    // Anchors at or inside the range are about to be collapsed by the
    // delete (and then dragged along by the streamed inserts): record
    // their pre-edit deltas so a revert can put them back exactly.
    let displaced: Vec<(u64, usize)> = st
        .notes
        .iter()
        .filter(|n| n.file == file && (s..e).contains(&n.offset))
        .map(|n| (n.id, n.offset - s))
        .collect();
    // Our own delete moves any note anchored below the cut in this file.
    let span = ResolvedSpan {
        at: s,
        deleted: e - s,
        inserted: String::new(),
    };
    shift_notes(&mut st.notes, file, std::slice::from_ref(&span));
    st.region = Some(StreamRegion {
        file: file.to_string(),
        start: s,
        anchor: s,
        original,
        displaced,
    });
    st.discarding = false;
    let lines: Vec<String> = new.split('\n').map(String::from).collect();
    let (row, col) = position(&lines, s);
    st.caret_now(file, row, col);
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
        // The inverse of the shifts open_region and EditBody applied: the
        // revert deletes the streamed bytes and puts the original back, so
        // notes the edit moved must move back with it.
        let span = ResolvedSpan {
            at: start,
            deleted: anchor - start,
            inserted: r.original.clone(),
        };
        shift_notes(&mut st.notes, &r.file, std::slice::from_ref(&span));
        // Anchors that sat inside the range cannot be un-shifted (the
        // delete collapsed them); restore them from the deltas recorded at
        // open. `start` tracked remote edits all along, so `start + delta`
        // lands exactly where the anchor sat in the restored slice.
        for (id, delta) in &r.displaced {
            if let Some(n) = st
                .notes
                .iter_mut()
                .find(|n| n.id == *id && n.file == r.file)
            {
                n.offset = start + delta;
            }
        }
    }
    st.session.send_stream_state(&r.file, false);
}

/// One pump tick: drain the relay; remote spans shift the streamed region's
/// offsets (concurrent human edits), StreamCancel interrupts and reverts.
fn pump_session(state: &Mutex<PairState>, sink: &TurnSink, req_id: &AtomicU64) {
    let mut interrupt = false;
    {
        let mut st = state.lock().unwrap();
        let events = st.session.poll(|_| None);
        for event in events {
            match event {
                CollabEvent::RemoteEdit { file, spans, .. } => {
                    if let Some(r) = st.region.as_mut()
                        && r.file == file
                    {
                        for span in &spans {
                            r.start = shift_offset(r.start, span);
                            r.anchor = shift_offset(r.anchor, span);
                        }
                    }
                    shift_notes(&mut st.notes, &file, &spans);
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
                    match st.events.clone() {
                        Some(tx) => {
                            let _ = tx.send(crate::pair_host::PairEvent::Commentary(
                                "[stream cancelled by a participant; reverted]".to_string(),
                            ));
                        }
                        None => println!("\n[stream cancelled by a participant; reverted]"),
                    }
                }
                _ => {}
            }
        }
        st.resolve_pending_caret();
    }
    // The claude write happens outside the state lock (lock order: state
    // then writer, same as send_turn; never both held). Only the claude
    // backend can interrupt mid-turn; can_interrupt never arms elsewhere.
    if interrupt {
        let TurnSink::Claude(writer) = sink else {
            return; // can_interrupt only ever arms on the claude transport
        };
        if let Some(w) = writer.lock().unwrap().as_mut() {
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

/// Compose one user turn: the task and the target file context. The file
/// is ALWAYS named when known — parse_task_line has already stripped the
/// `@file` prefix, so a buffer that never went live must not erase the
/// target from the task. (The pending cancel note rides in at write time,
/// [`with_pending_note`].)
fn compose_turn_text(task: &str, file: Option<&str>, buffer: Option<&str>) -> String {
    let mut content = String::new();
    content.push_str(task);
    match (file, buffer) {
        (Some(file), Some(text)) => {
            content.push_str(&format!(
                "\n\n--- CURRENT BUFFER ({file}) ---\n{text}\n--- END BUFFER ---"
            ));
        }
        (Some(file), None) => {
            content.push_str(&format!(
                "\n\nTarget file: {file} (its buffer is not shared yet; \
                 read it with the collab tools before editing)"
            ));
        }
        (None, _) => {}
    }
    content
}

/// `content` with each line prefixed `N|` (0-based), the numbering the ask
/// and yield turns use so NOTE rows have an unambiguous ground truth.
fn numbered(content: &str) -> String {
    content
        .split('\n')
        .enumerate()
        .map(|(i, l)| format!("{i}|{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compose an ask turn: the instruction, where it was invoked (a 0-based
/// line or line range, with the gutter's 1-based number beside it), the
/// selected text when there is one, and the numbered buffer.
pub(crate) fn compose_ask_turn(
    file: &str,
    range: (usize, usize),
    selection: &str,
    instruction: &str,
    content: &str,
) -> String {
    let mut text = String::new();
    let (s, e) = range;
    if s == e {
        text.push_str(&format!(
            "Your partner invoked you on {file} at line {s} (0-based; line {} in \
             your partner's gutter).\n",
            s + 1
        ));
    } else {
        text.push_str(&format!(
            "Your partner invoked you on {file} at lines {s}-{e} (0-based; lines \
             {}-{} in your partner's gutter).\n",
            s + 1,
            e + 1
        ));
    }
    if !selection.is_empty() {
        text.push_str(&format!(
            "--- SELECTED LINES ---\n{selection}\n--- END SELECTED ---\n"
        ));
    }
    text.push_str(&format!("\nTask: {instruction}\n"));
    text.push_str(&format!(
        "\n--- CURRENT BUFFER ({file}, each line prefixed with its 0-based \
         number and '|') ---\n{}\n--- END BUFFER ---",
        numbered(content)
    ));
    text
}

/// Compose a yield turn: the driver handed the navigator the floor. Comment
/// only — the host discards EDIT fences on this turn — with the diff since
/// the navigator last saw the file, and the numbered buffer.
pub(crate) fn compose_yield_turn(file: &str, content: &str, diff: Option<&str>) -> String {
    let mut text = String::from(
        "Your partner yielded the turn: review the file below and speak \
         through NOTE fences anchored to the lines you mean. This turn is \
         COMMENT-ONLY: any EDIT fence will be discarded by the host. \
         Propose changes in your notes and wait to be asked.\n",
    );
    if let Some(diff) = diff {
        text.push_str(&format!(
            "\n--- CHANGES SINCE YOUR LAST LOOK ---\n{diff}\n--- END CHANGES ---\n"
        ));
    }
    text.push_str(&format!(
        "\n--- CURRENT BUFFER ({file}, each line prefixed with its 0-based \
         number and '|') ---\n{}\n--- END BUFFER ---",
        numbered(content)
    ));
    text
}

/// Compose a reply turn: the driver answered one of the navigator's notes
/// inside its comment box. Names the note, carries the reply, lets the
/// model edit when the reply asks for a change (#1965), and grounds the
/// model with the numbered buffer.
pub(crate) fn compose_reply_turn(
    file: &str,
    row: usize,
    note_body: &str,
    reply: &str,
    content: &str,
) -> String {
    format!(
        "Your partner replied to your note on {file} at line {row} (0-based; \
         line {gutter} in your partner's gutter).\n\
         --- YOUR NOTE ---\n{note_body}\n--- END NOTE ---\n\
         --- THEIR REPLY ---\n{reply}\n--- END REPLY ---\n\
         If their reply asks you to make a change, make it with EDIT fences. \
         Otherwise answer with NOTE fences (anchor follow-ups to the lines \
         you mean; a NOTE re-anchored to row {row} continues this box).\n\
         \n--- CURRENT BUFFER ({file}, each line prefixed with its 0-based \
         number and '|') ---\n{}\n--- END BUFFER ---",
        numbered(content),
        gutter = row + 1,
    )
}

/// Compose and send one user turn: pending cancel note, the task, and the
/// target file's current buffer (so the model's fence coordinates have a
/// ground truth).
pub(crate) fn send_turn(state: &Mutex<PairState>, sink: &TurnSink, line: &str) -> Result<()> {
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
    let (file, buffer) = {
        let st = state.lock().unwrap();
        let file = st.target_file.clone();
        let buffer = file
            .as_deref()
            .and_then(|f| st.session.doc_text(f))
            .map(str::to_string);
        (file, buffer)
    };
    if file.is_some() && buffer.is_none() {
        diag(
            state,
            crate::output::OutputLevel::Warn,
            "[pair] target file is not live (no owner answered); \
             sending the task with the file name only",
        );
    }
    let body = compose_turn_text(&task, file.as_deref(), buffer.as_deref());
    write_user_turn(state, sink, body)
}

/// Send one composed user turn through the sink, prepending any pending
/// cancel note and marking the turn active. Lock order: state then writer,
/// never both held.
pub(crate) fn write_user_turn(
    state: &Mutex<PairState>,
    sink: &TurnSink,
    body: String,
) -> Result<()> {
    let (content, note) = {
        let mut st = state.lock().unwrap();
        let note = st.pending_note.take();
        st.turn_active = true;
        (with_pending_note(note.clone(), body), note)
    };
    let sent = sink.send_user(&content);
    if sent.is_err() {
        // The turn never left: unlatch it (a wedged turn_active refused
        // every later ask/yield/reply for the seat's life) and put the
        // cancel note back — it is still owed to the model on the next
        // turn that actually sends.
        let mut st = state.lock().unwrap();
        st.turn_active = false;
        if st.pending_note.is_none() {
            st.pending_note = note;
        }
    }
    sent
}

/// The pending cancel note rides in front of the next turn's body.
fn with_pending_note(note: Option<String>, body: String) -> String {
    match note {
        Some(n) => format!("{n}\n\n{body}"),
        None => body,
    }
}

/// A scripted claude: speaks just enough stream-json for the pilot.
/// argv: <log file> <mode>. Logs every stdin line (the e2es assert the
/// interrupt landed there). "stream" mode streams one fenced edit split
/// across deltas (header split mid-marker on purpose); "cancel" mode stops
/// mid-body and BLOCKS on stdin — only the pilot's interrupt line unblocks
/// it — then streams more body the pilot must drop; "notes" pins one NOTE
/// fence; "repeat" stays seated and inserts the turn's number (1, 2, ...) at
/// 0:0 on EVERY turn;
/// "linger" refuses to exit on stdin EOF. Crate-visible so the App's
/// navigator tests drive the same fake.
#[cfg(test)]
pub(crate) const FAKE_CLAUDE: &str = r#"
import json, sys

log = open(sys.argv[1], "a")
mode = sys.argv[2]

# The real claude CLI is chatty on stderr while its MCP servers start; the
# tee must route this to OUTPUT, never into the model's comment boxes.
print("mcp server starting", file=sys.stderr)
sys.stderr.flush()

def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def delta(text):
    emit({"type": "stream_event", "event": {
        "type": "content_block_delta",
        "delta": {"type": "text_delta", "text": text}}})

emit({"type": "system", "subtype": "init",
      "capabilities": ["interrupt_receipt_v1"]})

line = sys.stdin.readline()
if not line:
    sys.exit(0)
log.write(line)
log.flush()

if mode == "hang":
    # Read one turn, then never respond: turn_active stays true so the host
    # reports busy and rejects a second turn.
    import time
    time.sleep(60)
    sys.exit(0)

if mode == "notes":
    delta("Reviewing.\n")
    delta("<<<NOTE demo.txt:1>>>\n")
    delta("second line could be tighter\n")
    delta("<<<END>>>\n")
    emit({"type": "result", "subtype": "success",
          "is_error": False, "result": "ok"})
    # Stay seated like the real CLI: wait for the next turn (or EOF).
    sys.stdin.readline()
    sys.exit(0)

if mode == "repeat":
    # An edit on every turn, numbered so a test can tell which turn's edit
    # reached the owner and watch the host gate decide turn by turn.
    turn = 0
    while line:
        turn += 1
        delta("<<<EDIT demo.txt:0:0-0:0>>>\n%d\n<<<END>>>\n" % turn)
        emit({"type": "result", "subtype": "success",
              "is_error": False, "result": "ok"})
        line = sys.stdin.readline()
        log.write(line)
        log.flush()
    sys.exit(0)

delta("Let me fix that.\n")
delta("<<<EDIT demo.txt:0:6-0:11>")
delta(">>\n")
delta("streamed")
if mode == "cancel":
    line2 = sys.stdin.readline()
    log.write(line2)
    log.flush()
    delta(" MORE-AFTER-CANCEL")
    delta("\n<<<END>>>\n")
    emit({"type": "result", "subtype": "success",
          "is_error": False, "result": "cancelled turn"})
else:
    delta(" edit")
    delta("\n<<<END>>>\nDone.\n")
    emit({"type": "result", "subtype": "success",
          "is_error": False, "result": "ok"})
if mode == "linger":
    # The real claude CLI does NOT reliably exit on stdin EOF (MCP
    # teardown lingers); the pilot must not gamble its own exit on it.
    import time
    time.sleep(60)
"#;

#[cfg(test)]
mod tests;
