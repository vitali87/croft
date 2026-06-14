//! A single debug session: the lifecycle state machine over a [`DapTransport`].
//!
//! DAP is event-driven, so the session is too. The app calls [`DapSession::poll`]
//! once per frame; it drains every message the adapter has sent, advances the
//! handshake (`initialize` → on the `initialized` event push breakpoints +
//! `configurationDone` → run), and returns the user-facing [`DapEvent`]s the app
//! reacts to (stop-line highlight, output, teardown).
//!
//! The wire bodies are built and read as `serde_json::Value` rather than a large
//! vendored type set: the message surface croft drives is small, and the request
//! builders + the event classifier — the parts worth locking down — are pure and
//! unit-tested below.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};

use super::transport::DapTransport;

/// Where a session is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    /// `initialize` sent, awaiting the `initialized` event.
    Initializing,
    /// Breakpoints pushed + `configurationDone` sent; debuggee running.
    Running,
    /// Stopped at a breakpoint / step / exception.
    Stopped,
    /// Adapter reported the debuggee exited or the session ended.
    Terminated,
}

/// A user-facing event distilled from a raw DAP message, for the app to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DapEvent {
    /// Adapter is ready for configuration (breakpoints). Handled internally,
    /// surfaced for visibility/tests.
    Initialized,
    /// Execution stopped; `thread_id` is the stopped thread, `reason` e.g.
    /// `breakpoint` / `step` / `exception`.
    Stopped { thread_id: i64, reason: String },
    /// Execution resumed.
    Continued,
    /// Program / debug console output.
    Output { category: String, text: String },
    /// The debuggee process exited.
    Terminated,
    /// The adapter reported updated breakpoint verification (from a
    /// `setBreakpoints` response or a `breakpoint` event). The app refreshes the
    /// gutter so unverified breakpoints render hollow.
    BreakpointsUpdated,
}

/// One breakpoint's binding status as reported by the adapter: the source file,
/// the (possibly adjusted) 1-based line, and whether the adapter could actually
/// place it. An unverified breakpoint never pauses execution, so surfacing it is
/// what stops the "I set a breakpoint and it just ran" confusion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakpointReport {
    pub path: PathBuf,
    pub line: usize,
    pub verified: bool,
}

