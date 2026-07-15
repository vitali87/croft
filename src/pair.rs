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
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::collab::{
    CollabChannel, CollabEvent, CollabRole, CollabSession, ResolvedSpan, position,
};

/// Start of an edit-fence header line.
const EDIT_MARKER: &str = "<<<EDIT ";
/// Start of a note-fence header line (anchored commentary, never applied).
const NOTE_MARKER: &str = "<<<NOTE ";
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
    /// A well-formed header opened a note anchored to the 0-based `row` of
    /// `file`: commentary tied to a line, accumulated and surfaced whole at
    /// `NoteEnd`, never applied to any buffer.
    NoteStart { file: String, row: usize },
    /// The next fragment of the current note's body, in stream order.
    NoteBody(String),
    /// The note closed cleanly; its accumulated body may anchor.
    NoteEnd,
    /// The turn ended mid-note: nothing anchors.
    NoteAbort,
}

/// Where the machine is between pushes.
enum FenceState {
    /// Outside a fence: complete lines classify as header or commentary.
    Outside,
    /// Inside a fence body. `held_newline`: a body '\n' was consumed but not
    /// emitted (it may turn out to be the newline that separates the body
    /// from `<<<END>>>`, which is stripped). `at_line_start`: the unprocessed
    /// tail starts a fresh line, so it could still become the END marker.
    /// `is_note`: the fence opened with NOTE, so body/end/abort events use
    /// the note variants (same scanner, different consumer semantics).
    Body {
        held_newline: bool,
        at_line_start: bool,
        is_note: bool,
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
                            is_note: false,
                        };
                    } else if let Some(start) = parse_note_header(trimmed) {
                        events.push(start);
                        self.state = FenceState::Body {
                            held_newline: false,
                            at_line_start: true,
                            is_note: true,
                        };
                    } else {
                        events.push(FenceEvent::Commentary(line));
                    }
                }
                FenceState::Body {
                    ref mut held_newline,
                    ref mut at_line_start,
                    is_note,
                } => {
                    let body_event = if is_note {
                        FenceEvent::NoteBody
                    } else {
                        FenceEvent::EditBody
                    };
                    if *at_line_start {
                        match self.buf.find('\n') {
                            Some(nl) => {
                                if &self.buf[..nl] == END_MARKER {
                                    // The held newline separated body from
                                    // the marker: stripped, not body.
                                    self.buf.drain(..=nl);
                                    events.push(if is_note {
                                        FenceEvent::NoteEnd
                                    } else {
                                        FenceEvent::EditEnd
                                    });
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
                                    events.push(body_event(body));
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
                                events.push(body_event(body));
                                *at_line_start = false;
                                break;
                            }
                        }
                    } else {
                        match self.buf.find('\n') {
                            Some(nl) => {
                                if nl > 0 {
                                    events.push(body_event(self.buf[..nl].to_string()));
                                }
                                self.buf.drain(..=nl);
                                *held_newline = true;
                                *at_line_start = true;
                            }
                            None => {
                                if !self.buf.is_empty() {
                                    events.push(body_event(std::mem::take(&mut self.buf)));
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
            FenceState::Body {
                at_line_start,
                is_note,
                ..
            } => {
                if at_line_start && self.buf == END_MARKER {
                    events.push(if is_note {
                        FenceEvent::NoteEnd
                    } else {
                        FenceEvent::EditEnd
                    });
                } else if is_note {
                    events.push(FenceEvent::NoteAbort);
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

/// Parse `<<<NOTE <file>:<row>>>>` into its [`FenceEvent::NoteStart`]. The
/// row binds rightmost so the file name may itself contain ':'. None = not
/// a well-formed header (the line degrades to commentary).
fn parse_note_header(line: &str) -> Option<FenceEvent> {
    let inner = line.strip_prefix(NOTE_MARKER)?.strip_suffix(">>>")?;
    let (file, row) = inner.rsplit_once(':')?;
    if file.is_empty() {
        return None;
    }
    Some(FenceEvent::NoteStart {
        file: file.to_string(),
        row: row.parse().ok()?,
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

/// Byte offset of the start of `row` in `lines`, clamped to the last line
/// (models sometimes cite a row just past EOF).
fn note_offset(lines: &[String], row: usize) -> usize {
    let row = row.min(lines.len().saturating_sub(1));
    crate::collab::byte_offset(lines, row, 0)
}

/// Replay remote-edit spans over every note anchored in `file`, keeping
/// their offsets true while other participants type.
fn shift_notes(notes: &mut [Note], file: &str, spans: &[ResolvedSpan]) {
    for n in notes.iter_mut().filter(|n| n.file == file) {
        for span in spans {
            n.offset = shift_offset(n.offset, span);
        }
    }
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
- The header carries the file plus EXACTLY FOUR integers: START_ROW:START_COL-END_ROW:END_COL. Never omit the rows — for a single-line edit on line N the header reads <file>:N:C1-N:C2 (e.g. replacing columns 15..19 of line 0 of demo.txt is `<<<EDIT demo.txt:0:15-0:19>>>`, NOT `demo.txt:15:19`). A header without all four numbers is ignored as plain text and your edit does not happen.
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

To pin a remark to a specific line WITHOUT editing anything, emit a note fence:

<<<NOTE <file>:<row>>>>
<what you want to say about that line>
<<<END>>>

Note rules:
- The header carries the file plus EXACTLY ONE integer, the 0-based row the note speaks about (e.g. `<<<NOTE demo.txt:4>>>` pins a remark to line 4 of demo.txt). Your partner sees it attached to that line in their editor.
- Notes never change any buffer. Use them for review remarks, questions, and change proposals.

Some user messages number the buffer: each line arrives prefixed with `N|` where N is its 0-based row. The prefix is a label, not content — character columns still count from the first character AFTER the first `|`, and NOTE/EDIT rows use exactly those N values.

Turn kinds:
- An ask turn names a task and where you were invoked: you may edit with EDIT fences and remark with NOTE fences.
- A yielded turn says it is COMMENT-ONLY: your partner handed you the floor to review, not to type. EDIT fences on such a turn are DISCARDED by the editor. Speak in NOTE fences anchored to the lines you mean, propose changes there, and wait to be asked before editing.

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

/// One anchored navigator note: a byte offset into `file`'s replica text
/// (kept fresh against concurrent edits exactly like the stream region)
/// plus the accumulated body.
pub struct Note {
    pub file: String,
    pub offset: usize,
    pub body: String,
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
    /// Anchored navigator notes; offsets kept fresh by the pump.
    notes: Vec<Note>,
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
    /// What the navigator last saw of each file (yield turns diff against
    /// this instead of resending history).
    last_seen: std::collections::HashMap<String, String>,
}

impl PairState {
    /// The live replica text of `file`, split into lines (None = not live).
    pub(crate) fn doc_lines(&self, file: &str) -> Option<Vec<String>> {
        self.session
            .doc_text(file)
            .map(|t| t.split('\n').map(String::from).collect())
    }

    /// The anchored notes sitting in `file`, in landing order.
    pub(crate) fn notes_in<'a>(&'a self, file: &'a str) -> impl Iterator<Item = &'a Note> {
        self.notes.iter().filter(move |n| n.file == file)
    }

    /// True between sending a user turn and seeing its result. A new turn
    /// must not be sent while this holds: the shared `comment_only` flag and
    /// the target file would be clobbered mid-stream, defeating the
    /// host-enforced comment-only guarantee on a yield.
    pub(crate) fn turn_active(&self) -> bool {
        self.turn_active
    }

    /// Drop `file`'s notes: a new turn targeting the file supersedes its
    /// previous say.
    pub(crate) fn clear_notes(&mut self, file: &str) {
        self.notes.retain(|n| n.file != file);
    }

    /// Drop every note in every file (the user asked for a clean slate).
    pub(crate) fn clear_all_notes(&mut self) {
        self.notes.clear();
    }

    /// Arm a host turn targeting `file`: mode, target, note supersession,
    /// and the last-seen bookkeeping the yield diff reads. Returns what the
    /// navigator previously saw of the file (None = first look).
    #[allow(dead_code)]
    pub(crate) fn begin_turn(
        &mut self,
        file: &str,
        content: &str,
        comment_only: bool,
    ) -> Option<String> {
        self.comment_only = comment_only;
        self.target_file = Some(file.to_string());
        self.session.request_file(file);
        self.clear_notes(file);
        self.last_seen.insert(file.to_string(), content.to_string())
    }
}

/// The pilot's voice: an event to the in-process host when seated inside
/// croft (stdout belongs to the TUI there), else the debug REPL's stdout.
fn say(state: &Mutex<PairState>, text: &str) {
    let sink = state.lock().unwrap().events.clone();
    match sink {
        Some(tx) => {
            let _ = tx.send(crate::pair_host::PairEvent::Commentary(text.to_string()));
        }
        None => {
            print!("{text}");
            let _ = std::io::Stdout::flush(&mut std::io::stdout());
        }
    }
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

/// A seated pilot: the claude child plus the reader and pump threads over
/// shared state. Both drivers — the debug REPL ([`run_pilot`]) and the
/// in-process [`crate::pair_host::PairHost`] — run on top of this.
pub(crate) struct Pilot {
    pub(crate) state: Arc<Mutex<PairState>>,
    pub(crate) writer: Arc<Mutex<Option<ChildStdin>>>,
    pub(crate) turn_rx: Receiver<TurnEnd>,
    stop: Arc<AtomicBool>,
    guard: ChildGuard,
    reader: std::thread::JoinHandle<()>,
    pump: std::thread::JoinHandle<()>,
}

impl Pilot {
    /// The claude child's stdout hit EOF: the conversation is gone.
    pub(crate) fn reader_finished(&self) -> bool {
        self.reader.is_finished()
    }

    /// Tear the seat down: leave no stream badge behind, hang up, then
    /// grace-kill. The real claude CLI does not reliably exit on stdin EOF
    /// (MCP teardown lingers), and the reader joins only when claude's
    /// stdout closes — so the polite exit gets a short grace before the
    /// kill. The caller's exit must never hinge on a child's manners.
    pub(crate) fn shutdown(mut self) {
        {
            let mut st = self.state.lock().unwrap();
            revert_region(&mut st);
        }
        self.stop.store(true, Ordering::Relaxed);
        self.writer.lock().unwrap().take(); // EOF asks claude to end
        let grace = Instant::now() + Duration::from_secs(2);
        loop {
            match self.guard.0.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < grace => std::thread::sleep(Duration::from_millis(50)),
                _ => {
                    let _ = self.guard.0.kill();
                    break;
                }
            }
        }
        let _ = self.pump.join();
        let _ = self.reader.join();
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
    let state = Arc::new(Mutex::new(PairState {
        session,
        region: None,
        discarding: false,
        cancelled: false,
        can_interrupt: false,
        turn_active: false,
        target_file: None,
        pending_note: None,
        notes: Vec::new(),
        note_in_flight: None,
        events: events.clone(),
        comment_only: false,
        last_seen: std::collections::HashMap::new(),
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
    let guard = ChildGuard(child);
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
                    Ok(_) => match &events {
                        Some(tx) => {
                            let _ = tx.send(crate::pair_host::PairEvent::Commentary(format!(
                                "[claude] {}",
                                line.trim_end()
                            )));
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

    Ok(Pilot {
        state,
        writer,
        turn_rx,
        stop,
        guard,
        reader,
        pump,
    })
}

/// The debug REPL driver (hidden `--repl`), claude-command-agnostic so the
/// e2e tests can drive it with a scripted fake. Blocks until `input` (the
/// pilot's own terminal) hits EOF or the claude child hangs up mid-turn.
fn run_pilot(
    socket: &Path,
    name: &str,
    task: Option<String>,
    cmd: Command,
    input: &mut dyn BufRead,
) -> Result<()> {
    let pilot = seat_pilot(socket, name, cmd, None)?;

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
        send_turn(&pilot.state, &pilot.writer, &line)?;
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
                    say(
                        state,
                        "[pair] edit suppressed: this is a yielded, \
                         comment-only turn\n",
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
                say(
                    state,
                    &format!(
                        "[pair] no live croft session serves {file}; edit dropped \
                         (start croft in this workspace first)\n"
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
                return;
            }
            let mut st = state.lock().unwrap();
            let Some(lines) = st
                .session
                .doc_text(&file)
                .map(|t| t.split('\n').map(String::from).collect::<Vec<_>>())
            else {
                return;
            };
            let offset = note_offset(&lines, row);
            let row_now = position(&lines, offset).0;
            if let Some(tx) = st.events.clone() {
                let _ = tx.send(crate::pair_host::PairEvent::NoteAdded {
                    file: file.clone(),
                    row: row_now,
                    body: body.clone(),
                });
            }
            st.notes.push(Note { file, offset, body });
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
/// line or line range), the selected text when there is one, and the
/// numbered buffer.
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
            "Your partner invoked you on {file} at line {s} (0-based).\n"
        ));
    } else {
        text.push_str(&format!(
            "Your partner invoked you on {file} at lines {s}-{e} (0-based).\n"
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

/// Compose and send one user turn: pending cancel note, the task, and the
/// target file's current buffer (so the model's fence coordinates have a
/// ground truth).
pub(crate) fn send_turn(
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
        say(
            state,
            "[pair] target file is not live (no owner answered); \
             sending the task with the file name only\n",
        );
    }
    let body = compose_turn_text(&task, file.as_deref(), buffer.as_deref());
    write_user_turn(state, writer, body)
}

/// Write one composed user turn to claude's stdin, prepending any pending
/// cancel note and marking the turn active. Lock order: state then writer,
/// never both held.
pub(crate) fn write_user_turn(
    state: &Mutex<PairState>,
    writer: &Mutex<Option<ChildStdin>>,
    body: String,
) -> Result<()> {
    let content = {
        let mut st = state.lock().unwrap();
        let note = st.pending_note.take();
        st.turn_active = true;
        with_pending_note(note, body)
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
/// fence; "linger" refuses to exit on stdin EOF. Crate-visible so the App's
/// navigator tests drive the same fake.
#[cfg(test)]
pub(crate) const FAKE_CLAUDE: &str = r#"
import json, sys

log = open(sys.argv[1], "a")
mode = sys.argv[2]

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
mod tests {
    use super::*;
    use crate::collab::{ResolvedSpan, relay_serve};
    use crate::lsp::manager::is_on_path;
    use std::sync::atomic::{AtomicBool, Ordering};

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
            let deadline = Instant::now() + Duration::from_secs(10);
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
    }

    impl Drop for OwnerHarness {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(p) = self.pump.take() {
                let _ = p.join();
            }
        }
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
            run_pilot(
                &socket,
                "pilot",
                Some("fix demo.txt".into()),
                cmd,
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
        // must return promptly despite the child sleeping for 60s.
        let deadline = Instant::now() + Duration::from_secs(10);
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

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut events: Vec<crate::pair_host::PairEvent> = Vec::new();
        while !events
            .iter()
            .any(|e| matches!(e, crate::pair_host::PairEvent::TurnDone { .. }))
        {
            assert!(Instant::now() < deadline, "no TurnDone; saw {events:?}");
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
        assert_eq!(
            host.notes_snapshot("demo.txt"),
            vec![(1, "second line could be tighter".to_string())]
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
        let host =
            crate::pair_host::PairHost::spawn_cmd(&harness.socket, "nav", None, cmd).unwrap();

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
        let deadline = Instant::now() + Duration::from_secs(10);
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
        assert!(PAIR_SYSTEM_PROMPT.contains("N|"));
        assert!(PAIR_SYSTEM_PROMPT.contains("0-based row"));
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

        let deadline = Instant::now() + Duration::from_secs(10);
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
        assert!(
            events.iter().any(|e| matches!(
                e,
                crate::pair_host::PairEvent::Commentary(c) if c.contains("suppressed")
            )),
            "suppression must be spoken; saw {events:?}"
        );
        drop(host);
    }

    /// Remote-edit spans shift every note anchored in the edited file, and
    /// only that file (the same replay the stream region does).
    #[test]
    fn note_offsets_shift_on_remote_edit() {
        let mut notes = vec![
            Note {
                file: "a.rs".into(),
                offset: 10,
                body: "n1".into(),
            },
            Note {
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
}
