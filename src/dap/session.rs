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
    /// The call stack, scopes, or variables changed (a `stackTrace`/`scopes`/
    /// `variables` response landed). The app refreshes the Call Stack / Variables
    /// panels.
    InspectionUpdated,
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

/// Build a `stackTrace` request for the full call stack of `thread_id`. Omitting
/// `levels` asks the adapter for every frame (debugpy returns the whole stack),
/// which powers the Call Stack panel; the top frame still drives the stop-line.
pub fn stack_trace_request(thread_id: i64) -> Value {
    json!({
        "type": "request",
        "command": "stackTrace",
        "arguments": { "threadId": thread_id, "startFrame": 0 }
    })
}

/// Build a `scopes` request for one stack frame (locals / globals containers).
pub fn scopes_request(frame_id: i64) -> Value {
    json!({
        "type": "request",
        "command": "scopes",
        "arguments": { "frameId": frame_id }
    })
}

/// Build a `variables` request for a `variablesReference` (a scope's container or
/// an expandable variable's children).
pub fn variables_request(variables_reference: i64) -> Value {
    json!({
        "type": "request",
        "command": "variables",
        "arguments": { "variablesReference": variables_reference }
    })
}

/// One frame of the call stack: the adapter's frame `id` (used for `scopes`), the
/// display `name`, and the source location (None for frames with no source, e.g.
/// native frames).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackFrame {
    pub id: i64,
    pub name: String,
    pub path: Option<PathBuf>,
    pub line: usize,
}

/// A variable container for a frame (e.g. "Locals", "Globals"). `variables_ref`
/// is fed to a `variables` request to fetch its members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub name: String,
    pub variables_ref: i64,
}

/// A single variable. `variables_ref > 0` means it is expandable (its children
/// are fetched with a further `variables` request on that reference).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: String,
    pub variables_ref: i64,
}