/// Extract breakpoint statuses from either a `setBreakpoints` response
/// (`body.breakpoints[]`) or a `breakpoint` event (`body.breakpoint`). Entries
/// without a source path or line are skipped (debugpy occasionally reports
/// `line: 0` for a not-yet-resolved breakpoint). Pure.
pub fn breakpoint_reports(msg: &Value) -> Vec<BreakpointReport> {
    let body = match msg.get("body") {
        Some(b) => b,
        None => return Vec::new(),
    };
    let mut raw: Vec<&Value> = Vec::new();
    if let Some(arr) = body.get("breakpoints").and_then(Value::as_array) {
        raw.extend(arr.iter());
    }
    if let Some(one) = body.get("breakpoint") {
        raw.push(one);
    }
    raw.into_iter()
        .filter_map(|bp| {
            let path = bp.get("source")?.get("path")?.as_str()?;
            let line = bp.get("line").and_then(Value::as_i64).unwrap_or(0);
            if line <= 0 {
                return None;
            }
            Some(BreakpointReport {
                path: PathBuf::from(path),
                line: line as usize,
                verified: bp
                    .get("verified")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

/// Classify a raw incoming DAP message into a [`DapEvent`], or `None` for
/// messages the app doesn't surface (responses, bookkeeping events). Pure.
pub fn classify_event(msg: &Value) -> Option<DapEvent> {
    if msg.get("type")?.as_str()? != "event" {
        return None;
    }
    let body = msg.get("body");
    match msg.get("event")?.as_str()? {
        "initialized" => Some(DapEvent::Initialized),
        "stopped" => Some(DapEvent::Stopped {
            thread_id: body
                .and_then(|b| b.get("threadId"))
                .and_then(Value::as_i64)
                .unwrap_or(0),
            reason: body
                .and_then(|b| b.get("reason"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }),
        "continued" => Some(DapEvent::Continued),
        "output" => Some(DapEvent::Output {
            category: body
                .and_then(|b| b.get("category"))
                .and_then(Value::as_str)
                .unwrap_or("console")
                .to_string(),
            text: body
                .and_then(|b| b.get("output"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }),
        "terminated" | "exited" => Some(DapEvent::Terminated),
        _ => None,
    }
}

/// Build the `initialize` request body (sans `seq`, which the transport stamps).
pub fn initialize_request() -> Value {
    json!({
        "type": "request",
        "command": "initialize",
        "arguments": {
            "adapterID": "debugpy",
            "linesStartAt1": true,
            "columnsStartAt1": true,
            "pathFormat": "path",
            "supportsRunInTerminalRequest": true
        }
    })
}

/// Build the `launch` request body for a Python program under `interpreter`.
pub fn launch_request(program: &Path, interpreter: &Path, stop_on_entry: bool) -> Value {
    json!({
        "type": "request",
        "command": "launch",
        "arguments": {
            "request": "launch",
            "program": program.to_string_lossy(),
            "python": [interpreter.to_string_lossy()],
            "console": "internalConsole",
            "stopOnEntry": stop_on_entry,
            "justMyCode": false
        }
    })
}

/// Build a `setBreakpoints` request body for one source file.
pub fn set_breakpoints_request(path: &Path, lines: &[u32]) -> Value {
    let breakpoints: Vec<Value> = lines.iter().map(|l| json!({ "line": l })).collect();
    json!({
        "type": "request",
        "command": "setBreakpoints",
        "arguments": {
            "source": { "path": path.to_string_lossy() },
            "breakpoints": breakpoints,
            "lines": lines
        }
    })
}

/// Build a response to an adapter-initiated reverse request (e.g.
/// `runInTerminal`, `startDebugging`). The DAP client MUST answer every reverse
/// request or the adapter blocks waiting; `request_seq` references the adapter's
/// request `seq`. croft doesn't yet honor these (it debugs in-process via
/// `internalConsole`), so it declines with `success: false` rather than stall.
pub fn reverse_request_response(request_seq: i64, command: &str, success: bool) -> Value {
    json!({
        "type": "response",
        "request_seq": request_seq,
        "success": success,
        "command": command,
        "message": if success { Value::Null } else { Value::from("unsupported by croft") },
    })
}

/// Build a `configurationDone` request body.
pub fn configuration_done_request() -> Value {
    json!({ "type": "request", "command": "configurationDone", "arguments": {} })
}

/// Build a thread-scoped execution request (`continue` / `next` / `stepIn` /
/// `stepOut`).
pub fn thread_request(command: &str, thread_id: i64) -> Value {
    json!({
        "type": "request",
        "command": command,
        "arguments": { "threadId": thread_id }
    })
}

/// Build a `stackTrace` request for the top frame of `thread_id` — enough to
/// learn the file + line the debugger is paused on.
pub fn stack_trace_request(thread_id: i64) -> Value {
    json!({
        "type": "request",
        "command": "stackTrace",
        "arguments": { "threadId": thread_id, "startFrame": 0, "levels": 1 }
    })
}

/// Extract `(path, 1-based line)` of the top frame from a `stackTrace` response
/// body, or `None` if the shape isn't as expected. Pure.
pub fn top_frame_location(stack_trace_response: &Value) -> Option<(PathBuf, usize)> {
    let frame = stack_trace_response
        .get("body")?
        .get("stackFrames")?
        .as_array()?
        .first()?;
    let path = frame.get("source")?.get("path")?.as_str()?;
    let line = frame.get("line")?.as_i64()?;
    Some((PathBuf::from(path), line as usize))
}

/// Fold breakpoint reports into a per-path unverified-line map, returning `true`
/// if anything changed. Each reported path's unverified set is replaced
/// wholesale (a `setBreakpoints` response describes that file's full set); a path
/// whose breakpoints all verify is dropped from the map. Pure.
pub fn fold_breakpoint_reports(
    map: &mut BTreeMap<PathBuf, std::collections::BTreeSet<usize>>,
    reports: &[BreakpointReport],
) -> bool {
    use std::collections::{BTreeSet, btree_map::Entry};
    if reports.is_empty() {
        return false;
    }
    let mut by_path: BTreeMap<PathBuf, BTreeSet<usize>> = BTreeMap::new();
    for r in reports {
        let set = by_path.entry(r.path.clone()).or_default();
        if !r.verified {
            set.insert(r.line);
        }
    }
    let mut changed = false;
    for (path, unverified) in by_path {
        match map.entry(path) {
            Entry::Occupied(mut e) => {
                if *e.get() != unverified {
                    if unverified.is_empty() {
                        e.remove();
                    } else {
                        e.insert(unverified);
                    }
                    changed = true;
                }
            }
            Entry::Vacant(e) => {
                if !unverified.is_empty() {
                    e.insert(unverified);
                    changed = true;
                }
            }
        }
    }
    changed
}

/// One running debug session.
pub struct DapSession {
    transport: DapTransport,
    pub phase: SessionPhase,
    /// Breakpoints to push once the adapter is `initialized`, keyed by absolute
    /// file path.
    breakpoints: BTreeMap<PathBuf, Vec<u32>>,
    /// Thread id reported by the most recent `stopped` event.
    pub stopped_thread: Option<i64>,
    /// File + 1-based line the debugger is paused on, resolved from the
    /// `stackTrace` response that follows each `stopped` event. Cleared on
    /// resume/terminate.
    pub current_location: Option<(PathBuf, usize)>,
    /// Per-file set of breakpoint lines the adapter reported as NOT verified
    /// (could not bind). Updated from `setBreakpoints` responses and `breakpoint`
    /// events; the editor renders these hollow so the user sees they are inert.
    pub unverified_breakpoints: BTreeMap<PathBuf, std::collections::BTreeSet<usize>>,
}

impl DapSession {
    /// Spawn the adapter, send `initialize`, and stash the breakpoints to push
    /// when the `initialized` event arrives. `adapter_*` describe how to launch
    /// the DAP adapter (e.g. the debug-venv python with `-m debugpy.adapter`);
    /// `program`/`interpreter` describe the debuggee.
    #[allow(clippy::too_many_arguments)]
    pub fn launch(
        adapter_program: &str,
        adapter_args: &[String],
        cwd: &Path,
        program: &Path,
        interpreter: &Path,
        breakpoints: BTreeMap<PathBuf, Vec<u32>>,
        stop_on_entry: bool,
    ) -> Result<DapSession> {
        let transport = DapTransport::spawn(adapter_program, adapter_args, cwd)?;
        transport.send(initialize_request())?;
        transport.send(launch_request(program, interpreter, stop_on_entry))?;
        Ok(DapSession {
            transport,
            phase: SessionPhase::Initializing,
            breakpoints,
            stopped_thread: None,
            current_location: None,
            unverified_breakpoints: BTreeMap::new(),
        })
    }

    /// Fold a set of breakpoint reports into `unverified_breakpoints`, returning
    /// `true` if the set changed (so the caller can trigger a redraw).
    fn apply_breakpoint_reports(&mut self, reports: &[BreakpointReport]) -> bool {
        fold_breakpoint_reports(&mut self.unverified_breakpoints, reports)
    }

    /// Drain everything the adapter has sent, advance the handshake, and return
    /// the user-facing events. Non-blocking.
    pub fn poll(&mut self) -> Vec<DapEvent> {
        let mut out = Vec::new();
        while let Ok(msg) = self.transport.incoming.try_recv() {
            // Reverse requests (adapter -> client, e.g. `runInTerminal`,
            // `startDebugging`) MUST be answered or the adapter blocks. croft
            // debugs in-process (`internalConsole`) and doesn't honor them yet,
            // so it declines rather than stall.
            if msg.get("type").and_then(Value::as_str) == Some("request") {
                if let (Some(seq), Some(command)) = (
                    msg.get("seq").and_then(Value::as_i64),
                    msg.get("command").and_then(Value::as_str),
                ) {
                    let _ = self
                        .transport
                        .send(reverse_request_response(seq, command, false));
                }
                continue;
            }
            // Responses: the only one we act on is `stackTrace`, which tells us
            // the file + line a `stopped` paused on.
            if msg.get("type").and_then(Value::as_str) == Some("response") {
                match msg.get("command").and_then(Value::as_str) {
                    Some("stackTrace") => {
                        if let Some(loc) = top_frame_location(&msg) {
                            self.current_location = Some(loc);
                        }
                    }
                    Some("setBreakpoints") => {
                        let reports = breakpoint_reports(&msg);
                        if self.apply_breakpoint_reports(&reports) {
                            out.push(DapEvent::BreakpointsUpdated);
                        }
                    }
                    _ => {}
                }
                continue;
            }
            // `breakpoint` events carry late-resolved verification updates.
            if msg.get("event").and_then(Value::as_str) == Some("breakpoint") {
                let reports = breakpoint_reports(&msg);
                if self.apply_breakpoint_reports(&reports) {
                    out.push(DapEvent::BreakpointsUpdated);
                }
                continue;
            }
            let Some(event) = classify_event(&msg) else {
                continue;
            };
            match &event {
                DapEvent::Initialized => {
                    self.push_breakpoints();
                    let _ = self.transport.send(configuration_done_request());
                    self.phase = SessionPhase::Running;
                }
                DapEvent::Stopped { thread_id, .. } => {
                    self.stopped_thread = Some(*thread_id);
                    self.phase = SessionPhase::Stopped;
                    // Resolve the paused location asynchronously; the response
                    // arrives on a later poll and fills `current_location`.
                    let _ = self.transport.send(stack_trace_request(*thread_id));
                }
                DapEvent::Continued => {
                    self.phase = SessionPhase::Running;
                    self.current_location = None;
                }
                DapEvent::Terminated => {
                    self.phase = SessionPhase::Terminated;
                    self.current_location = None;
                }
                DapEvent::Output { .. } => {}
                // Produced internally above, never via classify_event.
                DapEvent::BreakpointsUpdated => {}
            }
            out.push(event);
        }
        out
    }

    /// Push every stashed breakpoint set (called on `initialized`).
    fn push_breakpoints(&mut self) {
        for (path, lines) in &self.breakpoints {
            let _ = self.transport.send(set_breakpoints_request(path, lines));
        }
    }

    /// Resume execution of the stopped thread.
    pub fn continue_execution(&mut self) {
        if let Some(tid) = self.stopped_thread {
            let _ = self.transport.send(thread_request("continue", tid));
            self.phase = SessionPhase::Running;
        }
    }

    /// Push an updated breakpoint set for one file mid-session (debugpy accepts
    /// `setBreakpoints` at any time), so toggling a breakpoint while paused or
    /// running takes effect without a restart.
    pub fn update_breakpoints(&mut self, path: &Path, lines: &[u32]) {
        let _ = self.transport.send(set_breakpoints_request(path, lines));
    }

    /// Step over (`next`), into (`stepIn`), or out (`stepOut`).
    pub fn step(&mut self, command: &str) {
        if let Some(tid) = self.stopped_thread {
            let _ = self.transport.send(thread_request(command, tid));
        }
    }

    /// Ask the adapter to disconnect and terminate the debuggee.
    pub fn disconnect(&mut self) {
        let _ = self.transport.send(json!({
            "type": "request",
            "command": "disconnect",
            "arguments": { "terminateDebuggee": true }
        }));
        self.phase = SessionPhase::Terminated;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_stopped_event_with_thread_and_reason() {
        let msg = json!({
            "type": "event", "event": "stopped",
            "body": { "threadId": 1, "reason": "breakpoint" }
        });
        assert_eq!(
            classify_event(&msg),
            Some(DapEvent::Stopped {
                thread_id: 1,
                reason: "breakpoint".into()
            })
        );
    }

    #[test]
    fn classifies_initialized_and_terminated() {
        assert_eq!(
            classify_event(&json!({"type": "event", "event": "initialized"})),
            Some(DapEvent::Initialized)
        );
        assert_eq!(
            classify_event(&json!({"type": "event", "event": "terminated"})),
            Some(DapEvent::Terminated)
        );
        assert_eq!(
            classify_event(&json!({"type": "event", "event": "exited"})),
            Some(DapEvent::Terminated)
        );
    }

    #[test]
    fn reverse_request_response_references_request_seq() {
        let r = reverse_request_response(42, "runInTerminal", false);
        assert_eq!(r["type"], "response");
        assert_eq!(r["request_seq"], 42);
        assert_eq!(r["command"], "runInTerminal");
        assert_eq!(r["success"], false);
        assert_eq!(r["message"], "unsupported by croft");
        // A success response carries no message.
        let ok = reverse_request_response(1, "startDebugging", true);
        assert_eq!(ok["success"], true);
        assert!(ok["message"].is_null());
    }

    #[test]
    fn breakpoint_reports_parses_response_array() {
        let msg = json!({
            "type": "response", "command": "setBreakpoints",
            "body": { "breakpoints": [
                { "verified": true, "line": 3, "source": { "path": "/a.py" } },
                { "verified": false, "line": 7, "source": { "path": "/a.py" } },
            ]}
        });
        let reports = breakpoint_reports(&msg);
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[1].line, 7);
        assert!(!reports[1].verified);
    }

    #[test]
    fn breakpoint_reports_parses_singular_event_and_skips_line_zero() {
        let event = json!({
            "type": "event", "event": "breakpoint",
            "body": { "reason": "changed",
                "breakpoint": { "verified": true, "line": 5, "source": { "path": "/b.py" } } }
        });
        let reports = breakpoint_reports(&event);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].path, PathBuf::from("/b.py"));

        // line: 0 (not yet resolved) is skipped, not reported as line 0.
        let unresolved = json!({
            "type": "response", "command": "setBreakpoints",
            "body": { "breakpoints": [
                { "verified": true, "line": 0, "source": { "path": "/c.py" } } ]}
        });
        assert!(breakpoint_reports(&unresolved).is_empty());
    }

    #[test]
    fn fold_breakpoint_reports_tracks_only_unverified() {
        let mut map = BTreeMap::new();
        let p = PathBuf::from("/a.py");
        // One verified, one not: only the unverified line is tracked.
        let changed = fold_breakpoint_reports(
            &mut map,
            &[
                BreakpointReport { path: p.clone(), line: 3, verified: true },
                BreakpointReport { path: p.clone(), line: 7, verified: false },
            ],
        );
        assert!(changed);
        assert_eq!(
            map.get(&p).unwrap().iter().copied().collect::<Vec<_>>(),
            vec![7]
        );
        // A later report verifying line 7 clears it (and the now-empty entry).
        let changed = fold_breakpoint_reports(
            &mut map,
            &[BreakpointReport { path: p.clone(), line: 7, verified: true }],
        );
        assert!(changed);
        assert!(!map.contains_key(&p));
        // No reports => no change.
        assert!(!fold_breakpoint_reports(&mut map, &[]));
    }

    #[test]
    fn classifies_output_event() {
        let msg = json!({
            "type": "event", "event": "output",
            "body": { "category": "stdout", "output": "hello\n" }
        });
        assert_eq!(
            classify_event(&msg),
            Some(DapEvent::Output {
                category: "stdout".into(),
                text: "hello\n".into()
            })
        );
    }

    #[test]
    fn ignores_responses_and_unknown_events() {
        assert_eq!(
            classify_event(&json!({"type": "response", "command": "initialize"})),
            None
        );
        assert_eq!(
            classify_event(&json!({"type": "event", "event": "module"})),
            None
        );
    }

    #[test]
    fn set_breakpoints_request_maps_lines_to_objects() {
        let req = set_breakpoints_request(Path::new("/a/b.py"), &[2, 7]);
        assert_eq!(req["command"], "setBreakpoints");
        assert_eq!(req["arguments"]["source"]["path"], "/a/b.py");
        let bps = req["arguments"]["breakpoints"].as_array().unwrap();
        assert_eq!(bps.len(), 2);
        assert_eq!(bps[0]["line"], 2);
        assert_eq!(bps[1]["line"], 7);
    }

    #[test]
    fn launch_request_carries_program_and_interpreter() {
        let req = launch_request(Path::new("/w/app.py"), Path::new("/v/bin/python"), false);
        assert_eq!(req["command"], "launch");
        assert_eq!(req["arguments"]["program"], "/w/app.py");
        assert_eq!(req["arguments"]["python"][0], "/v/bin/python");
        assert_eq!(req["arguments"]["request"], "launch");
    }

    #[test]
    fn thread_request_carries_thread_id() {
        let req = thread_request("continue", 3);
        assert_eq!(req["command"], "continue");
        assert_eq!(req["arguments"]["threadId"], 3);
    }

    #[test]
    fn stack_trace_request_targets_top_frame() {
        let req = stack_trace_request(5);
        assert_eq!(req["command"], "stackTrace");
        assert_eq!(req["arguments"]["threadId"], 5);
        assert_eq!(req["arguments"]["levels"], 1);
    }

    #[test]
    fn top_frame_location_reads_path_and_line() {
        let resp = json!({
            "type": "response", "command": "stackTrace",
            "body": { "stackFrames": [
                { "source": { "path": "/w/app.py" }, "line": 12 },
                { "source": { "path": "/w/lib.py" }, "line": 1 }
            ]}
        });
        assert_eq!(
            top_frame_location(&resp),
            Some((PathBuf::from("/w/app.py"), 12))
        );
    }

    #[test]
    fn top_frame_location_none_when_empty_or_malformed() {
        let empty =
            json!({"type": "response", "command": "stackTrace", "body": {"stackFrames": []}});
        assert_eq!(top_frame_location(&empty), None);
        assert_eq!(top_frame_location(&json!({"body": {}})), None);
    }

    /// End-to-end against a real debugpy adapter: launch a script with a
    /// breakpoint and confirm the session reaches `Stopped` at that line.
    /// Ignored by default — needs `~/.croft/debug-venv` (uv + debugpy on 3.14);
    /// run with `cargo test --bin croft -- --ignored launches_debugpy`.
    #[test]
    #[ignore = "requires ~/.croft/debug-venv (uv + debugpy on CPython 3.14)"]
    fn launches_debugpy_and_stops_at_breakpoint() {
        use std::io::Write;
        let py = crate::dap::install::ensure_debug_venv().expect("provision debug venv");
        let mut f = tempfile::Builder::new().suffix(".py").tempfile().unwrap();
        writeln!(f, "x = 1\ny = 2\nz = x + y\nprint(z)").unwrap();
        // Canonicalize so our breakpoint path matches debugpy's (macOS resolves
        // /var -> /private/var; an unresolved path would never bind).
        let path = f.path().canonicalize().unwrap();

        let mut bps = BTreeMap::new();
        bps.insert(path.clone(), vec![3u32]); // break on `z = x + y`
        let mut sess = DapSession::launch(
            &py.to_string_lossy(),
            &["-m".to_string(), "debugpy.adapter".to_string()],
            path.parent().unwrap(),
            &path,
            &py,
            bps,
            false,
        )
        .unwrap();

        let mut stopped = false;
        for _ in 0..100 {
            for ev in sess.poll() {
                if matches!(ev, DapEvent::Stopped { .. }) {
                    stopped = true;
                }
            }
            if sess.current_location.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        sess.disconnect();

        assert!(stopped, "expected a `stopped` event from debugpy");
        let (_loc, line) = sess.current_location.clone().expect("a paused location");
        assert_eq!(line, 3, "should pause on the breakpoint line");
    }

    /// Regression: a breakpoint set via a NON-canonical (symlinked) path must
    /// still bind, and debugpy must echo back the SAME path we launched with
    /// (not the canonical `realpath`). This is why croft does NOT canonicalize:
    /// debugpy keys breakpoints and reports `stackTrace` source paths by the
    /// path it was launched with, so `current_location` matches the editor's
    /// open path and the gutter triangle renders. Canonicalizing the launch path
    /// would make debugpy report the canonical path and break that match.
    /// (macOS `/tmp` is itself a symlink to `/private/tmp`, exercising the
    /// `/var`-style case too.)
    #[test]
    #[ignore = "requires ~/.croft/debug-venv (uv + debugpy on CPython 3.14)"]
    fn binds_breakpoint_set_via_symlinked_path() {
        use std::io::Write;
        let py = crate::dap::install::ensure_debug_venv().expect("provision debug venv");
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let mut f = std::fs::File::create(real.join("m.py")).unwrap();
        writeln!(f, "a = 1\nb = 2\nc = a + b\nprint(c)").unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // Open the file through the symlinked directory: NOT canonical.
        let via_link = link.join("m.py");
        assert_ne!(
            via_link,
            via_link.canonicalize().unwrap(),
            "test setup: path must be non-canonical"
        );

        let mut bps = BTreeMap::new();
        bps.insert(via_link.clone(), vec![3u32]);
        let mut sess = DapSession::launch(
            &py.to_string_lossy(),
            &["-m".to_string(), "debugpy.adapter".to_string()],
            &link,
            &via_link,
            &py,
            bps,
            false,
        )
        .unwrap();

        let mut stopped = false;
        for _ in 0..100 {
            for ev in sess.poll() {
                if matches!(ev, DapEvent::Stopped { .. }) {
                    stopped = true;
                }
            }
            if sess.current_location.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        sess.disconnect();
        assert!(stopped, "breakpoint via symlinked path must still bind+stop");
        let (loc, line) = sess.current_location.clone().expect("a paused location");
        assert_eq!(line, 3);
        // debugpy echoes the launched (symlinked) path, NOT the canonical one.
        assert_eq!(loc, via_link, "debugpy must report the path as launched");
    }
}
