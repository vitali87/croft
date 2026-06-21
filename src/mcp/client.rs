//! MCP client: JSON-RPC 2.0 envelope + the MCP lifecycle over a [`McpTransport`].
//!
//! The handshake is `initialize` (request) -> server result ->
//! `notifications/initialized` (notification), after which `tools/list` and
//! `tools/call` are available. Requests carry a monotonic integer `id`; the
//! client matches a response to its request by that id, discarding interleaved
//! server-initiated notifications (e.g. `notifications/message` logging).
//!
//! The client is synchronous: one outstanding request at a time, which is all a
//! deterministic palette-invoked host needs (a human triggers one tool call and
//! croft renders the result). That keeps response correlation a simple
//! recv-until-matching-id loop instead of a pending-request map.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::mcp::transport::McpTransport;

/// The MCP protocol revision croft speaks. The server echoes a (possibly older)
/// `protocolVersion` in its `initialize` result; croft accepts whatever the
/// server negotiates rather than hard-failing on an exact match.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

/// How long a single request waits for its matching response before erroring,
/// so a wedged server can't hang croft's invocation forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Build a JSON-RPC 2.0 request envelope (the transport does not add `id`).
pub fn request(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// Build a JSON-RPC 2.0 notification envelope (no `id`, no response expected).
pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// The `initialize` params: the protocol revision, croft's (empty) client
/// capabilities, and its identity.
pub fn initialize_params(client_name: &str, client_version: &str) -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": client_name, "version": client_version },
    })
}

/// The `tools/call` params: the tool name and its arguments object.
pub fn tools_call_params(name: &str, arguments: Value) -> Value {
    json!({ "name": name, "arguments": arguments })
}

/// Extract the `result` from a JSON-RPC response, turning a JSON-RPC `error`
/// object into an `Err` carrying its message. A response missing both is
/// malformed.
pub fn response_result(message: &Value) -> Result<Value> {
    if let Some(err) = message.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        bail!("MCP error {code}: {msg}");
    }
    message
        .get("result")
        .cloned()
        .context("JSON-RPC response has neither result nor error")
}

/// Whether `message` is the response to request `id` (a JSON-RPC response echoes
/// the request's integer `id`). Server notifications carry no `id` and are thus
/// never matched.
fn is_response_to(message: &Value, id: i64) -> bool {
    message.get("id").and_then(Value::as_i64) == Some(id)
}

/// One tool a server advertises, projected from a `tools/list` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    pub description: String,
}

