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

pub(crate) mod local;
pub(crate) mod proactive;

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
                    let mut tail = std::mem::take(&mut self.buf);
                    // Only the unterminated LAST line can be a fence header.
                    // A VALID one is protocol residue (a fence the turn cut
                    // off before its newline) and is dropped like an
                    // unterminated body; malformed ones stay commentary,
                    // the malformed-header rule.
                    let last_start = tail.rfind('\n').map_or(0, |i| i + 1);
                    let line = tail[last_start..].trim_end_matches('\r');
                    if parse_header(line).is_some() || parse_note_header(line).is_some() {
                        tail.truncate(last_start);
                    }
                    if !tail.is_empty() {
                        events.push(FenceEvent::Commentary(tail));
                    }
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

/// Parse an EDIT header into its [`FenceEvent::EditStart`]. Two forms:
/// four integers `<file>:SR:SC-ER:EC` (character range), or two integers
/// `<file>:SR-ER` (whole rows, inclusive — start column 0, end column
/// `usize::MAX`, which [`crate::collab::byte_offset`] clamps to the end of
/// row ER). Coordinates bind rightmost so the file name may itself contain
/// ':' or '-'. None = not a well-formed header (the line degrades to
/// commentary).
fn parse_header(line: &str) -> Option<FenceEvent> {
    let inner = line.strip_prefix(EDIT_MARKER)?.strip_suffix(">>>")?;
    parse_char_range_header(inner).or_else(|| parse_whole_line_header(inner))
}