/// Parse the full call stack from a `stackTrace` response. Pure.
pub fn parse_stack_frames(stack_trace_response: &Value) -> Vec<StackFrame> {
    stack_trace_response
        .get("body")
        .and_then(|b| b.get("stackFrames"))
        .and_then(Value::as_array)
        .map(|frames| {
            frames
                .iter()
                .filter_map(|f| {
                    Some(StackFrame {
                        id: f.get("id")?.as_i64()?,
                        name: f
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        path: f
                            .get("source")
                            .and_then(|s| s.get("path"))
                            .and_then(Value::as_str)
                            .map(PathBuf::from),
                        line: f.get("line").and_then(Value::as_i64).unwrap_or(0) as usize,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse scopes from a `scopes` response. Pure.
pub fn parse_scopes(scopes_response: &Value) -> Vec<Scope> {
    scopes_response
        .get("body")
        .and_then(|b| b.get("scopes"))
        .and_then(Value::as_array)
        .map(|scopes| {
            scopes
                .iter()
                .filter_map(|s| {
                    Some(Scope {
                        name: s.get("name")?.as_str()?.to_string(),
                        variables_ref: s.get("variablesReference")?.as_i64()?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse variables from a `variables` response. Pure.
pub fn parse_variables(variables_response: &Value) -> Vec<Variable> {
    variables_response
        .get("body")
        .and_then(|b| b.get("variables"))
        .and_then(Value::as_array)
        .map(|vars| {
            vars.iter()
                .filter_map(|v| {
                    Some(Variable {
                        name: v.get("name")?.as_str()?.to_string(),
                        value: v
                            .get("value")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        type_name: v
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        variables_ref: v
                            .get("variablesReference")
                            .and_then(Value::as_i64)
                            .unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
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
    /// Full call stack at the current stop (top frame first), from the
    /// `stackTrace` response. Empty while running.
    pub stack_frames: Vec<StackFrame>,
    /// Scopes (Locals / Globals) of the selected frame.
    pub scopes: Vec<Scope>,
    /// Variables keyed by `variablesReference`: the entry for a scope's ref holds
    /// that scope's members; expanding a variable adds its ref's children here.
    pub variables: BTreeMap<i64, Vec<Variable>>,
    /// Frame whose scopes/variables are currently loaded (top frame by default).
    pub selected_frame: Option<i64>,
    /// In-flight `variables` requests: request `seq` -> the `variablesReference`
    /// it asked for, so the response (which echoes only `request_seq`) can be
    /// filed under the right reference.
    pending_var_refs: std::collections::HashMap<i64, i64>,
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
            stack_frames: Vec::new(),
            scopes: Vec::new(),
            variables: BTreeMap::new(),
            selected_frame: None,
            pending_var_refs: std::collections::HashMap::new(),
        })
    }

    /// Clear all inspection state (call stack, scopes, variables). Done on every
    /// resume/step/terminate since frame ids and variable references are only
    /// valid for the current stop.
    fn clear_inspection(&mut self) {
        self.stack_frames.clear();
        self.scopes.clear();
        self.variables.clear();
        self.selected_frame = None;
        self.pending_var_refs.clear();
    }

    /// Load scopes for `frame_id` and fetch each scope's variables. Called when a
    /// stop resolves its stack (top frame) or the user selects another frame.
    pub fn load_frame(&mut self, frame_id: i64) {
        self.selected_frame = Some(frame_id);
        self.scopes.clear();
        self.variables.clear();
        self.pending_var_refs.clear();
        let _ = self.transport.send(scopes_request(frame_id));
    }

    /// Find a top-level variable by name across the selected frame's loaded
    /// scopes (Locals first, then Globals), for hover-to-evaluate. Returns the
    /// first match. None while running or before variables arrive.
    pub fn lookup_local(&self, name: &str) -> Option<&Variable> {
        self.scopes
            .iter()
            .filter_map(|s| self.variables.get(&s.variables_ref))
            .flatten()
            .find(|v| v.name == name)
    }

    /// Request the children of an expandable variable (its `variablesReference`)
    /// unless already loaded; the response files them under that reference.
    pub fn expand_variable(&mut self, variables_reference: i64) {
        if variables_reference > 0 && !self.variables.contains_key(&variables_reference) {
            self.request_variables(variables_reference);
        }
    }

    /// Send a `variables` request and remember which reference it was for.
    fn request_variables(&mut self, variables_reference: i64) {
        if let Ok(seq) = self.transport.send(variables_request(variables_reference)) {
            self.pending_var_refs.insert(seq, variables_reference);
        }
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
                        self.stack_frames = parse_stack_frames(&msg);
                        if let Some(top) = self.stack_frames.first() {
                            if let Some(path) = top.path.clone() {
                                self.current_location = Some((path, top.line));
                            }
                            // Auto-load the top frame's locals/globals.
                            let top_id = top.id;
                            self.load_frame(top_id);
                        }
                        out.push(DapEvent::InspectionUpdated);
                    }
                    Some("scopes") => {
                        self.scopes = parse_scopes(&msg);
                        // Fetch each scope's variables.
                        let refs: Vec<i64> =
                            self.scopes.iter().map(|s| s.variables_ref).collect();
                        for r in refs {
                            self.request_variables(r);
                        }
                        out.push(DapEvent::InspectionUpdated);
                    }
                    Some("variables") => {
                        let req_seq = msg.get("request_seq").and_then(Value::as_i64);
                        if let Some(reference) =
                            req_seq.and_then(|s| self.pending_var_refs.remove(&s))
                        {
                            self.variables.insert(reference, parse_variables(&msg));
                            out.push(DapEvent::InspectionUpdated);
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
                    // Frame ids / variable references from a prior stop are stale.
                    self.clear_inspection();
                    // Resolve the stack asynchronously; the response arrives on a
                    // later poll and fills the location + call stack + variables.
                    let _ = self.transport.send(stack_trace_request(*thread_id));
                }
                DapEvent::Continued => {
                    self.phase = SessionPhase::Running;
                    self.current_location = None;
                    self.clear_inspection();
                }
                DapEvent::Terminated => {
                    self.phase = SessionPhase::Terminated;
                    self.current_location = None;
                    self.clear_inspection();
                }
                DapEvent::Output { .. } => {}
                // Produced internally above, never via classify_event.
                DapEvent::BreakpointsUpdated | DapEvent::InspectionUpdated => {}
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
    fn stack_trace_request_targets_full_stack() {
        let req = stack_trace_request(5);
        assert_eq!(req["command"], "stackTrace");
        assert_eq!(req["arguments"]["threadId"], 5);
        assert_eq!(req["arguments"]["startFrame"], 0);
        // No `levels` cap: the adapter returns every frame.
        assert!(req["arguments"].get("levels").is_none());
    }

    #[test]
    fn parse_stack_frames_reads_all_frames_with_ids() {
        let resp = json!({
            "type": "response", "command": "stackTrace",
            "body": { "stackFrames": [
                { "id": 1000, "name": "isValid", "source": { "path": "/w/app.py" }, "line": 12 },
                { "id": 1001, "name": "<module>", "source": { "path": "/w/app.py" }, "line": 24 }
            ]}
        });
        let frames = parse_stack_frames(&resp);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].id, 1000);
        assert_eq!(frames[0].name, "isValid");
        assert_eq!(frames[0].path, Some(PathBuf::from("/w/app.py")));
        assert_eq!(frames[0].line, 12);
        assert_eq!(frames[1].id, 1001);
    }

    #[test]
    fn parse_stack_frames_empty_when_malformed() {
        assert!(parse_stack_frames(&json!({"body": {}})).is_empty());
        assert!(parse_stack_frames(&json!({})).is_empty());
    }

    #[test]
    fn parse_scopes_and_variables() {
        let scopes = parse_scopes(&json!({
            "body": { "scopes": [
                { "name": "Locals", "variablesReference": 7 },
                { "name": "Globals", "variablesReference": 8 }
            ]}
        }));
        assert_eq!(scopes.len(), 2);
        assert_eq!(scopes[0].name, "Locals");
        assert_eq!(scopes[0].variables_ref, 7);

        let vars = parse_variables(&json!({
            "body": { "variables": [
                { "name": "x", "value": "1", "type": "int", "variablesReference": 0 },
                { "name": "obj", "value": "<Foo>", "type": "Foo", "variablesReference": 9 }
            ]}
        }));
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].name, "x");
        assert_eq!(vars[0].value, "1");
        assert_eq!(vars[0].type_name, "int");
        assert_eq!(vars[0].variables_ref, 0);
        assert_eq!(vars[1].variables_ref, 9, "expandable variable keeps its ref");
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

    /// End-to-end: at a breakpoint, the inspection chain (stackTrace -> scopes ->
    /// variables) populates the call stack and the locals. Break on line 3 of
    /// `x=1; y=2; z=x+y; print(z)`, where `x` and `y` are already bound.
    #[test]
    #[ignore = "requires ~/.croft/debug-venv (uv + debugpy on CPython 3.14)"]
    fn inspection_chain_populates_stack_and_locals() {
        use std::io::Write;
        let py = crate::dap::install::ensure_debug_venv().expect("provision debug venv");
        let mut f = tempfile::Builder::new().suffix(".py").tempfile().unwrap();
        writeln!(f, "x = 1\ny = 2\nz = x + y\nprint(z)").unwrap();
        let path = f.path().canonicalize().unwrap();
        let mut bps = BTreeMap::new();
        bps.insert(path.clone(), vec![3u32]);
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

        // Poll until the variables for some scope have arrived (the chain runs
        // across several polls: stackTrace -> scopes -> variables).
        let mut have_locals = false;
        for _ in 0..200 {
            sess.poll();
            let names: Vec<&str> = sess
                .variables
                .values()
                .flatten()
                .map(|v| v.name.as_str())
                .collect();
            if names.contains(&"x") && names.contains(&"y") {
                have_locals = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let frames = sess.stack_frames.clone();
        let xy: Vec<(String, String)> = sess
            .variables
            .values()
            .flatten()
            .filter(|v| v.name == "x" || v.name == "y")
            .map(|v| (v.name.clone(), v.value.clone()))
            .collect();
        sess.disconnect();

        assert!(!frames.is_empty(), "call stack must be populated at the stop");
        assert!(have_locals, "locals x and y must arrive via the chain");
        assert!(xy.contains(&("x".to_string(), "1".to_string())));
        assert!(xy.contains(&("y".to_string(), "2".to_string())));
        // Hover-to-evaluate lookup resolves a loaded local by name.
        let x = sess.lookup_local("x").expect("lookup_local should find x");
        assert_eq!(x.value, "1");
        assert!(sess.lookup_local("nonexistent").is_none());
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