/// Parse the `tools` array out of a `tools/list` result.
pub fn parse_tools(result: &Value) -> Vec<Tool> {
    result
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| {
                    let name = t.get("name")?.as_str()?.to_string();
                    let description = t
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    Some(Tool { name, description })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Flatten a `tools/call` result's `content` array into plain text. MCP tool
/// content is a list of typed blocks; croft renders the `text` blocks joined by
/// blank lines (the markdown scratch buffer the pilot writes). Non-text blocks
/// (images, resources) are skipped in this first cut.
pub fn tool_result_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

/// A live MCP session: an initialized server plus the id counter for requests.
pub struct McpClient {
    transport: McpTransport,
    next_id: AtomicI64,
}

impl McpClient {
    /// Spawn and initialize an MCP server: handshake then return the ready
    /// client. `env` is the least-privilege environment for the server.
    pub fn connect(
        program: &str,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        client_name: &str,
        client_version: &str,
    ) -> Result<McpClient> {
        let transport = McpTransport::spawn(program, args, cwd, env)?;
        let client = McpClient {
            transport,
            next_id: AtomicI64::new(0),
        };
        client.initialize(client_name, client_version)?;
        Ok(client)
    }

    fn next_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Send a request and block until its matching response arrives (discarding
    /// interleaved server notifications), returning the `result` or an error.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id();
        self.transport.send(&request(id, method, params))?;
        let deadline = std::time::Instant::now() + REQUEST_TIMEOUT;
        loop {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .context("MCP request timed out")?;
            let msg = self
                .transport
                .incoming
                .recv_timeout(remaining)
                .context("MCP server closed or timed out before responding")?;
            if is_response_to(&msg, id) {
                return response_result(&msg);
            }
            // Otherwise a server notification/request: ignore and keep waiting.
        }
    }

    /// Send a fire-and-forget notification.
    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.transport.send(&notification(method, params))
    }

    /// Run the MCP handshake: `initialize`, then the `notifications/initialized`
    /// acknowledgement the spec requires before normal operation.
    fn initialize(&self, client_name: &str, client_version: &str) -> Result<Value> {
        let result = self.call("initialize", initialize_params(client_name, client_version))?;
        self.notify("notifications/initialized", json!({}))?;
        Ok(result)
    }

    /// List the tools the server advertises.
    pub fn list_tools(&self) -> Result<Vec<Tool>> {
        let result = self.call("tools/list", json!({}))?;
        Ok(parse_tools(&result))
    }

    /// Call a tool by name with an arguments object; returns the raw result.
    pub fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        self.call("tools/call", tools_call_params(name, arguments))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_notification_envelopes_are_jsonrpc_2_0() {
        let r = request(1, "tools/list", json!({}));
        assert_eq!(r["jsonrpc"], "2.0");
        assert_eq!(r["id"], 1);
        assert_eq!(r["method"], "tools/list");
        let n = notification("notifications/initialized", json!({}));
        assert_eq!(n["jsonrpc"], "2.0");
        assert_eq!(n["method"], "notifications/initialized");
        assert!(n.get("id").is_none(), "a notification carries no id");
    }

    #[test]
    fn initialize_params_advertise_the_protocol_version_and_client() {
        let p = initialize_params("croft", "0.1.0");
        assert_eq!(p["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(p["clientInfo"]["name"], "croft");
        assert_eq!(p["clientInfo"]["version"], "0.1.0");
    }

    #[test]
    fn response_result_extracts_result_and_maps_error_to_err() {
        let ok = json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}});
        assert_eq!(response_result(&ok).unwrap(), json!({"tools": []}));

        let err = json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "Method not found"}});
        let e = response_result(&err).unwrap_err().to_string();
        assert!(e.contains("-32601"), "{e}");
        assert!(e.contains("Method not found"), "{e}");

        let malformed = json!({"jsonrpc": "2.0", "id": 1});
        assert!(response_result(&malformed).is_err());
    }

    #[test]
    fn is_response_to_matches_only_the_request_id() {
        assert!(is_response_to(&json!({"id": 5, "result": {}}), 5));
        assert!(!is_response_to(&json!({"id": 6, "result": {}}), 5));
        // A server notification has no id and never matches.
        assert!(!is_response_to(
            &json!({"method": "notifications/message"}),
            5
        ));
    }

    #[test]
    fn parse_tools_projects_name_and_description() {
        let result = json!({
            "tools": [
                {"name": "fetch", "description": "Fetch a URL", "inputSchema": {}},
                {"name": "echo"},
            ]
        });
        let tools = parse_tools(&result);
        assert_eq!(tools.len(), 2);
        assert_eq!(
            tools[0],
            Tool {
                name: "fetch".into(),
                description: "Fetch a URL".into()
            }
        );
        assert_eq!(tools[1].name, "echo");
        assert_eq!(
            tools[1].description, "",
            "missing description defaults empty"
        );
    }

    #[test]
    fn tool_result_text_joins_text_content_blocks() {
        let result = json!({
            "content": [
                {"type": "text", "text": "# Heading"},
                {"type": "image", "data": "..."},
                {"type": "text", "text": "body"},
            ]
        });
        assert_eq!(tool_result_text(&result), "# Heading\n\nbody");
    }

    /// End-to-end against a real MCP server process: a tiny hermetic Python
    /// server (stdlib only) that implements `initialize`, `tools/list`, and an
    /// `echo` tool over NDJSON stdio. Proves the full spawn -> handshake ->
    /// tools/list -> tools/call loop on the real wire, no network or npm.
    ///
    /// Ignored by default (needs `python3` on PATH); run with
    /// `cargo test --bin croft -- --ignored mcp_handshake`.
    #[test]
    #[ignore = "requires python3 on PATH"]
    fn mcp_handshake_lists_and_calls_a_tool_against_a_real_server() {
        use std::io::Write;
        const SERVER: &str = r#"
import sys, json
def reply(i, result):
    sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":i,"result":result}) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    m = msg.get("method")
    if m == "initialize":
        reply(msg["id"], {"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"mock","version":"0"}})
    elif m == "notifications/initialized":
        pass
    elif m == "tools/list":
        reply(msg["id"], {"tools":[{"name":"echo","description":"Echo text"}]})
    elif m == "tools/call":
        text = msg["params"]["arguments"].get("text","")
        reply(msg["id"], {"content":[{"type":"text","text":text}]})
    elif "id" in msg:
        reply(msg["id"], {})
"#;
        let dir = std::env::temp_dir().join(format!("croft-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("server.py");
        let mut f = std::fs::File::create(&script).unwrap();
        f.write_all(SERVER.as_bytes()).unwrap();
        drop(f);

        let env = BTreeMap::new();
        let client = McpClient::connect(
            "python3",
            &[script.to_string_lossy().into_owned()],
            &dir,
            &env,
            "croft",
            "test",
        )
        .expect("connect + initialize against the mock MCP server");

        let tools = client.list_tools().expect("tools/list");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");

        let result = client
            .call_tool("echo", json!({"text": "hello mcp"}))
            .expect("tools/call echo");
        assert_eq!(tool_result_text(&result), "hello mcp");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
