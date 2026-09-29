//! The croft side of agent edit approvals (#347): what an agent's proposed
//! Edit, Write or MultiEdit would do to the file, worked out before it
//! lands so it can be shown as a diff and approved or denied.

use std::collections::VecDeque;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_hook::{ANSWER_WINDOW, Decision, EditRequest};

/// One proposed change: the file, its current text (`None` for a file the
/// proposal creates), and the text it would have after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub path: PathBuf,
    pub before: Option<String>,
    pub after: String,
}

/// Apply `tool`'s `input` to the file on disk, as the agent's own tool
/// would. `read` returns the file's text, `NotFound` for a missing file.
/// An edit that would fail when the agent runs it (text not found, or
/// found more than once without `replace_all`) is an error here too.
pub fn proposal_for(
    tool: &str,
    input: &Value,
    cwd: &Path,
    read: &dyn Fn(&Path) -> std::io::Result<String>,
) -> Result<Proposal, String> {
    let file = input["file_path"]
        .as_str()
        .ok_or_else(|| format!("{tool} names no file_path"))?;
    let path = cwd.join(file);
    let before = match read(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let after = match tool {
        "Write" => input["content"]
            .as_str()
            .ok_or("Write carries no content")?
            .to_string(),
        "Edit" | "MultiEdit" => {
            let mut text = before
                .clone()
                .ok_or_else(|| format!("{} does not exist", path.display()))?;
            let edits = if tool == "Edit" {
                vec![input.clone()]
            } else {
                input["edits"]
                    .as_array()
                    .cloned()
                    .ok_or("MultiEdit carries no edits")?
            };
            for edit in &edits {
                text = apply_edit(&text, edit)?;
            }
            text
        }
        other => return Err(format!("{other} is not a file edit")),
    };
    Ok(Proposal {
        path,
        before,
        after,
    })
}

/// The tool input that makes the agent's own tool produce `edited`
/// instead of what it proposed (#347, "edit then approve"), in the shape
/// the tool already takes, so Claude Code's `updatedInput` needs nothing
/// new from it. A `Write` writes `edited`; an `Edit` or `MultiEdit`
/// becomes one replacement of the whole current file, whose text the
/// proposal was computed from and so matches exactly once. A new or empty
/// file has no text for an `Edit` to match, and is refused.
pub fn edited_input(
    tool: &str,
    input: &Value,
    before: Option<&str>,
    edited: &str,
) -> Result<Value, String> {
    let file = input["file_path"].clone();
    match tool {
        "Write" => {
            let mut out = input.clone();
            out["content"] = Value::String(edited.to_string());
            Ok(out)
        }
        "Edit" | "MultiEdit" => {
            let before = before.filter(|b| !b.is_empty()).ok_or_else(|| {
                format!("an {tool} of an empty file cannot carry an edited version; approve or deny it as proposed")
            })?;
            let one = serde_json::json!({
                "old_string": before,
                "new_string": edited,
                "replace_all": false,
            });
            Ok(if tool == "Edit" {
                let mut out = one;
                out["file_path"] = file;
                out
            } else {
                serde_json::json!({ "file_path": file, "edits": [one] })
            })
        }
        other => Err(format!("{other} is not a file edit")),
    }
}

/// One `old_string` -> `new_string` replacement, with the agent's own
/// rules: the old text must be present, and exactly once unless
/// `replace_all` is set.
fn apply_edit(text: &str, edit: &Value) -> Result<String, String> {
    let old = edit["old_string"]
        .as_str()
        .ok_or("an edit has no old_string")?;
    let new = edit["new_string"]
        .as_str()
        .ok_or("an edit has no new_string")?;
    let all = edit["replace_all"].as_bool().unwrap_or(false);
    match text.matches(old).count() {
        0 => Err(format!("{old:?} is not in the file")),
        1 => Ok(text.replacen(old, new, 1)),
        _ if all => Ok(text.replace(old, new)),
        n => Err(format!("{old:?} appears {n} times; the edit is ambiguous")),
    }
}

/// Listen for this workspace's hook requests. Non-blocking, so draining it
/// never stalls a frame.
pub fn bind(workspace: &Path) -> std::io::Result<UnixListener> {
    let root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let listener = crate::session::bind_socket_0600(&crate::session::hook_socket_path(&root))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// A proposal waiting for the user, with the connection its answer goes
/// back on.
pub struct Pending {
    stream: UnixStream,
    pub request: EditRequest,
    pub proposal: Proposal,
    pub arrived: Instant,
    /// The one-time token a notification's Approve and Deny carry (#359).
    /// It answers this proposal once, from outside the popup, and dies
    /// with it: answered either way, or expired.
    pub token: String,
    /// Whether the notification for this proposal has gone out.
    pub notified: bool,
}

impl Pending {
    /// Send `decision` to the waiting hook. A hook that has already given
    /// up (its own window passed) is simply gone; nothing is owed to it.
    pub fn answer(mut self, decision: &Decision) {
        use std::io::Write;
        if let Ok(mut line) = serde_json::to_string(decision) {
            line.push('\n');
            let _ = self.stream.write_all(line.as_bytes());
        }
    }

    /// Past the hook's own window: it has answered "ask" by itself, so the
    /// proposal is no longer the user's to decide here.
    pub fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.arrived) >= ANSWER_WINDOW
    }
}

