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

mod apply;
mod caret;
mod claude_cli;
mod fence;
pub(crate) mod local;
mod notes;
mod offsets;
pub(crate) mod proactive;
mod provider;
mod seat;
mod state;
mod voice;
use apply::*;
#[cfg(test)]
pub(crate) use claude_cli::FAKE_CLAUDE;
pub(crate) use claude_cli::claude_command;
use claude_cli::*;
use fence::*;
#[cfg(test)]
pub(crate) use notes::inject_note;
pub(crate) use notes::inject_note_now;
use notes::*;
use offsets::*;
pub use provider::PairConfig;
pub use provider::Provider;
use provider::*;
pub(crate) use seat::Pilot;
pub(crate) use seat::seat_local;
pub(crate) use seat::seat_pilot;
use seat::*;
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

#[cfg(test)]
mod tests;
