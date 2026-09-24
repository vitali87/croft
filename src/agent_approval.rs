//! The croft side of agent edit approvals (#347): what an agent's proposed
//! Edit, Write or MultiEdit would do to the file, worked out before it
//! lands so it can be shown as a diff and approved or denied.

use std::collections::VecDeque;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
                }
                .answer(&reply);
            }
        }
    }
    changed
}

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
}

impl ApprovalUi {
    pub fn new(now: Instant) -> Self {
        Self {
            scroll: 0,
            reason: None,
            shown_at: now,
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
            KeyCode::Esc => {
                return Some(Decision::Deny {
                    reason: DEFAULT_DENY.into(),
                });
            }
            KeyCode::Char('r') => self.reason = Some(String::new()),
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
    let before = p.before.as_deref().unwrap_or("");
    let diff = similar::TextDiff::from_lines(before, &p.after);
    let mut rows = Vec::new();
    for group in diff.grouped_ops(3) {
        let (first, last) = (&group[0], &group[group.len() - 1]);
        rows.push((
            '@',
            format!(
                "@@ -{} +{} @@",
                first.old_range().start + 1,
                last.new_range().start + 1
            ),
        ));
        for op in &group {
            for change in diff.iter_changes(op) {
                let tag = match change.tag() {
                    similar::ChangeTag::Equal => ' ',
                    similar::ChangeTag::Delete => '-',
                    similar::ChangeTag::Insert => '+',
                };
                rows.push((tag, change.value().trim_end_matches('\n').to_string()));
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