/// How long a new connection gets to deliver its request line. The hook
/// writes it straight after connecting, so this only bounds a peer that
/// connects and says nothing.
const REQUEST_DEADLINE: Duration = Duration::from_millis(50);

/// Take every waiting connection off `listener`. Requests that make a
/// proposal are queued; one whose edit cannot be worked out is answered
/// "ask" at once, since there is nothing to show.
pub fn accept_into(listener: &UnixListener, queue: &mut VecDeque<Pending>) -> bool {
    let mut changed = false;
    while let Ok((stream, _)) = listener.accept() {
        let _ = stream.set_nonblocking(false);
        let deadline = Instant::now() + REQUEST_DEADLINE;
        let Ok(line) = crate::view_ipc::read_line_by_deadline(&stream, deadline, "the hook") else {
            continue;
        };
        if let Ok(DecideLine { decide }) = serde_json::from_str::<DecideLine>(line.trim()) {
            let reply = decide_by_token(queue, &decide, Instant::now());
            changed |= reply.ok;
            reply.send(stream);
            continue;
        }
        let Ok(request) = serde_json::from_str::<EditRequest>(line.trim()) else {
            continue;
        };
        let read = |p: &Path| std::fs::read_to_string(p);
        match proposal_for(&request.tool, &request.input, &request.cwd, &read) {
            Ok(proposal) => {
                queue.push_back(Pending {
                    stream,
                    request,
                    proposal,
                    arrived: Instant::now(),
                    token: new_token(),
                    notified: false,
                });
                changed = true;
            }
            Err(why) => {
                let reply = Decision::Ask {
                    reason: format!("croft could not preview this edit ({why})"),
                };
                Pending {
                    stream,
                    proposal: Proposal {
                        path: PathBuf::new(),
                        before: None,
                        after: String::new(),
                    },
                    request,
                    arrived: Instant::now(),
                    token: String::new(),
                    notified: true,
                }
                .answer(&reply);
            }
        }
    }
    changed
}

/// A fresh one-time token: 128 random bits as hex. Empty only if the
/// system RNG fails, and an empty token never matches anything, so such a
/// proposal is answered in the popup alone.
fn new_token() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    match ring::rand::SystemRandom::new().fill(&mut bytes) {
        Ok(()) => bytes.iter().map(|b| format!("{b:02x}")).collect(),
        Err(_) => String::new(),
    }
}

/// An answer given outside the popup (#359): `croft decide`, which a
/// notification's Approve or Deny runs, sends this line on the hook socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteDecision {
    pub token: String,
    pub allow: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct DecideLine {
    decide: RemoteDecision,
}

/// What `croft decide` hears back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecideReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

impl DecideReply {
    fn send(&self, mut stream: UnixStream) {
        use std::io::Write;
        if let Ok(mut line) = serde_json::to_string(self) {
            line.push('\n');
            let _ = stream.write_all(line.as_bytes());
        }
    }
}

/// The reason a deny from a notification carries to the agent.
pub const NOTIFICATION_DENY: &str = "denied from a croft notification";

