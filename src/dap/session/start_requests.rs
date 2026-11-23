//! The requests that start and end a session: `initialize`, each adapter family's
//! `launch` (debugpy, lldb-dap, delve and vscode-js-debug), `configurationDone` and
//! `disconnect`, and the adapter families themselves.

use super::*;

/// Build the `initialize` request body (sans `seq`, which the transport stamps).
///
/// `child_sessions` says whether the session can open a child session, and
/// so whether `supportsStartDebuggingRequest` is advertised. vscode-js-debug
/// only issues the `startDebugging` reverse request (the one that spawns the
/// child session that actually binds breakpoints) to a client that claims
/// support, and a js-debug session can open that child. debugpy also sends
/// it when offered, once per subprocess of the debuggee, and falls back to
/// its own `debugpyAttach` event when not; croft has no child path for
/// debugpy (or any other adapter), so those sessions must not claim one.
pub fn initialize_request(child_sessions: bool) -> Value {
    json!({
        "type": "request",
        "command": "initialize",
        "arguments": {
            "adapterID": "debugpy",
            "linesStartAt1": true,
            "columnsStartAt1": true,
            "pathFormat": "path",
            "supportsRunInTerminalRequest": true,
            "supportsStartDebuggingRequest": child_sessions
        }
    })
}

/// The debug adapter family for a given source language. Selected from the
/// file extension by [`crate::dap::registry::adapter_for_extension`] (a
/// data-driven lookup over the `[[debug_adapters]]` manifests); drives which
/// adapter is spawned and the `launch` shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    /// debugpy (the adapter on croft's CPython 3.14+ venv), launches the `.py`
    /// source directly under the project's own interpreter.
    Debugpy,
    /// lldb-dap, launches a compiled binary (Rust / C / C++).
    LldbDap,
    /// vscode-js-debug (`dapDebugServer.js` over TCP), launches a `.js`/`.ts`
    /// program under Node. Multi-session: a parent bootstraps and a child binds
    /// breakpoints (see [`start_debugging_request`]).
    JsDebug,
    /// delve (`dlv dap` over TCP), launches a Go package or test (#264).
    /// Single-session: breakpoints bind in the one connection, like debugpy.
    Delve,
}

/// The zero-config delve `launch` for the Go file at `file` (#264): the file's
/// package directory is the program, which delve builds itself. A `_test.go`
/// file runs in delve's `test` mode (the package's tests); anything else in
/// `debug` mode (the package's `main`).
pub fn delve_zero_config_request(file: &Path) -> Value {
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    let is_test = file
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with("_test.go"));
    json!({
        "type": "request",
        "command": "launch",
        "arguments": {
            "mode": if is_test { "test" } else { "debug" },
            "program": dir.to_string_lossy(),
            "cwd": dir.to_string_lossy(),
            "stopOnEntry": false,
        }
    })
}

/// A delve `launch` request with `outputMode: "remote"` unless it names a
/// mode: delve then forwards the program's stdout and stderr as DAP `output`
/// events for the Debug Console, as vscode-go does. In delve's default
/// `local` mode they go to delve's own stdout and stderr, which croft
/// discards, so nothing the program printed was shown (#1205). An `attach`
/// is left alone: the process's output is not delve's to forward.
pub fn with_remote_output(mut request: Value) -> Value {
    if request["command"] == "launch"
        && let Some(args) = request.get_mut("arguments").and_then(Value::as_object_mut)
    {
        args.entry("outputMode").or_insert_with(|| json!("remote"));
    }
    request
}

/// delve's `test` mode for the package in `dir`, with `args` for the test
/// binary: `-test.run` scopes it to one test (#264).
pub fn delve_test_request(dir: &Path, args: &[String]) -> Value {
    json!({
        "type": "request",
        "command": "launch",
        "arguments": {
            "mode": "test",
            "program": dir.to_string_lossy(),
            "cwd": dir.to_string_lossy(),
            "args": args,
            "stopOnEntry": false,
        }
    })
}

/// Build the lldb-dap `launch` request for a compiled `program` binary. Unlike
/// debugpy there is no interpreter; lldb-dap loads the binary and its DWARF.
pub fn lldb_launch_request(program: &Path, stop_on_entry: bool) -> Value {
    json!({
        "type": "request",
        "command": "launch",
        "arguments": {
            "program": program.to_string_lossy(),
            "stopOnEntry": stop_on_entry,
        }
    })
}

/// Build the lldb-dap `launch` request for a cargo TEST binary: like
/// [`lldb_launch_request`] plus the debuggee argv — libtest's substring
/// filter, so only the picked test runs under the debugger.
pub fn lldb_test_launch_request(program: &Path, args: &[String]) -> Value {
    json!({
        "type": "request",
        "command": "launch",
        "arguments": {
            "program": program.to_string_lossy(),
            "args": args,
            "stopOnEntry": false,
        }
    })
}

/// Build the debugpy `launch` request that debugs one pytest test: launch the
/// `pytest` MODULE under the project's own interpreter (`python`, where pytest
/// is importable — the adapter injects debugpy into the debuggee, so the
/// project venv needs no debugpy) with `-k name` narrowing the run.
pub fn pytest_debug_launch_request(python: &Path, cwd: &Path, name: &str) -> Value {
    json!({
        "type": "request",
        "command": "launch",
        "arguments": {
            "request": "launch",
            "module": "pytest",
            "args": ["-v", "-k", name],
            "python": [python.to_string_lossy()],
            "cwd": cwd.to_string_lossy(),
            "console": "internalConsole",
            "stopOnEntry": false,
            // Only the user's code, as VS Code and debugpy default (#1519):
            // a step over a library call does not walk into the library.
            "justMyCode": true
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
            // See `pytest_debug_launch_request` (#1519).
            "justMyCode": true
        }
    })
}

/// Build the vscode-js-debug `launch` request for a Node program. The `pwa-node`
/// type is the canonical Node launcher (Zed normalises `node`/`node-terminal`
/// onto it too); `sourceMaps` lets TypeScript resolve to its `.ts` source. This
/// is sent on BOTH the parent connection and, with the adapter-provided child
/// configuration, the child connection that actually runs the program.
pub fn js_launch_request(program: &Path, cwd: &Path, stop_on_entry: bool) -> Value {
    json!({
        "type": "request",
        "command": "launch",
        "arguments": {
            "type": "pwa-node",
            "request": "launch",
            "program": program.to_string_lossy(),
            "cwd": cwd.to_string_lossy(),
            // js-debug resolves its `${workspaceFolder}` defaults (rootPath,
            // resolveSourceMapLocations) against this, as VS Code sends it
            // (#1414); `cwd` is the file's workspace root here.
            "__workspaceFolder": cwd.to_string_lossy(),
            "console": "internalConsole",
            "sourceMaps": true,
            "stopOnEntry": stop_on_entry
        }
    })
}

/// Build a `configurationDone` request body.
pub fn configuration_done_request() -> Value {
    json!({ "type": "request", "command": "configurationDone", "arguments": {} })
}

/// Whether a start request attaches to a running process rather than
/// launching one.
pub(super) fn is_attach(request: &Value) -> bool {
    request["command"] == "attach"
}

/// `disconnect`, ending the debuggee only when the session launched it.
pub(super) fn disconnect_request(attached: bool) -> Value {
    json!({
        "type": "request",
        "command": "disconnect",
        "arguments": { "terminateDebuggee": !attached }
    })
}
