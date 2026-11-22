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

mod events;
mod start_requests;
pub use events::output_is_user_visible;
use events::*;
pub use start_requests::AdapterKind;
pub use start_requests::delve_test_request;
pub use start_requests::delve_zero_config_request;
pub use start_requests::launch_request;
pub use start_requests::lldb_launch_request;
pub use start_requests::lldb_test_launch_request;
pub use start_requests::pytest_debug_launch_request;
use start_requests::*;

/// What [`DapSession::request_step_in_targets`] did (#1543).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepInTargetsAsk {
    /// The request is out; the answer arrives as `StepInTargets`.
    Sent,
    /// Not paused, or the adapter cannot list step-in targets.
    Unsupported,
    /// The adapter could answer, but the paused frame is not known yet.
    NoFrame,
}

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
    /// An `evaluate` request resolved. `context` is the request context
    /// (`repl` / `watch` / `hover`), `expression` the input, `result` the value.
    Evaluated {
        context: String,
        expression: String,
        result: String,
        /// False when the adapter rejected the expression (`success: false`),
        /// so a watch row can render `<not available>` instead of the error.
        success: bool,
    },
    /// What an `exception` stop threw (#864), from the `exceptionInfo`
    /// response: `ModuleNotFoundError: No module named 'requests'`.
    ExceptionInfo { summary: String },
    /// The calls on the paused line that Step Into Target can enter (#1543),
    /// from a `stepInTargets` response: `(targetId, label)` in the
    /// adapter's order. Empty when the line makes no call it can name.
    StepInTargets { targets: Vec<(i64, String)> },
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
                verified: bp.get("verified").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

/// Build the child session-starting request from the configuration
/// vscode-js-debug hands back in its `startDebugging` reverse request. The
/// configuration (carrying the `__pendingTargetId` that ties the child to the
/// right target) is passed through verbatim as the arguments; the command
/// follows the configuration's own `request`, falling back to the reverse
/// request's outer `request` field (the DAP `startDebugging` shape carries it
/// beside the configuration), so an attach session's child attaches rather
/// than launching even when the inner configuration omits the field.
pub fn js_child_launch_request(configuration: &Value, outer_request: Option<&str>) -> Value {
    let command = match configuration
        .get("request")
        .and_then(Value::as_str)
        .or(outer_request)
    {
        Some("attach") => "attach",
        _ => "launch",
    };
    json!({
        "type": "request",
        "command": command,
        "arguments": configuration,
    })
}

/// A recognised `startDebugging` reverse request: vscode-js-debug asks the client
/// to open a child session for `config` and the client must answer the request
/// `request_seq`. `request` is the outer `arguments.request` ("launch" /
/// "attach"), kept separately because the inner configuration may omit its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartDebuggingRequest {
    pub request_seq: i64,
    pub config: Value,
    pub request: Option<String>,
}