/// Equal without an early exit, so the time a wrong guess takes says
/// nothing about how much of it was right.
fn same_token(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Answer the live proposal `decision.token` names, and take it off the
/// queue so the token cannot answer twice. A token that matches nothing
/// (already answered, expired, or never issued) answers nothing.
fn decide_by_token(
    queue: &mut VecDeque<Pending>,
    decision: &RemoteDecision,
    now: Instant,
) -> DecideReply {
    let found = queue.iter().position(|p| {
        !decision.token.is_empty() && same_token(&p.token, &decision.token) && !p.expired(now)
    });
    let Some(index) = found else {
        return DecideReply {
            ok: false,
            message: String::from(
                "no edit is waiting on that token: it was already answered, or it expired",
            ),
        };
    };
    let pending = queue.remove(index).expect("index from position");
    let file = pending.proposal.path.display().to_string();
    if decision.allow {
        pending.answer(&Decision::Allow);
    } else {
        pending.answer(&Decision::Deny {
            reason: NOTIFICATION_DENY.into(),
        });
    }
    DecideReply {
        ok: true,
        message: format!(
            "{} the edit to {file}",
            if decision.allow { "approved" } else { "denied" }
        ),
    }
}

/// `croft decide` (#359): answer the proposal `token` names, through the
/// croft serving `cwd`, without attaching to it.
pub fn send_decision(cwd: &Path, decision: &RemoteDecision) -> Result<String, String> {
    use std::io::Write;
    let mut stream = crate::agent_hook::connect_croft(cwd)
        .ok_or_else(|| format!("no croft is open for {}", cwd.display()))?;
    let mut line = serde_json::to_string(&DecideLine {
        decide: decision.clone(),
    })
    .map_err(|e| e.to_string())?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|e| format!("could not reach croft: {e}"))?;
    let deadline = Instant::now() + DECIDE_DEADLINE;
    let reply = crate::view_ipc::read_line_by_deadline(&stream, deadline, "croft")
        .map_err(|e| format!("croft did not answer: {e}"))?;
    let reply: DecideReply = serde_json::from_str(reply.trim())
        .map_err(|_| String::from("croft's answer was garbled"))?;
    if reply.ok {
        Ok(reply.message)
    } else {
        Err(reply.message)
    }
}

/// How long `croft decide` waits for the running croft, which reads the
/// hook socket once a frame.
const DECIDE_DEADLINE: Duration = Duration::from_secs(5);

/// Keys that arrive this soon after the popup appears are ignored, so an
/// Enter meant for the pane being typed in cannot approve an edit nobody
/// has looked at.
pub const ARM_DELAY: Duration = Duration::from_millis(400);

/// The reason sent with a plain deny.
pub const DEFAULT_DENY: &str = "denied in croft";

/// The popup's own state for the proposal at the head of the queue.
#[derive(Debug, Clone)]
pub struct ApprovalUi {
    pub scroll: usize,
    /// `Some` while a deny reason is being typed.
    pub reason: Option<String>,
    pub shown_at: Instant,
    /// Set by `a`: the approval also covers this agent's next proposals
    /// for [`AUTO_APPROVE_FOR`].
    pub approve_all: bool,
    /// `e` was pressed: open the proposal to edit before approving (#347).
    pub edit_requested: bool,
    /// The proposal's file is open in croft with unsaved edits, which the
    /// agent's proposal (computed from disk) knows nothing of. Approving
    /// then goes through a three-way merge instead (#347).
    pub target_dirty: bool,
}

/// How long `a` in the popup keeps approving one agent's edits (#347).
pub const AUTO_APPROVE_FOR: Duration = Duration::from_secs(10 * 60);

impl ApprovalUi {
    pub fn new(now: Instant) -> Self {
        Self {
            scroll: 0,
            reason: None,
            shown_at: now,
            approve_all: false,
            edit_requested: false,
            target_dirty: false,
        }
    }

