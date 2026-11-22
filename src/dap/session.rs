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

mod breakpoints;
mod child_sessions;
mod events;
mod exceptions;
mod inspection;
mod running_set;
mod start_requests;
mod stepping;
use breakpoints::*;
use child_sessions::*;
pub use events::output_is_user_visible;
use events::*;
use exceptions::*;
pub use inspection::inline_locals;
use inspection::*;
pub use running_set::ConsoleStream;
pub use running_set::DebugSessions;
pub use start_requests::AdapterKind;
pub use start_requests::delve_test_request;
pub use start_requests::delve_zero_config_request;
pub use start_requests::launch_request;
pub use start_requests::lldb_launch_request;
pub use start_requests::lldb_test_launch_request;
pub use start_requests::pytest_debug_launch_request;
use start_requests::*;
pub use stepping::StepInTargetsAsk;
use stepping::*;

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

    /// Whether the adapter declared capability `name` (#611). An adapter
    /// that has not answered `initialize` yet counts as supporting it, so a
    /// request made during startup is sent rather than refused.
    pub fn supports(&self, name: &str) -> bool {
        match &self.capabilities {
            Value::Null => true,
            caps => caps.get(name).and_then(Value::as_bool).unwrap_or(false),
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

#[cfg(test)]
mod session_set_tests;

#[cfg(test)]
pub(crate) mod tests;