/// Recognise a vscode-js-debug `startDebugging` reverse request and pull out the
/// child session configuration + the seq to answer. Returns `None` for anything
/// else (events, responses, other reverse requests like `runInTerminal`). Pure.
pub fn start_debugging_request(msg: &Value) -> Option<StartDebuggingRequest> {
    if msg.get("type")?.as_str()? != "request" {
        return None;
    }
    if msg.get("command")?.as_str()? != "startDebugging" {
        return None;
    }
    let arguments = msg.get("arguments")?;
    Some(StartDebuggingRequest {
        request_seq: msg.get("seq")?.as_i64()?,
        config: arguments.get("configuration")?.clone(),
        request: arguments
            .get("request")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// A breakpoint to set on a source line, with an optional `condition` (a boolean
/// expression the adapter evaluates; the breakpoint only pauses when it's true)
/// and an optional `log_message` (a logpoint: the adapter interpolates and
/// prints the text instead of pausing). The two are independent DAP fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBreakpoint {
    pub line: u32,
    pub condition: Option<String>,
    pub log_message: Option<String>,
    /// DAP `hitCondition` (#611): pause only once the line has been hit
    /// this many times (the adapter's syntax, e.g. `5` or `>= 5`).
    pub hit_condition: Option<String>,
}

impl SourceBreakpoint {
    pub fn plain(line: u32) -> Self {
        Self {
            line,
            condition: None,
            log_message: None,
            hit_condition: None,
        }
    }
}

/// Build a `setBreakpoints` request body for one source file, carrying any
/// per-breakpoint `condition` and `logMessage`.
pub fn set_breakpoints_request(path: &Path, breakpoints: &[SourceBreakpoint]) -> Value {
    let bps: Vec<Value> = breakpoints
        .iter()
        .map(|b| {
            let mut bp = json!({ "line": b.line });
            if let Some(c) = &b.condition {
                bp["condition"] = json!(c);
            }
            if let Some(m) = &b.log_message {
                bp["logMessage"] = json!(m);
            }
            if let Some(h) = &b.hit_condition {
                bp["hitCondition"] = json!(h);
            }
            bp
        })
        .collect();
    let lines: Vec<u32> = breakpoints.iter().map(|b| b.line).collect();
    json!({
        "type": "request",
        "command": "setBreakpoints",
        "arguments": {
            "source": { "path": path.to_string_lossy() },
            "breakpoints": bps,
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

/// Build a `setFunctionBreakpoints` request (#611): pause on entry to each
/// named function. The list replaces the adapter's whole set.
pub fn set_function_breakpoints_request(names: &[String]) -> Value {
    let bps: Vec<Value> = names.iter().map(|n| json!({ "name": n })).collect();
    json!({
        "type": "request",
        "command": "setFunctionBreakpoints",
        "arguments": { "breakpoints": bps }
    })
}

/// Build a `dataBreakpointInfo` request (#611): ask whether the variable
/// `name` under `variables_reference` can carry a data breakpoint, and for
/// the id to set it with.
pub fn data_breakpoint_info_request(variables_reference: i64, name: &str) -> Value {
    json!({
        "type": "request",
        "command": "dataBreakpointInfo",
        "arguments": { "variablesReference": variables_reference, "name": name }
    })
}

/// Build a `setDataBreakpoints` request (#611): pause when any of these data
/// ids is written. Replaces the adapter's whole set.
pub fn set_data_breakpoints_request(data_ids: &[String]) -> Value {
    let bps: Vec<Value> = data_ids
        .iter()
        .map(|id| json!({ "dataId": id, "accessType": "write" }))
        .collect();
    json!({
        "type": "request",
        "command": "setDataBreakpoints",
        "arguments": { "breakpoints": bps }
    })
}

/// The ids adapters give their "break where an exception is thrown, caught
/// or not" filter (#1426): debugpy `raised`, js-debug and netcoredbg `all`,
/// java `caught`. First offered wins.
const RAISED_FILTER_IDS: &[&str] = &["raised", "all", "caught"];

/// lldb-dap and CodeLLDB split "break on throw" per language: C++
/// `cpp_throw`, Objective-C `objc_throw`, Swift `swift_throw`. Every one
/// offered is turned on, so Objective-C(++) `@throw` stops too.
const THROW_FILTER_IDS: &[&str] = &["cpp_throw", "objc_throw", "swift_throw"];

/// The filter ids that mean "break on raised exceptions" among those an
/// adapter offers; `None` when it offers none of them.
fn raised_filter_ids(offered: &[(String, bool)]) -> Option<Vec<String>> {
    let has = |id: &str| offered.iter().any(|(o, _)| o == id);
    if let Some(id) = RAISED_FILTER_IDS.iter().find(|id| has(id)) {
        return Some(vec![id.to_string()]);
    }
    let throws: Vec<String> = THROW_FILTER_IDS
        .iter()
        .filter(|id| has(id))
        .map(|id| id.to_string())
        .collect();
    (!throws.is_empty()).then_some(throws)
}

/// The exception filters an adapter's `initialize` reply offers, as
/// `(id, default)`. `None` when the reply lists none.
fn offered_exception_filters(caps: &Value) -> Option<Vec<(String, bool)>> {
    let list = caps.get("exceptionBreakpointFilters")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|f| {
                let id = f.get("filter")?.as_str()?.to_string();
                Some((id, f.get("default").and_then(Value::as_bool) == Some(true)))
            })
            .collect(),
    )
}

/// Build a `setExceptionBreakpoints` request. `filters` are adapter-defined ids;
/// debugpy supports `raised`, `uncaught`, and `userUnhandled`. An empty slice
/// disables exception breaking.
pub fn set_exception_breakpoints_request(filters: &[String]) -> Value {
    json!({
        "type": "request",
        "command": "setExceptionBreakpoints",
        "arguments": { "filters": filters }
    })
}

/// Build a thread-scoped request (`continue` / `next` / `stepIn` /
/// `stepOut`, and `exceptionInfo`, which takes the same `threadId`).
pub fn thread_request(command: &str, thread_id: i64) -> Value {
    json!({
        "type": "request",
        "command": command,
        "arguments": { "threadId": thread_id }
    })
}

/// Build a `stepInTargets` request (#1543): the calls on `frame_id`'s line.
pub fn step_in_targets_request(frame_id: i64) -> Value {
    json!({
        "type": "request",
        "command": "stepInTargets",
        "arguments": { "frameId": frame_id }
    })
}

/// Build a `stepIn` into one of the line's calls (`targetId` from a
/// `stepInTargets` response) rather than the first call to run.
pub fn step_in_target_request(thread_id: i64, target_id: i64) -> Value {
    json!({
        "type": "request",
        "command": "stepIn",
        "arguments": { "threadId": thread_id, "targetId": target_id }
    })
}

/// The `(id, label)` pairs of a `stepInTargets` response.
pub fn parse_step_in_targets(msg: &Value) -> Vec<(i64, String)> {
    msg.pointer("/body/targets")
        .and_then(Value::as_array)
        .map(|ts| {
            ts.iter()
                .filter_map(|t| {
                    Some((
                        t.get("id")?.as_i64()?,
                        t.get("label")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Build a `threads` request: every thread the debuggee has.
pub fn threads_request() -> Value {
    json!({ "type": "request", "command": "threads" })
}

/// The `(id, name)` pairs of a `threads` response.
pub fn parse_threads(msg: &Value) -> Vec<(i64, String)> {
    msg.pointer("/body/threads")
        .and_then(Value::as_array)
        .map(|ts| {
            ts.iter()
                .filter_map(|t| {
                    Some((
                        t.get("id")?.as_i64()?,
                        t.get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
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

/// Build an `evaluate` request: run `expression` in the context of `frame_id`
/// (None evaluates in the global/REPL context). `context` is `repl`, `watch`,
/// or `hover` and tells the adapter how to format the result.
pub fn evaluate_request(expression: &str, frame_id: Option<i64>, context: &str) -> Value {
    let mut args = json!({ "expression": expression, "context": context });
    if let Some(fid) = frame_id {
        args["frameId"] = Value::from(fid);
    }
    json!({ "type": "request", "command": "evaluate", "arguments": args })
}

/// Extract an `evaluate` response's `result` string, or None. Pure.
pub fn parse_evaluate_result(response: &Value) -> Option<String> {
    response
        .get("body")?
        .get("result")?
        .as_str()
        .map(str::to_string)
}

/// One line naming what an `exceptionInfo` response says was thrown (#864):
/// `exceptionId: description`, with only the description's first line (an
/// adapter may append the stack) and without repeating an id the
/// description already opens with. None when the response names nothing.
pub fn exception_summary(response: &Value) -> Option<String> {
    let body = response.get("body")?;
    let text = |pointer: &str| {
        body.pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
    };
    // debugpy pads the id with "(note: full exception trace is shown but
    // execution is paused at: …)" when the stop is outside user code.
    let id = text("/exceptionId")
        .split("(note:")
        .next()
        .unwrap_or("")
        .trim();
    let description = [text("/description"), text("/details/message")]
        .into_iter()
        .find(|s| !s.is_empty())
        .and_then(|s| s.lines().next())
        .unwrap_or("")
        .trim();
    match (id.is_empty(), description.is_empty()) {
        (true, true) => None,
        (false, true) => Some(id.to_string()),
        (true, false) => Some(description.to_string()),
        (false, false) if description.starts_with(id) => Some(description.to_string()),
        (false, false) => Some(format!("{id}: {description}")),
    }
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
    /// The adapter marked it `expensive` (js-debug's Global, #1421): its
    /// variables are fetched only once the user opens it.
    pub expensive: bool,
}

/// A single variable. `variables_ref > 0` means it is expandable (its children
/// are fetched with a further `variables` request on that reference).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: String,
    pub variables_ref: i64,
    /// The adapter's `evaluateName` (#1546): an expression that evaluates
    /// to this variable from the frame, such as `obj.items[0]`. What Copy
    /// as Expression copies and Add to Watch watches. None when the adapter
    /// sends none (a synthetic row, or an adapter that doesn't know one).
    pub evaluate_name: Option<String>,
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
                        expensive: s.get("expensive").and_then(Value::as_bool) == Some(true),
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
                        evaluate_name: v
                            .get("evaluateName")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Apply `breakpoint` event reports to the unverified-line map, one line at
/// a time, returning `true` if anything changed. An event carries a single
/// breakpoint, unlike a `setBreakpoints` response, so it must not replace
/// its file's set: every other unverified line there would be dropped and
/// drawn as verified. Pure.
pub fn update_breakpoint_lines(
    map: &mut BTreeMap<PathBuf, std::collections::BTreeSet<usize>>,
    reports: &[BreakpointReport],
) -> bool {
    let mut changed = false;
    for r in reports {
        let set = map.entry(r.path.clone()).or_default();
        changed |= if r.verified {
            set.remove(&r.line)
        } else {
            set.insert(r.line)
        };
        if set.is_empty() {
            map.remove(&r.path);
        }
    }
    changed
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

/// A session and the name it was launched under.
///
/// `DapSession` itself has no name: it is built by four different adapter
/// constructors and none of them takes one. With a single session the UI
/// could say "the debugger" and be unambiguous; with several it has to say
/// WHICH, so the name rides alongside rather than being threaded through
/// every constructor (#310).
pub struct NamedSession<S = DapSession> {
    pub name: String,
    pub session: S,
    /// Events this session produced while it was NOT focused, kept for the
    /// moment it is (#567). Only the focused session's events drive the UI;
    /// dropping a background member's `stopped` left a breakpoint hit that
    /// nothing showed, so the member looked hung. Replayed in order on focus.
    pub backlog: Vec<DapEvent>,
    /// This session's Debug Console streams, one per output category: its
    /// own (#1501), so a sibling's colour or cut escape never runs into its
    /// output, and one per category (#932), so a `stderr` line or a progress
    /// bar never lands in the middle of a `stdout` line still being printed.
    pub console: Vec<ConsoleStream>,
}

impl<S> NamedSession<S> {
    /// The console stream for output `category`, started on its first use.
    pub fn console_stream(&mut self, category: &str) -> &mut ConsoleStream {
        let at = match self.console.iter().position(|c| c.category == category) {
            Some(at) => at,
            None => {
                self.console.push(ConsoleStream {
                    category: category.to_string(),
                    ..ConsoleStream::default()
                });
                self.console.len() - 1
            }
        };
        &mut self.console[at]
    }
}

/// One output category of a session, as the Debug Console reads it (#932).
#[derive(Default)]
pub struct ConsoleStream {
    pub category: String,
    pub ansi: crate::ansi_text::AnsiStream,
    /// Where the stream's open line is shown in the Debug Console, counted
    /// from the first line the console ever held so that lines dropped off
    /// its front do not move it. The line is redrawn there as later chunks
    /// add to it, and `None` means none is shown.
    pub shown_at: Option<usize>,
}

/// The debug sessions in flight, one of them FOCUSED (#310).
///
/// croft was single-session by construction: `Option<DapSession>`, and every
/// site that touched it meant "the session" because there could only be one.
/// A compound needs several, so the UI reads the FOCUSED one - call stack,
/// variables, stepping, status - while the protocol operations mean all of
/// them: every adapter is polled, every session is stopped, every session
/// takes a breakpoint.
///
/// Generic over the session type ONLY so the set's own rules - which session
/// is focused after a push, a replace, a removal - can be tested without an
/// adapter to talk to. Production always uses the default.
pub struct DebugSessions<S = DapSession> {
    sessions: Vec<NamedSession<S>>,
    /// Index into `sessions`. Meaningless when empty, and never left dangling:
    /// every mutation that can shrink the set clamps it.
    focused: usize,
}

// Hand-written rather than derived: a derive would demand `S: Default`, and
// a session has nothing sensible to be a default OF. An empty set does.
impl<S> Default for DebugSessions<S> {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            focused: 0,
        }
    }
}

impl<S> DebugSessions<S> {
    /// Whether nothing is being debugged at all.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// How many sessions are in flight.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// The session the UI is looking at: the call stack, the variables view,
    /// the stepping commands and the status line all mean this one.
    pub fn focused(&self) -> Option<&S> {
        self.sessions.get(self.focused).map(|s| &s.session)
    }

    /// The focused session's name, for a status line that has to say which.
    pub fn focused_name(&self) -> Option<&str> {
        self.sessions.get(self.focused).map(|s| s.name.as_str())
    }

    /// Mutable counterpart of [`focused`](Self::focused).
    pub fn focused_mut(&mut self) -> Option<&mut S> {
        self.sessions.get_mut(self.focused).map(|s| &mut s.session)
    }

    /// Replace the whole set with one session and focus it.
    ///
    /// What a single-configuration launch does, and it is deliberately not
    /// `push`: starting a configuration has always terminated whatever was
    /// running, and this slice does not change that. The caller disconnects
    /// the old ones first; this only drops them.
    pub fn replace_with(&mut self, name: impl Into<String>, session: S) {
        self.sessions.clear();
        self.sessions.push(NamedSession {
            name: name.into(),
            session,
            backlog: Vec::new(),
            console: Vec::new(),
        });
        self.focused = 0;
    }

    /// Add a session and focus it, leaving the others running.
    ///
    /// What a COMPOUND does, and the difference from
    /// [`replace_with`](Self::replace_with) is the whole of #310: a
    /// configuration launch has always terminated whatever was running, and
    /// a compound must not, or its second member would kill its first.
    pub fn push(&mut self, name: impl Into<String>, session: S) {
        self.sessions.push(NamedSession {
            name: name.into(),
            session,
            backlog: Vec::new(),
            console: Vec::new(),
        });
        self.focused = self.sessions.len() - 1;
    }

    /// Focus the session at `index`, if it exists.
    pub fn focus(&mut self, index: usize) -> bool {
        if index < self.sessions.len() {
            self.focused = index;
            true
        } else {
            false
        }
    }

    /// Every session's name, in launch order, for a picker or a status line.
    pub fn names(&self) -> Vec<String> {
        self.sessions.iter().map(|s| s.name.clone()).collect()
    }

    /// Take the session at `index` out of the set, clamping the focus.
    ///
    /// The caller disconnects it; this only removes it. One member of a
    /// compound ending must NOT take the others with it - dropping the whole
    /// set reaches the siblings as a `SIGKILL` through `DapTransport`'s drop,
    /// which is precisely the graceful handshake js-debug needs to retire its
    /// detached watchdog (#310).
    pub fn remove(&mut self, index: usize) -> Option<NamedSession<S>> {
        if index >= self.sessions.len() {
            return None;
        }
        let gone = self.sessions.remove(index);
        // Keep the focus on a session that exists, and prefer to stay where
        // the user was looking. The rule is positional, not identity-based:
        // removing a session BEFORE the focused one shifts the index down so
        // the same session stays focused, and removing the focused one itself
        // leaves the index where it was - which lands on the session that
        // moved up into that slot, or steps back when it was the last. So
        // retiring the middle of [A, B, C] while watching B leaves the user
        // on C, the next live session, rather than jumping back to A.
        if self.focused > index || self.focused >= self.sessions.len() {
            self.focused = self.focused.saturating_sub(1);
        }
        Some(gone)
    }

    /// Which session the UI is looking at, by index.
    pub fn focused_index(&self) -> usize {
        self.focused
    }

    /// Every session with its position and name, for the poll that routes a
    /// background member's events by who produced them (#567).
    pub fn iter_named_mut_indexed(
        &mut self,
    ) -> impl Iterator<Item = (usize, &mut NamedSession<S>)> {
        self.sessions.iter_mut().enumerate()
    }

    /// The focused session's queued background events, emptied: what it did
    /// while the user was looking at another member, to replay now.
    pub fn take_focused_backlog(&mut self) -> Vec<DapEvent> {
        self.sessions
            .get_mut(self.focused)
            .map(|s| std::mem::take(&mut s.backlog))
            .unwrap_or_default()
    }

    /// Drop every session without disconnecting - the old `= None`.
    pub fn clear(&mut self) {
        self.sessions.clear();
        self.focused = 0;
    }

    /// Every session, for the operations that mean all of them: the poll that
    /// drains each adapter, and the stop that must not orphan the rest.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut S> {
        self.sessions.iter_mut().map(|s| &mut s.session)
    }
}

/// One running debug session.
pub struct DapSession {
    transport: DapTransport,
    /// vscode-js-debug child session. js-debug is multi-session: the `transport`
    /// (parent) bootstraps and, via a `startDebugging` reverse request, asks us
    /// to open this second connection to the same server; the child is where
    /// breakpoints bind and execution stops. `None` for debugpy / lldb-dap and
    /// until the parent requests it.
    child: Option<DapTransport>,
    /// For a js-debug session, the `host:port` of the debug server so the child
    /// connection can be opened on demand. `None` for stdio adapters.
    js_server: Option<(String, u16)>,
    pub phase: SessionPhase,
    /// Why the adapter refused the `launch` or `attach` request, when it
    /// did (#264): delve rejecting a Go too old for it, say. The session
    /// ends with it rather than waiting in Initializing for an
    /// `initialized` that will never come.
    pub launch_error: Option<String>,
    /// Breakpoints to push once the adapter is `initialized`, keyed by absolute
    /// file path (with optional per-breakpoint conditions).
    breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
    /// Thread id reported by the most recent `stopped` event.
    pub stopped_thread: Option<i64>,
    /// Every thread (goroutine, under delve) at the last stop, as `(id,
    /// name)` in the adapter's order (#264). Refreshed at each stop.
    pub threads: Vec<(i64, String)>,
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
    /// The expensive scopes the user opened (#1421), each keyed by its name
    /// and its ordinal among the scopes of that name, so two scopes sharing
    /// a name open separately. They stay open, and are fetched, at every
    /// later stop, as in VS Code; the others stay collapsed and cost nothing.
    pub open_expensive_scopes: std::collections::BTreeSet<(String, usize)>,
    /// Variables keyed by `variablesReference`: the entry for a scope's ref holds
    /// that scope's members; expanding a variable adds its ref's children here.
    pub variables: BTreeMap<i64, Vec<Variable>>,
    /// Frame whose scopes/variables are currently loaded (top frame by default).
    pub selected_frame: Option<i64>,
    /// In-flight `variables` requests: request `seq` -> the `variablesReference`
    /// it asked for, so the response (which echoes only `request_seq`) can be
    /// filed under the right reference.
    pending_var_refs: std::collections::HashMap<i64, i64>,
    /// In-flight `evaluate` requests: request `seq` -> (context, expression), so
    /// the response can be turned into an [`DapEvent::Evaluated`].
    pending_evals: std::collections::HashMap<i64, (String, String)>,
    /// The `seq` of the `exceptionInfo` request the current exception stop
    /// sent (#864). Only its reply names what was thrown: a resume, a newer
    /// stop or the end of the session clears it, so a reply that lands
    /// after the program moved on is dropped instead of labelling it.
    exception_info_seq: Option<i64>,
    /// The `stepInTargets` request awaiting its answer (#1543), so a reply
    /// for a stop that is already over is dropped.
    step_in_targets_seq: Option<i64>,
    /// Active exception-breakpoint filter ids (debugpy: `raised`, `uncaught`;
    /// js-debug: `all`, `uncaught`; lldb-dap: `cpp_throw`, …). Sent on
    /// `initialized` and whenever toggled. Defaults to `uncaught` so an
    /// unhandled exception pauses the debugger instead of silently exiting;
    /// once the adapter lists its own filters (#1426) only the ones it
    /// offers are kept, plus those it turns on by default.
    pub exception_filters: Vec<String>,
    /// Break on Raised Exceptions toggled before the adapter answered
    /// `initialize` (#1426): the filter ids are not known yet, so the wanted
    /// state waits here and is translated once the capabilities arrive.
    pending_raised: Option<bool>,
    /// Most recently seen thread id (from `thread`/`stopped` events). Used to
    /// target `pause` while the program is running and `stopped_thread` is None.
    known_thread: Option<i64>,
    /// The adapter's `initialize` capabilities (#611), `Null` until it
    /// answers; gates the breakpoint kinds it does not implement.
    pub capabilities: Value,
    /// Function breakpoints (#611), sent on `initialized` and on change.
    pub function_breakpoints: Vec<String>,
    /// Run to Cursor's temporary breakpoint (#611): sent alongside the
    /// file's own set, and withdrawn at the next stop, whatever caused it.
    run_to: Option<(PathBuf, u32)>,
    /// Data breakpoints (#611): (data id, label) for each "break when this
    /// value changes", and the `dataBreakpointInfo` requests in flight.
    pub data_breakpoints: Vec<(String, String)>,
    pending_data_info: std::collections::HashMap<i64, String>,
    /// The session attached to a process croft did not start, so ending
    /// it must leave that process running.
    attached: bool,
    /// Where delve built the program it launched (#1206). Declared after
    /// `transport`, so delve is gone before the folder is removed.
    build_dir: Option<DebugBuildDir>,
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
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
        stop_on_entry: bool,
    ) -> Result<DapSession> {
        Self::launch_with(
            adapter_program,
            adapter_args,
            cwd,
            launch_request(program, interpreter, stop_on_entry),
            breakpoints,
        )
    }

    /// Generic launch: spawn `adapter_program args...`, send `initialize`, then
    /// the caller-built `launch_request` (debugpy or lldb-dap shaped). The rest
    /// of the protocol (breakpoints, configurationDone, stepping, inspection) is
    /// adapter-agnostic, so every adapter shares this path.
    pub fn launch_with(
        adapter_program: &str,
        adapter_args: &[String],
        cwd: &Path,
        launch_request: Value,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
    ) -> Result<DapSession> {
        let transport = DapTransport::spawn(adapter_program, adapter_args, cwd)?;
        transport.send(initialize_request(false))?;
        let attached = is_attach(&launch_request);
        transport.send(launch_request)?;
        let mut session = Self::new_with_transport(transport, breakpoints, None);
        session.attached = attached;
        Ok(session)
    }

    /// Attach to a DAP server that is already listening at `host:port`
    /// (#1417): a `debugpy --listen` process, whose port is its own adapter.
    /// No adapter is spawned; `initialize` and `attach_request` go straight
    /// to the socket.
    pub fn attach_tcp(
        host: &str,
        port: u16,
        attach_request: Value,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
    ) -> Result<DapSession> {
        let transport = DapTransport::connect_tcp_once(host, port)?;
        transport.send(initialize_request(false))?;
        transport.send(attach_request)?;
        let mut session = Self::new_with_transport(transport, breakpoints, None);
        session.attached = true;
        Ok(session)
    }

    /// Launch a vscode-js-debug session: spawn `node dapDebugServer.js <port>
    /// <host>` as a TCP debug server, connect the parent socket, and drive the
    /// `pwa-node` launch. The parent will later issue a `startDebugging` reverse
    /// request (handled in [`DapSession::poll`]) that opens the child session
    /// which actually runs `program` and binds breakpoints.
    pub fn launch_js(
        node: &str,
        server_js: &Path,
        cwd: &Path,
        program: &Path,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
        stop_on_entry: bool,
    ) -> Result<DapSession> {
        Self::launch_js_with(
            node,
            server_js,
            cwd,
            js_launch_request(program, cwd, stop_on_entry),
            breakpoints,
        )
    }

    /// Like [`DapSession::launch_js`] but with a caller-built session-starting
    /// request (a launch.json config's `launch` or `attach`), mirroring what
    /// [`DapSession::launch_with`] is to the stdio adapters.
    pub fn launch_js_with(
        node: &str,
        server_js: &Path,
        cwd: &Path,
        start_request: Value,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
    ) -> Result<DapSession> {
        let host = "127.0.0.1";
        let port = crate::dap::transport::free_port()?;
        let args = vec![
            server_js.to_string_lossy().into_owned(),
            port.to_string(),
            host.to_string(),
        ];
        let node_dir = crate::dap::install::node_bin_dir(node);
        let transport =
            DapTransport::connect_tcp_server(node, &args, cwd, host, port, node_dir.as_deref())?;
        transport.send(initialize_request(true))?;
        transport.send(start_request)?;
        Ok(Self::new_with_transport(
            transport,
            breakpoints,
            Some((host.to_string(), port)),
        ))
    }

    /// Launch a delve session (#264): spawn `dlv dap --listen=<host:port>`,
    /// connect over TCP, and send `start_request`. delve serves one session
    /// per connection, so unlike js-debug there is no child to open later.
    pub fn launch_delve(
        dlv: &Path,
        cwd: &Path,
        start_request: Value,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
    ) -> Result<DapSession> {
        let build_dir = delve_builds(&start_request)
            .then(DebugBuildDir::create)
            .and_then(std::io::Result::ok);
        let start_request = match &build_dir {
            Some(dir) => with_build_output(start_request, dir.path()),
            None => start_request,
        };
        let host = "127.0.0.1";
        let port = crate::dap::transport::free_port()?;
        let args = vec![String::from("dap"), format!("--listen={host}:{port}")];
        let transport =
            DapTransport::connect_tcp_server(&dlv.to_string_lossy(), &args, cwd, host, port, None)?;
        transport.send(initialize_request(false))?;
        let attached = is_attach(&start_request);
        transport.send(with_remote_output(start_request))?;
        let mut session = Self::new_with_transport(transport, breakpoints, None);
        session.attached = attached;
        session.build_dir = build_dir;
        Ok(session)
    }

    /// Attach to a headless delve that already serves DAP at `host:port`
    /// (#1418, the Go extension's "Connect to server"): no dlv is started,
    /// `initialize` and the `mode: "remote"` attach go straight to the
    /// socket, and stopping disconnects with `terminateDebuggee: false`.
    /// Delve keeps the remote program running only when it was started
    /// with `--accept-multiclient`; a single-client server shuts down with
    /// its debuggee on disconnect.
    pub fn attach_delve_server(
        host: &str,
        port: u16,
        attach_request: Value,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
    ) -> Result<DapSession> {
        let transport = DapTransport::connect_tcp(host, port)?;
        transport.send(initialize_request(false))?;
        transport.send(attach_request)?;
        let mut session = Self::new_with_transport(transport, breakpoints, None);
        session.attached = true;
        Ok(session)
    }

    /// Assemble a fresh session around its (parent) `transport`. `js_server` is
    /// set only for vscode-js-debug, marking the session multi-session and
    /// carrying the address its child connection reuses.
    fn new_with_transport(
        transport: DapTransport,
        breakpoints: BTreeMap<PathBuf, Vec<SourceBreakpoint>>,
        js_server: Option<(String, u16)>,
    ) -> DapSession {
        DapSession {
            transport,
            child: None,
            js_server,
            phase: SessionPhase::Initializing,
            launch_error: None,
            breakpoints,
            stopped_thread: None,
            threads: Vec::new(),
            current_location: None,
            unverified_breakpoints: BTreeMap::new(),
            stack_frames: Vec::new(),
            scopes: Vec::new(),
            open_expensive_scopes: std::collections::BTreeSet::new(),
            variables: BTreeMap::new(),
            selected_frame: None,
            pending_var_refs: std::collections::HashMap::new(),
            pending_evals: std::collections::HashMap::new(),
            exception_info_seq: None,
            step_in_targets_seq: None,
            exception_filters: vec![String::from("uncaught")],
            pending_raised: None,
            known_thread: None,
            capabilities: Value::Null,
            function_breakpoints: Vec::new(),
            run_to: None,
            data_breakpoints: Vec::new(),
            pending_data_info: std::collections::HashMap::new(),
            attached: false,
            build_dir: None,
        }
    }

    /// The transport that user-facing operations (breakpoints, stepping, stack,
    /// variables, evaluate) target: the js-debug child once it exists, otherwise
    /// the parent. For stdio adapters there is only the parent.
    fn active(&self) -> &DapTransport {
        self.child.as_ref().unwrap_or(&self.transport)
    }

    /// Interrupt a running program (`pause`), targeting the most recently known
    /// thread. No-op if no thread is known yet.
    pub fn pause(&mut self) {
        if let Some(tid) = self.stopped_thread.or(self.known_thread) {
            let _ = self.active().send(thread_request("pause", tid));
        }
    }

    /// Replace the active exception-breakpoint filters, resending immediately so
    /// the change takes effect mid-session (debugpy accepts it at any time).
    pub fn set_exception_filters(&mut self, filters: Vec<String>) {
        self.exception_filters = filters;
        // The active connection: for js-debug that is the child session,
        // where the debuggee runs; the parent ignores exception filters.
        let _ = self
            .active()
            .send(set_exception_breakpoints_request(&self.exception_filters));
    }

    /// This adapter's ids for "break on raised exceptions" (#1426):
    /// debugpy's `raised`, js-debug's `all`, lldb-dap's `cpp_throw` and
    /// `objc_throw`. `None` before the adapter answers `initialize`, or when
    /// it offers no such filter (Delve).
    pub fn raised_exception_filters(&self) -> Option<Vec<String>> {
        raised_filter_ids(&offered_exception_filters(&self.capabilities)?)
    }

    /// Turn the adapter's raised-exceptions filters on or off, keeping the
    /// other active filters (`uncaught`), and resend. `Some(on)` says which
    /// way it went; `None` (nothing sent) when the adapter has no such filter.
    /// Before the adapter answers `initialize` the wanted state is kept and
    /// applied with its own ids once it does.
    pub fn toggle_raised_exceptions(&mut self) -> Option<bool> {
        if self.capabilities.is_null() {
            let on = !self.pending_raised.unwrap_or(false);
            self.pending_raised = Some(on);
            return Some(on);
        }
        let raised = self.raised_exception_filters()?;
        let on = !raised.iter().any(|r| self.has_exception_filter(r));
        let mut filters: Vec<String> = self
            .exception_filters
            .iter()
            .filter(|f| !raised.contains(f))
            .cloned()
            .collect();
        if on {
            filters.extend(raised);
        }
        self.set_exception_filters(filters);
        Some(on)
    }

    /// Whether an exception filter id is currently active.
    pub fn has_exception_filter(&self, filter: &str) -> bool {
        self.exception_filters.iter().any(|f| f == filter)
    }

    /// Evaluate `expression` in the selected frame (or globally if no frame is
    /// selected). The result arrives on a later poll as [`DapEvent::Evaluated`].
    /// `context` is typically `repl` or `watch`.
    pub fn evaluate(&mut self, expression: &str, context: &str) {
        let req = evaluate_request(expression, self.selected_frame, context);
        if let Ok(seq) = self.active().send(req) {
            self.pending_evals
                .insert(seq, (context.to_string(), expression.to_string()));
        }
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

    /// Show another thread's stack (#264): a goroutine other than the one
    /// that hit the breakpoint. Stepping then applies to it, as in VS Code.
    /// Only while stopped: a running thread has no stack to show.
    pub fn select_thread(&mut self, thread_id: i64) {
        if self.phase != SessionPhase::Stopped || self.stopped_thread == Some(thread_id) {
            return;
        }
        self.stopped_thread = Some(thread_id);
        self.known_thread = Some(thread_id);
        self.clear_inspection();
        let _ = self.active().send(stack_trace_request(thread_id));
    }

    /// Load scopes for `frame_id` and fetch each scope's variables. Called when a
    /// stop resolves its stack (top frame) or the user selects another frame.
    pub fn load_frame(&mut self, frame_id: i64) {
        self.selected_frame = Some(frame_id);
        self.scopes.clear();
        self.variables.clear();
        self.pending_var_refs.clear();
        let _ = self.active().send(scopes_request(frame_id));
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

    /// The persistent identity of the scope at `index`: its name and how
    /// many earlier scopes share that name (#1421). A `variablesReference`
    /// changes at every stop, so it cannot keep a scope open across stops.
    fn scope_key(&self, index: usize) -> Option<(String, usize)> {
        let scope = self.scopes.get(index)?;
        let nth = self.scopes[..index]
            .iter()
            .filter(|s| s.name == scope.name)
            .count();
        Some((scope.name.clone(), nth))
    }

    /// Whether the variables of the scope at `index` are shown: always,
    /// unless it is an expensive one the user has not opened (#1421).
    pub fn scope_open(&self, index: usize) -> bool {
        match self.scopes.get(index) {
            Some(scope) if scope.expensive => self
                .scope_key(index)
                .is_some_and(|k| self.open_expensive_scopes.contains(&k)),
            Some(_) => true,
            None => false,
        }
    }

    /// Open or close the expensive scope at `index` (#1421), fetching its
    /// variables when it opens.
    pub fn toggle_expensive_scope(&mut self, index: usize) {
        let Some(key) = self.scope_key(index) else {
            return;
        };
        if !self.open_expensive_scopes.remove(&key) {
            self.open_expensive_scopes.insert(key);
            let r = self.scopes[index].variables_ref;
            self.expand_variable(r);
        }
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
        if let Ok(seq) = self.active().send(variables_request(variables_reference)) {
            self.pending_var_refs.insert(seq, variables_reference);
        }
    }

    /// Fold a set of breakpoint reports into `unverified_breakpoints`, returning
    /// `true` if the set changed (so the caller can trigger a redraw).
    fn apply_breakpoint_reports(&mut self, reports: &[BreakpointReport]) -> bool {
        fold_breakpoint_reports(&mut self.unverified_breakpoints, reports)
    }

    /// Drain everything the adapter(s) have sent, advance the handshake, and
    /// return the user-facing events. Non-blocking. For js-debug both the parent
    /// and the child connections are drained; `from_child` records which one a
    /// message came from so reverse requests are answered on the right socket.
    pub fn poll(&mut self) -> Vec<DapEvent> {
        let mut out = Vec::new();
        // Drain the parent first so a `startDebugging` request can open the child
        // before we try to drain it this same poll.
        while let Ok(msg) = self.transport.incoming.try_recv() {
            self.handle_message(msg, false, &mut out);
        }
        // Drain the child into a buffer first (handling a message borrows `self`
        // mutably, which can't coexist with holding the child's receiver).
        let mut child_msgs = Vec::new();
        if let Some(child) = self.child.as_ref() {
            while let Ok(msg) = child.incoming.try_recv() {
                child_msgs.push(msg);
            }
        }
        for msg in child_msgs {
            self.handle_message(msg, true, &mut out);
        }
        out
    }

    /// Handle one decoded message from the parent (`from_child = false`) or the
    /// js-debug child (`from_child = true`), pushing any user-facing events.
    fn handle_message(&mut self, msg: Value, from_child: bool, out: &mut Vec<DapEvent>) {
        // Reverse requests (adapter -> client) MUST be answered or the adapter
        // blocks. vscode-js-debug's `startDebugging` asks us to open the child
        // session; everything else croft declines (it debugs in-process). So
        // is a `startDebugging` on a session with no child path (anything but
        // js-debug, see `initialize_request`): acknowledging it would promise
        // a subprocess session that never opens.
        if msg.get("type").and_then(Value::as_str) == Some("request") {
            if let Some(req) = start_debugging_request(&msg).filter(|_| self.js_server.is_some()) {
                let _ = self.transport.send(reverse_request_response(
                    req.request_seq,
                    "startDebugging",
                    true,
                ));
                self.open_child_session(&req.config, req.request.as_deref());
            } else if let (Some(seq), Some(command)) = (
                msg.get("seq").and_then(Value::as_i64),
                msg.get("command").and_then(Value::as_str),
            ) {
                let resp = reverse_request_response(seq, command, false);
                if from_child {
                    if let Some(c) = self.child.as_ref() {
                        let _ = c.send(resp);
                    }
                } else {
                    let _ = self.transport.send(resp);
                }
            }
            return;
        }
        // Responses: the only one we act on is `stackTrace`, which tells us the
        // file + line a `stopped` paused on.
        if msg.get("type").and_then(Value::as_str) == Some("response") {
            match msg.get("command").and_then(Value::as_str) {
                Some("stackTrace") => {
                    self.stack_frames = parse_stack_frames(&msg);
                    if let Some(top) = self.stack_frames.first() {
                        if let Some(path) = top.path.clone() {
                            self.current_location = Some((path, top.line));
                        }
                        let top_id = top.id;
                        self.load_frame(top_id);
                    }
                    out.push(DapEvent::InspectionUpdated);
                }
                Some("threads") => {
                    self.threads = parse_threads(&msg);
                    out.push(DapEvent::InspectionUpdated);
                }
                Some("stepInTargets") => {
                    let req_seq = msg.get("request_seq").and_then(Value::as_i64);
                    if req_seq.is_some() && req_seq == self.step_in_targets_seq {
                        self.step_in_targets_seq = None;
                        out.push(DapEvent::StepInTargets {
                            targets: parse_step_in_targets(&msg),
                        });
                    }
                }
                Some("exceptionInfo") => {
                    let req_seq = msg.get("request_seq").and_then(Value::as_i64);
                    if req_seq.is_some() && req_seq == self.exception_info_seq {
                        self.exception_info_seq = None;
                        if let Some(summary) = exception_summary(&msg) {
                            out.push(DapEvent::ExceptionInfo { summary });
                        }
                    }
                }
                Some("scopes") => {
                    self.scopes = parse_scopes(&msg);
                    // An expensive scope (a whole Global object) waits until
                    // the user opens it (#1421), or every step would wait
                    // for it.
                    let refs: Vec<i64> = (0..self.scopes.len())
                        .filter(|&i| self.scope_open(i))
                        .map(|i| self.scopes[i].variables_ref)
                        .collect();
                    for r in refs {
                        self.request_variables(r);
                    }
                    out.push(DapEvent::InspectionUpdated);
                }
                Some("variables") => {
                    let req_seq = msg.get("request_seq").and_then(Value::as_i64);
                    if let Some(reference) = req_seq.and_then(|s| self.pending_var_refs.remove(&s))
                    {
                        self.variables.insert(reference, parse_variables(&msg));
                        out.push(DapEvent::InspectionUpdated);
                    }
                }
                Some("evaluate") => {
                    let req_seq = msg.get("request_seq").and_then(Value::as_i64);
                    if let Some((context, expression)) =
                        req_seq.and_then(|s| self.pending_evals.remove(&s))
                    {
                        let success = msg.get("success").and_then(Value::as_bool).unwrap_or(false);
                        let result = parse_evaluate_result(&msg).unwrap_or_else(|| {
                            msg.get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string()
                        });
                        out.push(DapEvent::Evaluated {
                            context,
                            expression,
                            result,
                            success,
                        });
                    }
                }
                // js-debug reports breakpoints `verified:false` ("provisional")
                // before binding them on the fly, so treating its reports as
                // unverified would wrongly render every js/ts breakpoint hollow.
                // Only stdio adapters' reports drive the hollow rendering; for a
                // js-debug session the guard fails and the arm falls through to
                // the no-op `_` below.
                Some("initialize") => {
                    if let Some(body) = msg.get("body") {
                        self.capabilities = body.clone();
                        // Keep only the filters this adapter knows (#1426),
                        // plus those it turns on by default.
                        if let Some(offered) = offered_exception_filters(body) {
                            let mut keep: Vec<String> = self
                                .exception_filters
                                .iter()
                                .filter(|f| offered.iter().any(|(id, _)| id == *f))
                                .cloned()
                                .collect();
                            for (id, default) in &offered {
                                if *default && !keep.contains(id) {
                                    keep.push(id.clone());
                                }
                            }
                            // A toggle made before this reply, in this
                            // adapter's own ids.
                            if self.pending_raised == Some(true) {
                                for id in raised_filter_ids(&offered).unwrap_or_default() {
                                    if !keep.contains(&id) {
                                        keep.push(id);
                                    }
                                }
                            }
                            self.exception_filters = keep;
                        }
                        self.pending_raised = None;
                    }
                }
                Some("launch" | "attach")
                    if msg.get("success").and_then(Value::as_bool) == Some(false) =>
                {
                    let reason = launch_failure_reason(&msg);
                    out.push(DapEvent::Output {
                        category: String::from("stderr"),
                        text: format!("{reason}\n"),
                    });
                    self.launch_error = Some(reason);
                    self.phase = SessionPhase::Terminated;
                    self.current_location = None;
                    out.push(DapEvent::Terminated);
                }
                Some("dataBreakpointInfo") => {
                    let req_seq = msg.get("request_seq").and_then(Value::as_i64);
                    if let Some(label) = req_seq.and_then(|s| self.pending_data_info.remove(&s)) {
                        let body = msg.get("body");
                        match body.and_then(|b| b.get("dataId")).and_then(Value::as_str) {
                            Some(id) => {
                                self.data_breakpoints.push((id.to_string(), label));
                                self.push_data_breakpoints();
                            }
                            None => {
                                let why = body
                                    .and_then(|b| b.get("description"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("the adapter cannot watch it")
                                    .to_string();
                                out.push(DapEvent::Output {
                                    category: String::from("console"),
                                    text: format!("Cannot break when {label} changes: {why}\n"),
                                });
                            }
                        }
                    }
                }
                Some("setBreakpoints") if self.js_server.is_none() => {
                    let reports = breakpoint_reports(&msg);
                    if self.apply_breakpoint_reports(&reports) {
                        out.push(DapEvent::BreakpointsUpdated);
                    }
                }
                _ => {}
            }
            return;
        }
        // `thread` events announce thread ids; remember the latest so we can
        // target `pause` while the program is running.
        if msg.get("event").and_then(Value::as_str) == Some("thread") {
            if let Some(tid) = msg
                .get("body")
                .and_then(|b| b.get("threadId"))
                .and_then(Value::as_i64)
            {
                self.known_thread = Some(tid);
            }
            return;
        }
        // `breakpoint` events carry late-resolved verification updates.
        if msg.get("event").and_then(Value::as_str) == Some("breakpoint") {
            if self.js_server.is_none() {
                let reports = breakpoint_reports(&msg);
                if update_breakpoint_lines(&mut self.unverified_breakpoints, &reports) {
                    out.push(DapEvent::BreakpointsUpdated);
                }
            }
            return;
        }
        let Some(event) = classify_event(&msg) else {
            return;
        };
        match &event {
            DapEvent::Initialized => {
                // Push breakpoints + configurationDone to the connection that
                // just initialized (parent or child). `active()` already resolves
                // to the child once it exists, and to the parent before that.
                self.push_breakpoints();
                let t = self.active();
                let _ = t.send(set_exception_breakpoints_request(&self.exception_filters));
                let _ = t.send(configuration_done_request());
                self.phase = SessionPhase::Running;
            }
            DapEvent::Stopped { thread_id, reason } => {
                // Run to Cursor's breakpoint lives for one stop, whichever
                // breakpoint or step produced it.
                if let Some((path, _)) = self.run_to.take() {
                    let own = self.breakpoints.get(&path).cloned().unwrap_or_default();
                    let _ = self.active().send(set_breakpoints_request(&path, &own));
                }
                self.stopped_thread = Some(*thread_id);
                self.known_thread = Some(*thread_id);
                self.phase = SessionPhase::Stopped;
                self.clear_inspection();
                let _ = self.active().send(stack_trace_request(*thread_id));
                let _ = self.active().send(threads_request());
                // "Paused (exception)" alone does not say what was thrown
                // (#864); the answer arrives as `ExceptionInfo`, for this
                // stop only.
                self.exception_info_seq = None;
                if reason == "exception" && self.supports("supportsExceptionInfoRequest") {
                    self.exception_info_seq = self
                        .active()
                        .send(thread_request("exceptionInfo", *thread_id))
                        .ok();
                }
            }
            DapEvent::Continued => {
                self.phase = SessionPhase::Running;
                self.current_location = None;
                self.exception_info_seq = None;
                self.clear_inspection();
            }
            DapEvent::Terminated => {
                // A js-debug parent ("bootstrap") can end its own connection
                // while the child debuggee is still running; that must NOT tear
                // the session down. Only the child's exit (or a stdio adapter /
                // parent with no child) actually ends the run.
                if from_child || self.child.is_none() {
                    self.phase = SessionPhase::Terminated;
                    self.current_location = None;
                    self.exception_info_seq = None;
                    self.clear_inspection();
                }
            }
            DapEvent::Output { .. } => {}
            DapEvent::BreakpointsUpdated
            | DapEvent::InspectionUpdated
            | DapEvent::Evaluated { .. }
            | DapEvent::ExceptionInfo { .. }
            | DapEvent::StepInTargets { .. } => {}
        }
        // Suppress the parent's own `terminated` for js-debug while a child is
        // still live (the parent can wind down its bootstrap connection first).
        if from_child || !matches!(event, DapEvent::Terminated) || self.child.is_none() {
            out.push(event);
        }
    }

    /// Open the vscode-js-debug child session: connect a second socket to the
    /// same debug server and replay `initialize` + `launch` with the adapter-
    /// provided child configuration (which carries the `__pendingTargetId`).
    fn open_child_session(&mut self, config: &Value, outer_request: Option<&str>) {
        let Some((host, port)) = self.js_server.clone() else {
            return;
        };
        match DapTransport::connect_tcp(&host, port) {
            Ok(child) => {
                let _ = child.send(initialize_request(true));
                let _ = child.send(js_child_launch_request(config, outer_request));
                self.child = Some(child);
            }
            Err(e) => {
                crate::dap::log::log(&format!("js-debug child connect failed: {e}"));
            }
        }
    }

    /// Push every stashed breakpoint set to the active connection (called on
    /// `initialized`).
    fn push_breakpoints(&mut self) {
        let t = self.active();
        for (path, lines) in &self.breakpoints {
            let _ = t.send(set_breakpoints_request(path, lines));
        }
        if !self.function_breakpoints.is_empty() {
            let _ = t.send(set_function_breakpoints_request(&self.function_breakpoints));
        }
    }

    /// Whether the adapter declared capability `name` (#611). An adapter
    /// that has not answered `initialize` yet counts as supporting it, so a
    /// request made during startup is sent rather than refused.
    pub fn supports(&self, name: &str) -> bool {
        match &self.capabilities {
            Value::Null => true,
            caps => caps.get(name).and_then(Value::as_bool).unwrap_or(false),
        }
    }

    /// Replace the function breakpoints (#611), resending at once.
    pub fn set_function_breakpoints(&mut self, names: Vec<String>) {
        self.function_breakpoints = names;
        if !matches!(self.phase, SessionPhase::Initializing) {
            let _ = self
                .active()
                .send(set_function_breakpoints_request(&self.function_breakpoints));
        }
    }

    /// Run to Cursor (#611): add a one-stop breakpoint at 1-based `line` of
    /// `path` and resume.
    pub fn run_to_cursor(&mut self, path: &Path, line: u32) {
        // A second Run to Cursor before the first stopped withdraws the
        // first one's line, or it would stay behind with no gutter mark.
        if let Some((old, _)) = self.run_to.take()
            && old != path
        {
            let own = self.breakpoints.get(&old).cloned().unwrap_or_default();
            let _ = self.active().send(set_breakpoints_request(&old, &own));
        }
        self.run_to = Some((path.to_path_buf(), line));
        let own = self.breakpoints.get(path).cloned().unwrap_or_default();
        let set = self.with_run_to(path, &own);
        let _ = self.active().send(set_breakpoints_request(path, &set));
        self.continue_execution();
    }

    fn with_run_to(&self, path: &Path, own: &[SourceBreakpoint]) -> Vec<SourceBreakpoint> {
        with_run_to(self.run_to.as_ref(), path, own)
    }

    /// Break when a variable changes (#611): ask the adapter for the data id
    /// of `name` under `variables_reference`; the reply sets the breakpoint.
    pub fn break_on_change(&mut self, variables_reference: i64, name: &str) {
        if let Ok(seq) = self
            .active()
            .send(data_breakpoint_info_request(variables_reference, name))
        {
            self.pending_data_info.insert(seq, name.to_string());
        }
    }

    /// Drop every data breakpoint (#611).
    pub fn clear_data_breakpoints(&mut self) {
        self.data_breakpoints.clear();
        self.push_data_breakpoints();
    }

    fn push_data_breakpoints(&mut self) {
        let ids: Vec<String> = self
            .data_breakpoints
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        let _ = self.active().send(set_data_breakpoints_request(&ids));
    }

    /// Resume execution of the stopped thread.
    pub fn continue_execution(&mut self) {
        if let Some(tid) = self.stopped_thread {
            let _ = self.active().send(thread_request("continue", tid));
            self.phase = SessionPhase::Running;
            self.exception_info_seq = None;
            self.step_in_targets_seq = None;
        }
    }

    /// Push an updated breakpoint set for one file mid-session (debugpy accepts
    /// `setBreakpoints` at any time), so toggling a breakpoint while paused or
    /// running takes effect without a restart.
    pub fn update_breakpoints(&mut self, path: &Path, breakpoints: &[SourceBreakpoint]) {
        // Kept current so Run to Cursor can restore exactly this set.
        self.breakpoints
            .insert(path.to_path_buf(), breakpoints.to_vec());
        // A pending Run to Cursor in this file keeps its line.
        let set = self.with_run_to(path, breakpoints);
        let _ = self.active().send(set_breakpoints_request(path, &set));
    }

    /// Step over (`next`), into (`stepIn`), or out (`stepOut`).
    pub fn step(&mut self, command: &str) {
        if let Some(tid) = self.stopped_thread {
            let _ = self.active().send(thread_request(command, tid));
            // The stop is over; its exception reply no longer applies (#864).
            self.exception_info_seq = None;
            self.step_in_targets_seq = None;
        }
    }

    /// Ask for the calls on the selected frame's line (#1543), answered as
    /// [`DapEvent::StepInTargets`]. [`StepInTargetsAsk::Unsupported`] when
    /// the program is not paused or the adapter lacks
    /// `supportsStepInTargetsRequest`; [`StepInTargetsAsk::NoFrame`] when it
    /// could answer but the paused frame has not arrived yet (or the send
    /// failed), so the caller must not fall back to a plain step in.
    pub fn request_step_in_targets(&mut self) -> StepInTargetsAsk {
        if self.phase != SessionPhase::Stopped || !self.supports("supportsStepInTargetsRequest") {
            return StepInTargetsAsk::Unsupported;
        }
        let Some(frame) = self
            .selected_frame
            .or_else(|| self.stack_frames.first().map(|f| f.id))
        else {
            return StepInTargetsAsk::NoFrame;
        };
        self.step_in_targets_seq = self.active().send(step_in_targets_request(frame)).ok();
        if self.step_in_targets_seq.is_some() {
            StepInTargetsAsk::Sent
        } else {
            StepInTargetsAsk::NoFrame
        }
    }

    /// Step into the call `target_id` on the paused line (#1543).
    pub fn step_in_target(&mut self, target_id: i64) {
        if let Some(tid) = self.stopped_thread {
            let _ = self.active().send(step_in_target_request(tid, target_id));
            self.exception_info_seq = None;
            self.step_in_targets_seq = None;
        }
    }

    /// Ask the adapter to disconnect and terminate the debuggee. Sent to both the
    /// child (if any) and the parent so a js-debug server tears the whole tree
    /// down, not just the target. An attached session leaves its process
    /// running: croft did not start it, and delve and lldb-dap both honour
    /// `terminateDebuggee` for an attach.
    pub fn disconnect(&mut self) {
        let req = disconnect_request(self.attached);
        if let Some(child) = self.child.as_ref() {
            let _ = child.send(req.clone());
        }
        let _ = self.transport.send(req);
        self.phase = SessionPhase::Terminated;
        self.exception_info_seq = None;
    }
}

/// Whether delve builds the program for this request: a `launch` in
/// `debug`, `test` or `auto` mode (delve's default is `debug`) whose
/// launch.json names no `output`. An `exec`, a core dump or an attach
/// builds nothing (#1206).
pub fn delve_builds(request: &Value) -> bool {
    let args = &request["arguments"];
    request["command"] == "launch"
        && args.get("output").is_none()
        && matches!(
            args["mode"].as_str(),
            None | Some("debug" | "test" | "auto")
        )
}

/// The launch request with delve's `output` set to a `__debug_bin` in `dir`,
/// so the binary it builds lands outside the workspace. Left in the package
/// folder, it stayed behind whenever croft ended delve before delve could
/// delete it, showing in the Explorer and as untracked in git (#1206).
pub fn with_build_output(mut request: Value, dir: &Path) -> Value {
    let bin = dir.join(format!("__debug_bin{}", std::env::consts::EXE_SUFFIX));
    if let Some(args) = request.get_mut("arguments").and_then(Value::as_object_mut) {
        args.insert("output".into(), json!(bin.to_string_lossy()));
    }
    request
}

/// A private temp folder for one delve session's build, removed with the
/// session however delve ended (#1206).
#[derive(Debug)]
pub struct DebugBuildDir(PathBuf);

impl DebugBuildDir {
    /// A fresh folder under the system temp dir. `create_dir` refuses one
    /// that already exists, so a path someone else planted is never used.
    pub fn create() -> std::io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir = std::env::temp_dir().join(format!(
            "croft-dlv-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&dir)?;
        Ok(Self(dir))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for DebugBuildDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The `(name, value)` pairs inline values draw from (#135): every variable
/// of the "Locals"-named scopes, or of the first scope when no scope carries
/// that name (adapters differ — debugpy says "Locals", js-debug "Local").
/// Pure over already-fetched data; no adapter round-trip.
pub fn inline_locals(
    scopes: &[Scope],
    variables: &std::collections::BTreeMap<i64, Vec<Variable>>,
) -> Vec<(String, String)> {
    let local_refs: Vec<i64> = scopes
        .iter()
        .filter(|s| s.name.to_lowercase().contains("local"))
        .map(|s| s.variables_ref)
        .collect();
    let refs: Vec<i64> = if local_refs.is_empty() {
        scopes
            .first()
            .map(|s| s.variables_ref)
            .into_iter()
            .collect()
    } else {
        local_refs
    };
    refs.iter()
        .filter_map(|r| variables.get(r))
        .flatten()
        .map(|v| (v.name.clone(), v.value.clone()))
        .collect()
}

#[cfg(test)]
mod session_set_tests;

/// `own` plus a pending Run to Cursor line when it is in `path`, as a plain
/// breakpoint: a logpoint, condition or hit count already on that line would
/// let the program run straight past it.
fn with_run_to(
    run_to: Option<&(PathBuf, u32)>,
    path: &Path,
    own: &[SourceBreakpoint],
) -> Vec<SourceBreakpoint> {
    let mut set = own.to_vec();
    if let Some((target, line)) = run_to
        && target == path
    {
        set.retain(|b| b.line != *line);
        set.push(SourceBreakpoint::plain(*line));
    }
    set
}

#[cfg(test)]
pub(crate) mod tests;