    /// Handle one key. `Some` is the answer for the head proposal.
    pub fn key(
        &mut self,
        key: crossterm::event::KeyEvent,
        now: Instant,
        rows: usize,
    ) -> Option<Decision> {
        use crossterm::event::KeyCode;
        if now.duration_since(self.shown_at) < ARM_DELAY {
            return None;
        }
        if let Some(reason) = self.reason.as_mut() {
            match key.code {
                KeyCode::Enter => {
                    let reason = reason.trim();
                    let reason = if reason.is_empty() {
                        DEFAULT_DENY
                    } else {
                        reason
                    };
                    return Some(Decision::Deny {
                        reason: reason.to_string(),
                    });
                }
                KeyCode::Esc => self.reason = None,
                KeyCode::Backspace => {
                    reason.pop();
                }
                KeyCode::Char(c) => reason.push(c),
                _ => {}
            }
            return None;
        }
        let last = rows.saturating_sub(1);
        match key.code {
            KeyCode::Enter => return Some(Decision::Allow),
            KeyCode::Char('a') => {
                self.approve_all = true;
                return Some(Decision::Allow);
            }
            KeyCode::Esc => {
                return Some(Decision::Deny {
                    reason: DEFAULT_DENY.into(),
                });
            }
            KeyCode::Char('r') => self.reason = Some(String::new()),
            KeyCode::Char('e') => self.edit_requested = true,
            KeyCode::Down | KeyCode::Char('j') => self.scroll = (self.scroll + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll = (self.scroll + 10).min(last),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = last,
            _ => {}
        }
        None
    }
}

/// The proposal as unified-diff rows: `(' ' | '+' | '-' | '@', text)`.
pub fn diff_rows(p: &Proposal) -> Vec<(char, String)> {
    diff_rows_numbered(p)
        .into_iter()
        .map(|(tag, text, _)| (tag, text))
        .collect()
}

/// [`diff_rows`] with each row's 0-based line in the PROPOSED file, for the
/// rows that are in it (context and additions), so a diagnostic about the
/// proposal can be shown against the row it names (#347).
pub fn diff_rows_numbered(p: &Proposal) -> Vec<(char, String, Option<usize>)> {
    let before = p.before.as_deref().unwrap_or("");
    let diff = similar::TextDiff::from_lines(before, &p.after);
    let mut rows = Vec::new();
    for group in diff.grouped_ops(3) {
        let first = &group[0];
        rows.push((
            '@',
            format!(
                "@@ -{} +{} @@",
                first.old_range().start + 1,
                first.new_range().start + 1
            ),
            None,
        ));
        for op in &group {
            for change in diff.iter_changes(op) {
                let tag = match change.tag() {
                    similar::ChangeTag::Equal => ' ',
                    similar::ChangeTag::Delete => '-',
                    similar::ChangeTag::Insert => '+',
                };
                rows.push((
                    tag,
                    change.value().trim_end_matches('\n').to_string(),
                    change.new_index(),
                ));
            }
        }
    }
    rows
}

/// How long the popup says the proposal is being checked before it admits
/// no server has answered.
pub const CHECK_PATIENCE: Duration = Duration::from_secs(15);

/// The language server's verdict on the proposal at the head of the queue
/// (#347): its text is sent as the file's content, and what the servers
/// publish for that path lands here rather than in the editor, until the
/// proposal leaves the head and the file's real text goes back.
#[derive(Debug)]
pub struct ProposalCheck {
    /// Which proposal: its arrival, as the queue head is told apart.
    pub arrived: Instant,
    /// When its text went to the server.
    pub started: Instant,
    pub path: PathBuf,
    /// Whether the check opened the file with the server (no tab had it),
    /// so ending it closes the file rather than restoring a buffer.
    pub opened: bool,
    /// Each server's latest diagnostics for the proposal.
    pub by_server: std::collections::HashMap<String, Vec<crate::lsp::manager::Diagnostic>>,
}

impl ProposalCheck {
    /// Whether any server has answered yet.
    pub fn heard(&self) -> bool {
        !self.by_server.is_empty()
    }

    /// Every server's diagnostics, worst first, then by line.
    pub fn diagnostics(&self) -> Vec<crate::lsp::manager::Diagnostic> {
        let mut all: Vec<_> = self.by_server.values().flatten().cloned().collect();
        all.sort_by_key(|d| (severity_rank(d.severity), d.start_line, d.start_char));
        all
    }

