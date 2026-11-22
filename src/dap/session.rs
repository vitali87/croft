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
mod launch;
mod messages;
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
#[cfg(test)]
use launch::*;
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