/// The four-int form: `<file>:SR:SC-ER:EC`.
fn parse_char_range_header(inner: &str) -> Option<FenceEvent> {
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

/// The two-int whole-line form: `<file>:SR-ER`, both rows inclusive. A file
/// part that itself ends in `:<digits>` is a TRUNCATED four-int header
/// (`file:SR:SC-ER` missing its end column), not a path — rejected so it
/// degrades to commentary instead of editing a bogus file.
fn parse_whole_line_header(inner: &str) -> Option<FenceEvent> {
    let (file, range) = inner.rsplit_once(':')?;
    let (sr, er) = range.split_once('-')?;
    if file.is_empty() {
        return None;
    }
    if let Some((_, tail)) = file.rsplit_once(':')
        && !tail.is_empty()
        && tail.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(FenceEvent::EditStart {
        file: file.to_string(),
        start: (sr.parse().ok()?, 0),
        end: (er.parse().ok()?, usize::MAX),
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

/// Taught to the model via --append-system-prompt: the fence protocol whose
/// streamed body croft applies token by token.
const PAIR_SYSTEM_PROMPT: &str = r#"You are pair-programming INSIDE the croft editor: your streamed text is parsed live and fenced edits are applied to the shared buffer token by token, so your human partner watches the edit appear as you write it.

To edit a file, emit an edit fence directly in your text output:

<<<EDIT <file>:<start_row>:<start_col>-<end_row>:<end_col>>>>
<replacement text>
<<<END>>>

Rules:
- The header carries the file plus EXACTLY FOUR integers: START_ROW:START_COL-END_ROW:END_COL. Never omit the rows — for a single-line edit on row N the header reads <file>:N:C1-N:C2 (e.g. replacing columns 15..19 of row 0 of demo.txt, your partner's gutter line 1, is `<<<EDIT demo.txt:0:15-0:19>>>`, NOT `demo.txt:15:19`). A header without all four numbers is ignored as plain text and your edit does not happen.
- <file> is the workspace-relative path. Coordinates are 0-based CHARACTER positions (not bytes) into the file's CURRENT text: the range start..end (start inclusive, end exclusive) is deleted and the fence body replaces it. To insert without deleting, use a zero-width range (start == end).
- Compute coordinates against the buffer text in the latest user message (the --- CURRENT BUFFER --- block) or from the mcp__croft-collab__collab_read tool. If you emit several fences in one reply, later fences must use coordinates that account for your earlier fences' changes.
- The header and <<<END>>> each sit alone on their own line. The body between them is applied verbatim: real code only, no markdown fences, no commentary.
- Everything outside a fence is commentary shown to your partner in the terminal; it is never applied to any buffer.
- Start the fence as soon as you know the edit; stream the body naturally rather than planning silently.

Example (replace rows 3-4 of src/lib.rs, gutter lines 4-5, where row 4 is 12 chars long):
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
- The header carries the file plus EXACTLY ONE integer, the 0-based row the note speaks about (e.g. `<<<NOTE demo.txt:4>>>` pins a remark to row 4 of demo.txt, gutter line 5). Your partner sees it attached to that line in their editor.
- Notes never change any buffer. Use them for review remarks, questions, and change proposals.

Some user messages number the buffer: each line arrives prefixed with `N|` where N is its 0-based row. The prefix is a label, not content — character columns still count from the first character AFTER the first `|`, and NOTE/EDIT rows use exactly those N values.
Your partner's gutter numbers lines from 1, so row N is line N+1 to them. Whenever you mention a line in words they read (a note body or your commentary), use that 1-based gutter number. Fence headers and `N|` prefixes stay 0-based.

Turn kinds:
- An ask turn names a task and where you were invoked: you may edit with EDIT fences and remark with NOTE fences.
- A yielded turn says it is COMMENT-ONLY: your partner handed you the floor to review, not to type. EDIT fences on such a turn are DISCARDED by the editor. Speak in NOTE fences anchored to the lines you mean, propose changes there, and wait to be asked before editing.
- A reply turn answers one of your notes with your partner's own words. If the reply asks for a change ("correct this", "do it"), that is the ask: make the change with EDIT fences. If it asks a question or disagrees, answer with NOTE fences.

A participant can cancel your stream mid-edit; the streamed text is then reverted and your next user message starts with a note saying so. When that happens, stop that approach and ask what they want instead."#;

/// Appended to [`PAIR_SYSTEM_PROMPT`] for local models only: character
/// columns trip weaker models (the spike's under-covered range), so they are
/// steered to the whole-line header form instead.
const LOCAL_MODEL_PROMPT_SUFFIX: &str = r#"Local-model addendum:
- PREFER the whole-line EDIT header form: <<<EDIT <file>:<start_row>-<end_row>>>> with exactly TWO integers. It replaces those entire lines — both rows 0-based, both INCLUSIVE — with the fence body, so you never count characters. Replacing row 1 (gutter line 2) whole is `<<<EDIT greet.py:1-1>>>`.
- Rewrite whole lines instead of splicing inside a line; use the four-integer form only when you must keep part of a line.
- You have no tools: everything you need is in the message. Keep replies short — the fences plus at most a couple of commentary lines."#;

/// Which conversation transport seats the navigator: the claude CLI child
/// (the full agent) or a direct Anthropic-compatible `/v1/messages` endpoint
/// (Ollama, LM Studio, llama.cpp, vLLM — the minimal payload those can
/// actually serve; see docs/MULTIPLAYER.md).
#[derive(Clone, Debug, PartialEq)]
pub enum Provider {
    Claude,
    Local { base_url: String },
}

impl Provider {
    /// Map a pair record's stringly provider to the transport it names.
    /// Absent or unrecognized = Claude (every 0.1.635 record is claude).
    pub fn from_record(provider: Option<&str>, base_url: Option<&str>) -> Self {
        match provider {
            Some("ollama") => Provider::Local {
                base_url: base_url.unwrap_or("http://localhost:11434").to_string(),
            },
            _ => Provider::Claude,
        }
    }
}

/// The system prompt a provider's model is taught: the shared fence
/// protocol, plus the whole-line guidance for local models.
pub(crate) fn system_prompt(provider: &Provider) -> std::borrow::Cow<'static, str> {
    match provider {
        Provider::Claude => std::borrow::Cow::Borrowed(PAIR_SYSTEM_PROMPT),
        Provider::Local { .. } => std::borrow::Cow::Owned(format!(
            "{PAIR_SYSTEM_PROMPT}\n\n{LOCAL_MODEL_PROMPT_SUFFIX}"
        )),
    }
}

/// Everything `croft pair` needs to sit down: the relay socket, the
/// workspace (cwd and MCP reader seat), the caret name, and the claude
/// launch knobs.
pub struct PairConfig {
    pub socket: PathBuf,
    pub workspace: PathBuf,
    pub name: String,
    pub model: Option<String>,
    pub task: Option<String>,
    pub provider: Provider,
}

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

/// What [`PairState::stage_turn`] overwrote, so a send that never leaves
/// the process can be undone ([`PairState::unstage_turn`]).
pub(crate) struct StagedTurn {
    file: String,
    /// The previous look at `file`; also the yield-diff baseline the
    /// caller reads before sending.
    pub(crate) prior_seen: Option<String>,
    prior_comment_only: bool,
    prior_target: Option<String>,
    /// Which staging this undo belongs to: a rollback that lands after a
    /// NEWER turn has staged (its send failed while the pilot mutex was
    /// wedged, so the unstage waited on a thread) is stale and must not
    /// stomp the live turn's books.
    stage_gen: u64,
}

/// One anchored navigator note: a byte offset into `file`'s replica text
/// (kept fresh against concurrent edits exactly like the stream region)
/// plus the accumulated body.
pub struct Note {
    /// Stable identity for the note's comment box: Ignore removes exactly
    /// this note, a reply appends to exactly this note. Monotonic per seat.
    pub id: u64,
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

impl PairState {
    /// A fresh seat over `session`: no turn in flight, nothing streamed yet.
    /// `events` seats the pilot inside croft (its voice becomes
    /// [`crate::pair_host::PairEvent`]s); None keeps stdio.
    pub(crate) fn new(
        session: CollabSession,
        events: Option<Sender<crate::pair_host::PairEvent>>,
    ) -> Self {
        Self {
            session,
            region: None,
            discarding: false,
            cancelled: false,
            can_interrupt: false,
            turn_active: false,
            target_file: None,
            pending_note: None,
            stage_gen: 0,
            notes: Vec::new(),
            next_note_id: 1,
            note_in_flight: None,
            events,
            comment_only: false,
            pending_caret: None,
            last_seen: std::collections::HashMap::new(),
        }
    }

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

    /// Drop every note in every file (the user asked for a clean slate).
    pub(crate) fn clear_all_notes(&mut self) {
        self.notes.clear();
    }

    /// Drop exactly one note (the driver ignored its comment box).
    pub(crate) fn remove_note(&mut self, id: u64) {
        self.notes.retain(|n| n.id != id);
    }

    /// Append one line to a note's body (the driver replied in its box, so
    /// the box keeps the running conversation).
    pub(crate) fn append_to_note(&mut self, id: u64, line: &str) {
        if let Some(n) = self.notes.iter_mut().find(|n| n.id == id) {
            n.body.push('\n');
            n.body.push_str(line);
        }
    }

    /// The next note id (monotonic within the seat).
    fn take_note_id(&mut self) -> u64 {
        let id = self.next_note_id;
        self.next_note_id += 1;
        id
    }

    /// Arm a host turn targeting `file`: mode, target, and the last-seen
    /// bookkeeping the yield diff reads. Returns what the navigator
    /// previously saw of the file (None = first look).
    pub(crate) fn begin_turn(
        &mut self,
        file: &str,
        content: &str,
        comment_only: bool,
    ) -> Option<String> {
        self.comment_only = comment_only;
        self.target_file = Some(file.to_string());
        self.session.request_file(file);
        // Notes deliberately survive the new turn: they are open comment
        // boxes, and only the driver closes them (Ignore / Clear All).
        self.last_seen.insert(file.to_string(), content.to_string())
    }

    /// [`Self::begin_turn`] plus everything needed to undo it: the caller
    /// stages the look BEFORE the send, and a failed send must not keep it
    /// (`last_seen` is the yield-diff baseline — a retry would diff the
    /// file against itself — and a latched `comment_only` would silently
    /// edit-suppress the next successful turn).
    pub(crate) fn stage_turn(
        &mut self,
        file: &str,
        content: &str,
        comment_only: bool,
    ) -> StagedTurn {
        let prior_comment_only = self.comment_only;
        let prior_target = self.target_file.clone();
        let prior_seen = self.begin_turn(file, content, comment_only);
        self.stage_gen += 1;
        StagedTurn {
            file: file.to_string(),
            prior_seen,
            prior_comment_only,
            prior_target,
            stage_gen: self.stage_gen,
        }
    }

    /// Put back what [`Self::stage_turn`] overwrote (the send never left).
    /// A no-op when a NEWER turn has staged since: this rollback ran late
    /// (see [`StagedTurn::stage_gen`]) and its books are stale.
    pub(crate) fn unstage_turn(&mut self, staged: StagedTurn) {
        if self.stage_gen != staged.stage_gen {
            return;
        }
        // An applied undo re-opens the generation beneath it, so undo
        // records unwind LIFO all the way back to the original state
        // (turn A fails parked, turn B stages over the residue and fails:
        // B's undo restores A's books AND re-admits A's undo).
        self.stage_gen = staged.stage_gen - 1;
        self.comment_only = staged.prior_comment_only;
        self.target_file = staged.prior_target;
        match staged.prior_seen {
            Some(old) => {
                self.last_seen.insert(staged.file, old);
            }
            None => {
                self.last_seen.remove(&staged.file);
            }
        }
    }

    /// Park the navigator's visible caret at `row` (0-based, column 0):
    /// broadcast now when the file is live, else once its bootstrap lands
    /// (the pump resolves it). A newer park supersedes an unresolved one.
    pub(crate) fn park_caret(&mut self, file: &str, row: usize) {
        self.pending_caret = (!self.send_parked_caret(file, row)).then(|| (file.to_string(), row));
    }

    /// Broadcast the caret at an exact position and drop any unresolved
    /// pending park — a streamed edit or a landed note is newer attention,
    /// and a stale park firing afterwards would yank the caret back.
    fn caret_now(&mut self, file: &str, row: usize, col: usize) {
        self.pending_caret = None;
        self.session.send_caret(file, row, col);
    }

    /// This seat's site id in every live file: the navigator's wire
    /// identity (the App keys caret color and unseat cleanup off these).
    pub(crate) fn my_site_ids(&self) -> Vec<u64> {
        self.session.my_site_ids()
    }

    /// What the navigator last saw of `file` (None = it never looked).
    /// The proactive trigger diffs the live buffer against this.
    pub(crate) fn last_seen_of(&self, file: &str) -> Option<String> {
        self.last_seen.get(file).cloned()
    }

    /// Broadcast the parked caret when `file` is live, the row clamped to
    /// the document's last line. False = not live yet.
    fn send_parked_caret(&mut self, file: &str, row: usize) -> bool {
        let Some(lines) = self.doc_lines(file) else {
            return false;
        };
        let row = row.min(lines.len().saturating_sub(1));
        self.session.send_caret(file, row, 0);
        true
    }

    /// Resolve a pending caret park (one pump tick): broadcast it if the
    /// file went live, drop it if the bootstrap died unanswered.
    fn resolve_pending_caret(&mut self) {
        let Some((file, row)) = self.pending_caret.clone() else {
            return;
        };
        if self.send_parked_caret(&file, row) || !self.session.is_bootstrapping(&file) {
            self.pending_caret = None;
        }
    }
}

/// Bounded lock for anything that DECIDES or ACTS on the pilot's state
/// from the App's threads (is a turn running? send this ask; land this
/// note): transient contention is normal — the pilot's pump holds the
/// mutex for microseconds every 25ms poll — so an instant try_lock
/// spuriously refuses keypresses and misreports a quiet seat as busy. But
/// an unbounded lock joins the deadlock cycle when a pilot thread is
/// wedged in a blocking relay write. Retry briefly, then give up.
/// Per-frame render reads never wait at all (see `PairHost::caret_sites`):
/// they have a next frame, a decision does not.
pub(crate) fn lock_briefly(
    state: &Mutex<PairState>,
) -> Option<std::sync::MutexGuard<'_, PairState>> {
    let deadline = std::time::Instant::now() + Duration::from_millis(100);
    loop {
        if let Ok(guard) = state.try_lock() {
            return Some(guard);
        }
        if std::time::Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Host and child DIAGNOSTICS, never the model's voice. Seated in croft
/// they go to the OUTPUT channel: [`say`]'s Commentary events are reserved
/// for the model's own prose, which the App anchors as a navigator comment
/// box — a diagnostic riding that channel was rendered as the navigator's
/// remark in the user's file and counted as one of its comments. The REPL
/// driver prints to stdout, its only voice.
fn diag(state: &Mutex<PairState>, level: crate::output::OutputLevel, text: &str) {
    // Bounded (see lock_briefly): diagnostics also fire from paths a
    // wedged pilot would block. Unknown seating falls back to OUTPUT — the
    // right sink when seated, and for the REPL a diagnostic in OUTPUT is
    // the lesser loss (stdout is unreachable without the lock, and stderr
    // would corrupt a seated alternate screen).
    let seated = lock_briefly(state).is_none_or(|st| st.events.is_some());
    if seated {
        crate::output::push("Navigator", level, text);
    } else {
        println!("{text}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
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

/// Anchor a note into `file` at `row` from the app side (commentary landing
/// as a comment box). Mirrors the NoteEnd path: request the file, give the
/// bootstrap the same bounded wait, then anchor by byte offset. None = the
/// owner never served the file.
/// Test-only: the e2e suite anchors seed notes right after a spawn, before
/// the bootstrap settles; production (the App tick thread) must never block
/// and uses [`inject_note_now`].
#[cfg(test)]
pub(crate) fn inject_note(
    state: &Mutex<PairState>,
    file: &str,
    row: usize,
    body: &str,
) -> Option<u64> {
    state.lock().unwrap().session.request_file(file);
    if !wait_live(state, file) {
        return None;
    }
    inject_note_now(state, file, row, body)
}

/// Non-blocking [`inject_note`]: anchors only when `file` is already live.
/// The App's tick thread lands turn commentary through this — it must never
/// sit out a bootstrap wait.
pub(crate) fn inject_note_now(
    state: &Mutex<PairState>,
    file: &str,
    row: usize,
    body: &str,
) -> Option<u64> {
    // Bounded lock (see lock_briefly): the App's tick thread must never
    // wait indefinitely on the pilot mutex (held across blocking relay
    // writes). None sends the caller to its OUTPUT fallback.
    let mut st = lock_briefly(state)?;
    if !st.session.is_live(file) {
        return None;
    }
    let lines = st.doc_lines(file)?;
    let offset = note_offset(&lines, row);
    // The AI is visibly "looking" where it just commented.
    st.caret_now(file, position(&lines, offset).0, 0);
    let id = st.take_note_id();
    st.notes.push(Note {
        id,
        file: file.to_string(),
        offset,
        body: body.to_string(),
    });
    Some(id)
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