    /// One line on what the servers found: nothing yet (or, after
    /// [`CHECK_PATIENCE`], that none has answered), all clear, or the counts
    /// and the first problem.
    pub fn summary(&self, now: Instant) -> String {
        use crate::lsp::manager::DiagnosticSeverity as S;
        if !self.heard() {
            return if now.duration_since(self.started) < CHECK_PATIENCE {
                String::from("Checking the proposed file with the language server…")
            } else {
                String::from("The language server has not reported on the proposed file")
            };
        }
        let all = self.diagnostics();
        let count = |s: S| all.iter().filter(|d| d.severity == s).count();
        let (errors, warnings) = (count(S::Error), count(S::Warning));
        let Some(first) = all
            .iter()
            .find(|d| matches!(d.severity, S::Error | S::Warning))
        else {
            return String::from("No problems in the proposed file");
        };
        let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
        format!(
            "{}, {} in the proposed file — line {}: {}",
            plural(errors, "error"),
            plural(warnings, "warning"),
            first.start_line + 1,
            first.message.lines().next().unwrap_or_default()
        )
    }
}

fn severity_rank(s: crate::lsp::manager::DiagnosticSeverity) -> u8 {
    use crate::lsp::manager::DiagnosticSeverity as S;
    match s {
        S::Error => 0,
        S::Warning => 1,
        S::Information => 2,
        S::Hint => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edited_proposal_becomes_input_the_tool_already_takes() {
        // #347: whatever the tool, running it with the new input yields
        // exactly the edited text: checked by replaying it through
        // `proposal_for`, the same apply the agent's tool does.
        let before = "fn a() {}\nfn b() {}\n";
        let edited = "fn a() {}\nfn c() {}\n";
        let read = |_: &Path| Ok(before.to_string());
        for (tool, input) in [
            (
                "Write",
                serde_json::json!({"file_path": "x.rs", "content": "zzz"}),
            ),
            (
                "Edit",
                serde_json::json!({"file_path": "x.rs", "old_string": "b", "new_string": "q"}),
            ),
            (
                "MultiEdit",
                serde_json::json!({"file_path": "x.rs", "edits": [{"old_string": "a", "new_string": "q"}]}),
            ),
        ] {
            let out = edited_input(tool, &input, Some(before), edited).unwrap();
            let replay = proposal_for(tool, &out, Path::new("/w"), &read).unwrap();
            assert_eq!(replay.after, edited, "{tool}: {out}");
            assert_eq!(out["file_path"], "x.rs");
        }
        let err = edited_input(
            "Edit",
            &serde_json::json!({"file_path": "x.rs"}),
            Some(""),
            edited,
        )
        .unwrap_err();
        assert!(err.contains("empty file"), "{err}");
        // A Write of a new file is fine: it carries the whole text.
        assert!(
            edited_input(
                "Write",
                &serde_json::json!({"file_path": "n.rs", "content": ""}),
                None,
                "x"
            )
            .is_ok()
        );
    }
    use serde_json::json;

    fn disk(text: &'static str) -> impl Fn(&Path) -> std::io::Result<String> {
        move |_| Ok(text.to_string())
    }

    fn missing(_: &Path) -> std::io::Result<String> {
        Err(std::io::ErrorKind::NotFound.into())
    }

    #[test]
    fn an_edit_replaces_its_one_match() {
        let input =
            json!({"file_path": "/w/a.rs", "old_string": "let x = 1;", "new_string": "let x = 2;"});
        let p = proposal_for(
            "Edit",
            &input,
            Path::new("/w"),
            &disk("fn f() {\n    let x = 1;\n}\n"),
        )
        .unwrap();
        assert_eq!(p.path, PathBuf::from("/w/a.rs"));
        assert_eq!(p.before.as_deref(), Some("fn f() {\n    let x = 1;\n}\n"));
        assert_eq!(p.after, "fn f() {\n    let x = 2;\n}\n");
    }

    #[test]
    fn an_ambiguous_or_missing_match_is_refused_unless_replace_all() {
        let text = disk("a b a\n");
        let one = json!({"file_path": "/w/a", "old_string": "a", "new_string": "c"});
        assert!(proposal_for("Edit", &one, Path::new("/w"), &text).is_err());
        let all =
            json!({"file_path": "/w/a", "old_string": "a", "new_string": "c", "replace_all": true});
        assert_eq!(
            proposal_for("Edit", &all, Path::new("/w"), &text)
                .unwrap()
                .after,
            "c b c\n"
        );
        let absent = json!({"file_path": "/w/a", "old_string": "z", "new_string": "c"});
        assert!(proposal_for("Edit", &absent, Path::new("/w"), &text).is_err());
    }

    #[test]
    fn a_write_creates_or_replaces_and_a_relative_path_is_under_cwd() {
        let input = json!({"file_path": "new.txt", "content": "hi\n"});
        let p = proposal_for("Write", &input, Path::new("/w"), &missing).unwrap();
        assert_eq!(p.path, PathBuf::from("/w/new.txt"));
        assert_eq!(p.before, None);
        assert_eq!(p.after, "hi\n");
        let p = proposal_for("Write", &input, Path::new("/w"), &disk("old\n")).unwrap();
        assert_eq!(p.before.as_deref(), Some("old\n"));
    }

    #[test]
    fn a_multi_edit_applies_its_edits_in_order() {
        let input = json!({"file_path": "/w/a", "edits": [
            {"old_string": "one", "new_string": "two"},
            {"old_string": "two two", "new_string": "three"},
        ]});
        let p = proposal_for("MultiEdit", &input, Path::new("/w"), &disk("one two\n")).unwrap();
        assert_eq!(p.after, "three\n");
        let bad =
            json!({"file_path": "/w/a", "edits": [{"old_string": "nope", "new_string": "x"}]});
        assert!(proposal_for("MultiEdit", &bad, Path::new("/w"), &disk("one two\n")).is_err());
    }

    #[test]
    fn an_edit_of_a_missing_file_or_an_unknown_tool_is_refused() {
        let input = json!({"file_path": "/w/a", "old_string": "a", "new_string": "b"});
        assert!(proposal_for("Edit", &input, Path::new("/w"), &missing).is_err());
        assert!(
            proposal_for("Bash", &json!({"command": "ls"}), Path::new("/w"), &missing).is_err()
        );
        assert!(
            proposal_for(
                "Edit",
                &json!({"old_string": "a"}),
                Path::new("/w"),
                &missing
            )
            .is_err()
        );
    }

    fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    #[test]
    fn keys_before_the_popup_is_armed_do_nothing() {
        use crossterm::event::KeyCode;
        let t0 = Instant::now();
        let mut ui = ApprovalUi::new(t0);
        assert_eq!(
            ui.key(key(KeyCode::Enter), t0 + Duration::from_millis(50), 10),
            None
        );
        assert_eq!(
            ui.key(key(KeyCode::Enter), t0 + ARM_DELAY, 10),
            Some(Decision::Allow)
        );
    }

    #[test]
    fn deny_sends_the_typed_reason_or_the_default() {
        use crossterm::event::KeyCode;
        let t = Instant::now() + ARM_DELAY * 2;
        let mut ui = ApprovalUi::new(Instant::now());
        assert_eq!(
            ui.key(key(KeyCode::Esc), t, 10),
            Some(Decision::Deny {
                reason: DEFAULT_DENY.into()
            })
        );
        let mut ui = ApprovalUi::new(Instant::now());
        assert_eq!(ui.key(key(KeyCode::Char('r')), t, 10), None);
        for c in "use a helperx".chars() {
            ui.key(key(KeyCode::Char(c)), t, 10);
        }
        ui.key(key(KeyCode::Backspace), t, 10);
        // Enter while typing sends the reason; it does not approve.
        assert_eq!(
            ui.key(key(KeyCode::Enter), t, 10),
            Some(Decision::Deny {
                reason: "use a helper".into()
            })
        );
        // Esc while typing backs out to the diff instead of denying.
        let mut ui = ApprovalUi::new(Instant::now());
        ui.key(key(KeyCode::Char('r')), t, 10);
        assert_eq!(ui.key(key(KeyCode::Esc), t, 10), None);
        assert!(ui.reason.is_none());
    }

    #[test]
    fn scrolling_stays_within_the_diff() {
        use crossterm::event::KeyCode;
        let t = Instant::now() + ARM_DELAY * 2;
        let mut ui = ApprovalUi::new(Instant::now());
        ui.key(key(KeyCode::PageDown), t, 4);
        assert_eq!(ui.scroll, 3);
        ui.key(key(KeyCode::Up), t, 4);
        assert_eq!(ui.scroll, 2);
        ui.key(key(KeyCode::Home), t, 4);
        assert_eq!(ui.scroll, 0);
    }

    #[test]
    fn diff_rows_mark_removed_and_added_lines() {
        let p = Proposal {
            path: "/w/a".into(),
            before: Some("a\nb\nc\n".into()),
            after: "a\nB\nc\n".into(),
        };
        let rows = diff_rows(&p);
        assert_eq!(rows[0].0, '@');
        assert!(rows.contains(&('-', "b".into())));
        assert!(rows.contains(&('+', "B".into())));
        assert!(rows.contains(&(' ', "a".into())));
    }

    #[test]
    fn a_request_is_queued_and_its_answer_reaches_the_hook() {
        use std::io::{BufRead, Write};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.rs");
        std::fs::write(&file, "let x = 1;\n").unwrap();
        let sock = dir.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut queue = VecDeque::new();
        assert!(!accept_into(&listener, &mut queue), "nothing waiting yet");

        let mut hook = UnixStream::connect(&sock).unwrap();
        let req = EditRequest {
            agent: "claude-code".into(),
            tool: "Edit".into(),
            input: serde_json::json!({"file_path": file, "old_string": "1", "new_string": "2"}),
            cwd: dir.path().into(),
        };
        writeln!(hook, "{}", serde_json::to_string(&req).unwrap()).unwrap();
        assert!(accept_into(&listener, &mut queue));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].proposal.after, "let x = 2;\n");
        // The hook stops waiting after its own window; so does the queue.
        let now = Instant::now();
        assert!(!queue[0].expired(now));
        queue[0].arrived = now - ANSWER_WINDOW;
        assert!(queue[0].expired(now));
        queue[0].arrived = now;

        queue.pop_front().unwrap().answer(&Decision::Deny {
            reason: "no".into(),
        });
        let mut line = String::new();
        std::io::BufReader::new(hook).read_line(&mut line).unwrap();
        let got: Decision = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(
            got,
            Decision::Deny {
                reason: "no".into()
            }
        );
    }

