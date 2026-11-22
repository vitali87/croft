//! vscode-js-debug's child sessions: its `startDebugging` reverse request, the answer
//! every reverse request needs, and opening the child.

use super::*;

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

impl DapSession {
    /// Open the vscode-js-debug child session: connect a second socket to the
    /// same debug server and replay `initialize` + `launch` with the adapter-
    /// provided child configuration (which carries the `__pendingTargetId`).
    pub(super) fn open_child_session(&mut self, config: &Value, outer_request: Option<&str>) {
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
}