    #[test]
    fn an_edit_that_cannot_be_previewed_is_answered_ask_at_once() {
        use std::io::{BufRead, Write};
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut hook = UnixStream::connect(&sock).unwrap();
        let req = EditRequest {
            agent: "claude-code".into(),
            tool: "Edit".into(),
            input: serde_json::json!({"file_path": "missing.rs", "old_string": "a", "new_string": "b"}),
            cwd: dir.path().into(),
        };
        writeln!(hook, "{}", serde_json::to_string(&req).unwrap()).unwrap();
        let mut queue = VecDeque::new();
        assert!(!accept_into(&listener, &mut queue));
        assert!(queue.is_empty());
        let mut line = String::new();
        std::io::BufReader::new(hook).read_line(&mut line).unwrap();
        assert!(matches!(
            serde_json::from_str(line.trim()).unwrap(),
            Decision::Ask { .. }
        ));
    }

    /// Queue one proposal for `file` through `listener`, as a hook would,
    /// and return the hook's end of the connection.
    fn queue_one(
        listener: &UnixListener,
        sock: &Path,
        dir: &Path,
        queue: &mut VecDeque<Pending>,
    ) -> UnixStream {
        use std::io::Write;
        let file = dir.join(format!("f{}.rs", queue.len()));
        std::fs::write(&file, "let x = 1;\n").unwrap();
        let mut hook = UnixStream::connect(sock).unwrap();
        let req = EditRequest {
            agent: "claude-code".into(),
            tool: "Edit".into(),
            input: serde_json::json!({"file_path": file, "old_string": "1", "new_string": "2"}),
            cwd: dir.into(),
        };
        writeln!(hook, "{}", serde_json::to_string(&req).unwrap()).unwrap();
        assert!(accept_into(listener, queue));
        hook
    }

    /// Send `decide` on the socket as `croft decide` does, let the app's
    /// pass pick it up, and return the reply `croft decide` would print.
    fn decide(
        listener: &UnixListener,
        sock: &Path,
        queue: &mut VecDeque<Pending>,
        token: &str,
        allow: bool,
    ) -> DecideReply {
        use std::io::{BufRead, Write};
        let mut client = UnixStream::connect(sock).unwrap();
        let line = serde_json::to_string(&DecideLine {
            decide: RemoteDecision {
                token: token.into(),
                allow,
            },
        })
        .unwrap();
        writeln!(client, "{line}").unwrap();
        accept_into(listener, queue);
        let mut reply = String::new();
        std::io::BufReader::new(client)
            .read_line(&mut reply)
            .unwrap();
        serde_json::from_str(reply.trim()).unwrap()
    }

    fn hook_answer(hook: UnixStream) -> Decision {
        use std::io::BufRead;
        let mut line = String::new();
        std::io::BufReader::new(hook).read_line(&mut line).unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    #[test]
    fn each_proposal_gets_its_own_token_and_starts_unnotified() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut queue = VecDeque::new();
        let _a = queue_one(&listener, &sock, dir.path(), &mut queue);
        let _b = queue_one(&listener, &sock, dir.path(), &mut queue);
        assert_eq!(queue[0].token.len(), 32);
        assert!(queue[0].token.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(queue[0].token, queue[1].token);
        assert!(!queue[0].notified && !queue[1].notified);
    }

    #[test]
    fn a_token_answers_its_own_proposal_once_from_outside_the_popup() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut queue = VecDeque::new();
        let first = queue_one(&listener, &sock, dir.path(), &mut queue);
        let second = queue_one(&listener, &sock, dir.path(), &mut queue);
        let (t1, t2) = (queue[0].token.clone(), queue[1].token.clone());

        // The second proposal, not the head: a token names its own.
        let reply = decide(&listener, &sock, &mut queue, &t2, true);
        assert!(reply.ok, "{reply:?}");
        assert!(reply.message.starts_with("approved the edit to "));
        assert_eq!(hook_answer(second), Decision::Allow);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].token, t1);

        // Single use: the same token again answers nothing.
        let again = decide(&listener, &sock, &mut queue, &t2, false);
        assert!(!again.ok);
        assert!(again.message.contains("already answered, or it expired"));
        assert_eq!(queue.len(), 1);

        let deny = decide(&listener, &sock, &mut queue, &t1, false);
        assert!(deny.ok);
        assert_eq!(
            hook_answer(first),
            Decision::Deny {
                reason: NOTIFICATION_DENY.into()
            }
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn a_wrong_empty_or_expired_token_answers_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut queue = VecDeque::new();
        let _hook = queue_one(&listener, &sock, dir.path(), &mut queue);
        let token = queue[0].token.clone();
        let mut wrong = token.clone();
        wrong.replace_range(..1, if token.starts_with('0') { "1" } else { "0" });
        assert!(!decide(&listener, &sock, &mut queue, &wrong, true).ok);
        assert!(!decide(&listener, &sock, &mut queue, "", true).ok);
        // A proposal with no token (the RNG failed) is not matched by an
        // empty one either.
        queue[0].token.clear();
        assert!(!decide(&listener, &sock, &mut queue, "", true).ok);
        queue[0].token = token.clone();
        // It expires with the proposal: past the hook's window, the hook
        // has answered by itself.
        queue[0].arrived = Instant::now() - ANSWER_WINDOW;
        assert!(!decide(&listener, &sock, &mut queue, &token, true).ok);
        assert_eq!(queue.len(), 1, "the app's own pass drops the expired one");
    }

    /// #347: each diff row knows its line in the PROPOSED file, which is
    /// what a server's diagnostic about the proposal names.
    #[test]
    fn diff_rows_carry_their_line_in_the_proposed_file() {
        let p = Proposal {
            path: PathBuf::from("a.py"),
            before: Some(String::from("a\nb\nc\n")),
            after: String::from("a\nB\nc\nd\n"),
        };
        let rows = diff_rows_numbered(&p);
        let numbered: Vec<(char, &str, Option<usize>)> =
            rows.iter().map(|(t, s, n)| (*t, s.as_str(), *n)).collect();
        assert_eq!(
            numbered,
            [
                ('@', "@@ -1 +1 @@", None),
                (' ', "a", Some(0)),
                ('-', "b", None),
                ('+', "B", Some(1)),
                (' ', "c", Some(2)),
                ('+', "d", Some(3)),
            ]
        );
        assert_eq!(diff_rows(&p).len(), rows.len());
    }

    /// #347: the check's one-line verdict: waiting, clear, or the counts
    /// and the worst problem first.
    #[test]
    fn a_proposal_check_sums_up_what_the_servers_found() {
        use crate::lsp::manager::{Diagnostic, DiagnosticSeverity as S};
        let d = |line: u32, severity: S, message: &str| Diagnostic {
            start_line: line,
            start_char: 0,
            end_line: line,
            end_char: 1,
            severity,
            message: message.into(),
        };
        let now = Instant::now();
        let mut check = ProposalCheck {
            arrived: now,
            started: now,
            path: PathBuf::from("a.py"),
            opened: true,
            by_server: Default::default(),
        };
        assert!(check.summary(now).starts_with("Checking"));
        assert!(
            check
                .summary(now + CHECK_PATIENCE)
                .contains("has not reported")
        );
        check
            .by_server
            .insert("ruff".into(), vec![d(0, S::Hint, "style")]);
        assert_eq!(check.summary(now), "No problems in the proposed file");
        check.by_server.insert(
            "ruff".into(),
            vec![d(4, S::Warning, "unused"), d(2, S::Error, "nope\nmore")],
        );
        check
            .by_server
            .insert("pyright".into(), vec![d(9, S::Error, "bad type")]);
        assert_eq!(
            check.summary(now),
            "2 errors, 1 warning in the proposed file — line 3: nope"
        );
        assert_eq!(
            check.diagnostics()[0].message,
            "nope\nmore",
            "worst, then first"
        );
    }
}
