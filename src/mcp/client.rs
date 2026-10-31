//! MCP client: JSON-RPC 2.0 envelope + the MCP lifecycle over a [`McpTransport`].
//!
//! croft is a dual-era client. It opens every server with a `server/discover`
//! probe carrying [`MODERN_VERSION`] in `_meta`. A server that answers it is
//! modern: there is no handshake, and every later request carries the protocol
//! version, croft's identity and its capabilities in its own `_meta`. A server
//! that rejects the probe with anything but a modern error, or stays silent, is
//! legacy: croft falls back to `initialize` (request) -> server result ->
//! `notifications/initialized` (notification), after which plain `tools/list`
//! and `tools/call` work. The era is settled once per server process.
//!
//! Requests carry a monotonic integer `id`; the client matches a response to its
//! request by that id, discarding interleaved server-initiated notifications
//! (e.g. `notifications/message` logging).
//!
//! The client is synchronous: one outstanding request at a time, which is all a
//! deterministic palette-invoked host needs (a human triggers one tool call and
//! croft renders the result). That keeps response correlation a simple
//! recv-until-matching-id loop instead of a pending-request map.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use crate::mcp::transport::McpTransport;

/// The modern MCP revision croft prefers: version, identity and capabilities
/// travel in every request's `_meta`, and there is no `initialize` handshake.
pub const MODERN_VERSION: &str = "2026-07-28";

/// The legacy revision croft asks for in `initialize` when a server predates
/// [`MODERN_VERSION`]. The server echoes a (possibly older) `protocolVersion`;
/// croft accepts whatever it negotiates rather than hard-failing on a mismatch.
pub const LEGACY_VERSION: &str = "2025-11-25";

/// Every revision croft speaks, newest first: what an
/// `UnsupportedProtocolVersionError` lists as `supported`.
pub const SUPPORTED_VERSIONS: [&str; 2] = [MODERN_VERSION, LEGACY_VERSION];

/// The `_meta` keys a modern request carries.
pub const PROTOCOL_VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
pub const CLIENT_INFO_KEY: &str = "io.modelcontextprotocol/clientInfo";
pub const CLIENT_CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
/// The `_meta` key a modern result names its server under.
pub const SERVER_INFO_KEY: &str = "io.modelcontextprotocol/serverInfo";

/// A modern server that does not serve the requested version (`data.supported`
/// lists the ones it does).
pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
/// A modern server that needs a client capability croft did not declare.
const MISSING_REQUIRED_CLIENT_CAPABILITY: i64 = -32021;

/// How long a single request waits for its matching response before erroring,
/// so a wedged server can't hang croft's invocation forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the `server/discover` probe waits before croft treats the server
/// as legacy: some legacy servers ignore a request that arrives before
/// `initialize`. A modern server merely slow to start is still caught, by its
/// late reply to the probe beside its rejection of `initialize`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a request whose server died waits for the rest of its stderr.
const EXIT_REASON_WAIT: Duration = Duration::from_millis(500);

/// Build a JSON-RPC 2.0 request envelope (the transport does not add `id`).
pub fn request(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// Build a JSON-RPC 2.0 notification envelope (no `id`, no response expected).
pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// The legacy `initialize` params: the protocol revision, croft's (empty)
/// client capabilities, and its identity.
pub fn initialize_params(client_name: &str, client_version: &str) -> Value {
    json!({
        "protocolVersion": LEGACY_VERSION,
        "capabilities": {},
        "clientInfo": { "name": client_name, "version": client_version },
    })
}

/// The `_meta` every modern request carries: the protocol revision, croft's
/// identity, and its (empty) capabilities.
pub fn request_meta(client_name: &str, client_version: &str) -> Map<String, Value> {
    let mut meta = Map::new();
    meta.insert(PROTOCOL_VERSION_KEY.into(), json!(MODERN_VERSION));
    meta.insert(
        CLIENT_INFO_KEY.into(),
        json!({ "name": client_name, "version": client_version }),
    );
    meta.insert(CLIENT_CAPABILITIES_KEY.into(), json!({}));
    meta
}

/// `params` with `meta` merged into its `_meta`, keeping any keys the caller
/// already set there (e.g. a `progressToken`).
pub fn with_meta(params: Value, meta: &Map<String, Value>) -> Value {
    let mut params = match params {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let mut slot = match params.remove("_meta") {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    slot.extend(meta.clone());
    params.insert("_meta".into(), Value::Object(slot));
    Value::Object(params)
}

/// The `tools/call` params: the tool name and its arguments object.
pub fn tools_call_params(name: &str, arguments: Value) -> Value {
    json!({ "name": name, "arguments": arguments })
}

/// Extract the `result` from a JSON-RPC response, turning a JSON-RPC `error`
/// object into an `Err` carrying its message. A response missing both is
/// malformed. A modern result names its `resultType`; croft declares no client
/// capabilities, so only `complete` (or a legacy result's absent type) is one
/// it can use.
pub fn response_result(message: &Value) -> Result<Value> {
    if let Some(err) = message.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        bail!("MCP error {code}: {msg}");
    }
    let result = message
        .get("result")
        .cloned()
        .context("JSON-RPC response has neither result nor error")?;
    match result.get("resultType").and_then(Value::as_str) {
        None | Some("complete") => Ok(result),
        Some(other) => bail!("the server answered with an unsupported result type `{other}`"),
    }
}

/// How croft talks to a server, settled once per server process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    /// [`MODERN_VERSION`]: every request carries its own `_meta`.
    Modern,
    /// An `initialize` handshake first, then plain requests.
    Legacy,
}

/// The era to use with a server advertising `versions`: modern if it speaks
/// [`MODERN_VERSION`], legacy if it speaks any older revision (`initialize`
/// negotiates which), and an error naming both sides when there is none.
fn era_for(versions: &[Value]) -> Result<Era> {
    let versions: Vec<&str> = versions.iter().filter_map(Value::as_str).collect();
    if versions.contains(&MODERN_VERSION) {
        Ok(Era::Modern)
    } else if versions.iter().any(|v| *v < MODERN_VERSION) {
        Ok(Era::Legacy)
    } else {
        bail!(
            "the server speaks MCP {}, and croft speaks {}",
            versions.join(", "),
            SUPPORTED_VERSIONS.join(", ")
        )
    }
}

/// What a server's reply to the `server/discover` probe says about its era.
/// A discover result, or a modern error, comes from a modern server; any other
/// error comes from a legacy one, whatever its code (legacy servers reject an
/// unknown pre-`initialize` request in their own ways).
pub fn classify_probe(reply: &Value) -> Result<Era> {
    if let Some(versions) = reply
        .pointer("/result/supportedVersions")
        .and_then(Value::as_array)
    {
        return era_for(versions);
    }
    match reply.pointer("/error/code").and_then(Value::as_i64) {
        Some(UNSUPPORTED_PROTOCOL_VERSION) => era_for(
            reply
                .pointer("/error/data/supported")
                .and_then(Value::as_array)
                .map_or(&[], Vec::as_slice),
        ),
        Some(MISSING_REQUIRED_CLIENT_CAPABILITY) => bail!(
            "the server needs client capabilities croft does not offer: {}",
            reply
                .pointer("/error/data/requiredCapabilities")
                .unwrap_or(&Value::Null)
        ),
        _ => Ok(Era::Legacy),
    }
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

/// A stable fingerprint of a named tool's definition (name + description + input
/// schema) taken from a raw `tools/list` result, for trust-on-first-use rug-pull
/// detection: croft records the fingerprint the first time it calls a tool and
/// refuses to call again if the definition silently changed (the MCPoison /
/// tool-mutation class). The compact JSON of the canonical fields is the
/// fingerprint — serde_json orders object keys deterministically, so it is
/// stable across runs without a crypto dependency. Empty string if not found.
pub fn tool_fingerprint(tools_list_result: &Value, name: &str) -> String {
    let Some(entry) = tools_list_result
        .get("tools")
        .and_then(Value::as_array)
        .and_then(|a| {
            a.iter()
                .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
        })
    else {
        return String::new();
    };
    json!({
        "name": entry.get("name").cloned().unwrap_or(Value::Null),
        "description": entry.get("description").cloned().unwrap_or(Value::Null),
        "inputSchema": entry.get("inputSchema").cloned().unwrap_or(Value::Null),
    })
    .to_string()
}

/// A live MCP session: a server whose era is settled, plus the id counter for
/// requests.
pub struct McpClient {
    transport: McpTransport,
    next_id: AtomicI64,
    era: Era,
    client_name: String,
    client_version: String,
}

impl McpClient {
    /// Spawn an MCP server and settle its era (see the module docs), returning
    /// the ready client. `env` is the least-privilege environment for the server.
    pub fn connect(
        program: &str,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        client_name: &str,
        client_version: &str,
    ) -> Result<McpClient> {
        let mut client = McpClient::spawn(program, args, cwd, env, client_name, client_version)?;
        client.handshake(PROBE_TIMEOUT)?;
        Ok(client)
    }

    /// Spawn the server without talking to it yet.
    fn spawn(
        program: &str,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        client_name: &str,
        client_version: &str,
    ) -> Result<McpClient> {
        Ok(McpClient {
            transport: McpTransport::spawn(program, args, cwd, env)?,
            next_id: AtomicI64::new(0),
            era: Era::Legacy,
            client_name: client_name.to_string(),
            client_version: client_version.to_string(),
        })
    }

    fn next_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn meta(&self) -> Map<String, Value> {
        request_meta(&self.client_name, &self.client_version)
    }

    /// Write a request, returning its id. A server that already died closed
    /// its stdin: say why it died.
    fn send(&self, method: &str, params: Value) -> Result<i64> {
        let id = self.next_id();
        if let Err(e) = self.transport.send(&request(id, method, params)) {
            let broken_pipe = e
                .root_cause()
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe);
            if broken_pipe {
                bail!("{}", self.transport.exit_reason(EXIT_REASON_WAIT));
            }
            return Err(e);
        }
        Ok(id)
    }

    /// Wait up to `timeout` for the reply to any of `ids`, discarding server
    /// notifications. `None` when the time runs out.
    fn wait(&self, ids: &[i64], timeout: Duration) -> Result<Option<Value>> {
        let deadline = Instant::now() + timeout;
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(None);
            };
            match self.transport.incoming.recv_timeout(remaining) {
                Ok(msg) if ids.iter().any(|&id| is_response_to(&msg, id)) => return Ok(Some(msg)),
                // A server notification: ignore and keep waiting.
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                // stdout closed: the server died, and its stderr says why.
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("{}", self.transport.exit_reason(EXIT_REASON_WAIT))
                }
            }
        }
    }

    /// Send a request and block until its matching response arrives, returning
    /// the `result` or an error. A modern server gets croft's `_meta` on it.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let params = match self.era {
            Era::Modern => with_meta(params, &self.meta()),
            Era::Legacy => params,
        };
        let id = self.send(method, params)?;
        let reply = self
            .wait(&[id], REQUEST_TIMEOUT)?
            .context("MCP server timed out before responding")?;
        response_result(&reply)
    }

    /// Send a fire-and-forget notification.
    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.transport.send(&notification(method, params))
    }

    /// Settle the server's era: probe with `server/discover`, and fall back to
    /// the legacy `initialize` handshake unless the server proves modern.
    fn handshake(&mut self, probe_timeout: Duration) -> Result<()> {
        let probe = self.send("server/discover", json!({ "_meta": self.meta() }))?;
        if let Some(reply) = self.wait(&[probe], probe_timeout)? {
            self.era = classify_probe(&reply)?;
            if self.era == Era::Modern {
                return Ok(());
            }
        }
        let init = self.send(
            "initialize",
            initialize_params(&self.client_name, &self.client_version),
        )?;
        let mut late_probe = None;
        loop {
            let reply = self
                .wait(&[probe, init], REQUEST_TIMEOUT)?
                .context("MCP server timed out before responding")?;
            if is_response_to(&reply, probe) {
                late_probe = Some(reply);
                continue;
            }
            return match response_result(&reply) {
                Ok(_) => {
                    self.era = Era::Legacy;
                    self.notify("notifications/initialized", json!({}))
                }
                // A modern server only slow to answer the probe rejects
                // `initialize`, and its late discover reply says it is modern.
                Err(e) => match late_probe.as_ref().map(classify_probe) {
                    Some(Ok(Era::Modern)) => {
                        self.era = Era::Modern;
                        Ok(())
                    }
                    Some(Err(why)) => Err(why),
                    _ => Err(e),
                },
            };
        }
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
        assert_eq!(p["protocolVersion"], LEGACY_VERSION);
        assert_eq!(p["clientInfo"]["name"], "croft");
        assert_eq!(p["clientInfo"]["version"], "0.1.0");
    }

    /// A modern request carries version, identity and capabilities in
    /// `_meta`, beside the caller's params and any `_meta` keys it set.
    #[test]
    fn with_meta_adds_the_modern_fields_and_keeps_the_callers() {
        let meta = request_meta("croft", "0.1.0");
        let p = with_meta(
            json!({"name": "echo", "_meta": {"progressToken": 7}}),
            &meta,
        );
        assert_eq!(p["name"], "echo");
        assert_eq!(p["_meta"]["progressToken"], 7);
        assert_eq!(p["_meta"][PROTOCOL_VERSION_KEY], MODERN_VERSION);
        assert_eq!(p["_meta"][CLIENT_INFO_KEY]["name"], "croft");
        assert_eq!(p["_meta"][CLIENT_CAPABILITIES_KEY], json!({}));
        assert_eq!(with_meta(Value::Null, &meta)["_meta"], Value::Object(meta));
    }

    /// The probe's reply settles the era: a discover result or a modern error
    /// is modern, any other error is legacy whatever its code.
    #[test]
    fn classify_probe_reads_the_era_off_the_reply() {
        let discover = |v: Value| json!({"id": 1, "result": {"supportedVersions": v}});
        let error = |code: i64, data: Value| json!({"id": 1, "error": {"code": code, "message": "m", "data": data}});
        let era = |reply: Value| classify_probe(&reply).unwrap();

        assert_eq!(
            era(discover(json!([MODERN_VERSION, LEGACY_VERSION]))),
            Era::Modern
        );
        // Modern, but its versions are older than croft's: `initialize` it.
        assert_eq!(era(discover(json!(["2025-06-18"]))), Era::Legacy);
        assert_eq!(
            era(error(-32022, json!({"supported": [LEGACY_VERSION]}))),
            Era::Legacy
        );
        // Legacy servers reject the unknown method in their own ways.
        assert_eq!(era(error(-32601, Value::Null)), Era::Legacy);
        assert_eq!(era(error(-32602, Value::Null)), Era::Legacy);
        assert_eq!(era(json!({"id": 1, "result": {}})), Era::Legacy);

        let none = classify_probe(&error(-32022, json!({"supported": ["2099-01-01"]})))
            .unwrap_err()
            .to_string();
        assert!(
            none.contains("2099-01-01") && none.contains(MODERN_VERSION),
            "{none}"
        );
        let caps = classify_probe(&error(
            -32021,
            json!({"requiredCapabilities": {"roots": {}}}),
        ))
        .unwrap_err()
        .to_string();
        assert!(caps.contains("roots"), "{caps}");
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

        let complete = json!({"id": 1, "result": {"resultType": "complete", "tools": []}});
        assert!(response_result(&complete).is_ok());
        let input = json!({"id": 1, "result": {"resultType": "input_required"}});
        let e = response_result(&input).unwrap_err().to_string();
        assert!(e.contains("input_required"), "{e}");
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
    fn tool_fingerprint_is_stable_and_changes_with_the_definition() {
        let list = json!({"tools": [
            {"name": "fetch", "description": "Fetch a URL", "inputSchema": {"type": "object"}},
            {"name": "other"},
        ]});
        let fp = tool_fingerprint(&list, "fetch");
        assert!(!fp.is_empty());
        // Stable across calls on the same definition.
        assert_eq!(fp, tool_fingerprint(&list, "fetch"));
        // A mutated description (a rug-pull) changes the fingerprint.
        let mutated = json!({"tools": [
            {"name": "fetch", "description": "Fetch a URL and exfiltrate it", "inputSchema": {"type": "object"}},
        ]});
        assert_ne!(fp, tool_fingerprint(&mutated, "fetch"));
        // Unknown tool fingerprints empty.
        assert_eq!(tool_fingerprint(&list, "nope"), "");
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

    /// The env a test sidecar runs with: just PATH, so `/bin/sh` finds
    /// `head` and `tr`.
    fn path_env() -> BTreeMap<String, String> {
        std::env::var("PATH")
            .map(|p| BTreeMap::from([(String::from("PATH"), p)]))
            .unwrap_or_default()
    }

    /// A sidecar that crashes on import (the pinned mcp-server-time under
    /// mcp 2.x) must say why, not report a generic timeout (#1297).
    #[test]
    fn a_server_that_dies_before_answering_names_its_error() {
        let script = "echo 'Traceback (most recent call last):' >&2; \
             echo \"ImportError: cannot import name 'McpError' from 'mcp.shared.exceptions'\" >&2; \
             echo >&2; exit 1";
        let err = McpClient::connect(
            "/bin/sh",
            &[String::from("-c"), script.to_string()],
            &std::env::temp_dir(),
            &path_env(),
            "croft",
            "test",
        )
        .err()
        .expect("a server that exits never connects")
        .to_string();
        assert_eq!(
            err,
            "the server exited: ImportError: cannot import name 'McpError' from 'mcp.shared.exceptions'"
        );
    }

    /// A server that dies without a word still says it exited, rather than
    /// that it timed out.
    #[test]
    fn a_silent_server_exit_is_reported_as_an_exit() {
        let err = McpClient::connect(
            "/bin/sh",
            &[String::from("-c"), String::from("exit 3")],
            &std::env::temp_dir(),
            &path_env(),
            "croft",
            "test",
        )
        .err()
        .expect("a server that exits never connects")
        .to_string();
        assert_eq!(err, "the server exited before responding");
    }

    /// Negative: a server that logs far past the 64 KiB pipe buffer before
    /// answering still answers, since stderr is drained, not left to fill.
    #[test]
    fn a_server_logging_past_the_pipe_buffer_still_answers() {
        let script = format!(
            "head -c 300000 /dev/zero | tr '\\000' x >&2; read l; echo '{DISCOVERED}'; cat >/dev/null"
        );
        McpClient::connect(
            "/bin/sh",
            &[String::from("-c"), script],
            &std::env::temp_dir(),
            &path_env(),
            "croft",
            "test",
        )
        .expect("a chatty server still completes the handshake");
    }

    /// A modern server's reply to the probe (id 1).
    const DISCOVERED: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}}}}"#;

    /// A stand-in server running `script` under `/bin/sh`, its era settled by
    /// a probe that gives up after `probe_timeout`.
    fn connect_sh(script: &str, probe_timeout: Duration) -> Result<McpClient> {
        let args = [String::from("-c"), script.to_string()];
        let mut client = McpClient::spawn(
            "/bin/sh",
            &args,
            &std::env::temp_dir(),
            &path_env(),
            "croft",
            "test",
        )?;
        client.handshake(probe_timeout)?;
        Ok(client)
    }

    /// A modern server answers the probe and gets no `initialize`: the next
    /// line it reads is the request, carrying croft's `_meta`.
    #[test]
    fn a_modern_server_is_spoken_to_with_per_request_meta() {
        let script = format!(
            r#"read l; echo '{DISCOVERED}'; read l
            case "$l" in *'"method":"tools/list"'*'"io.modelcontextprotocol/protocolVersion":"2026-07-28"'*)
              echo '{{"jsonrpc":"2.0","id":2,"result":{{"resultType":"complete","tools":[{{"name":"echo"}}]}}}}';;
            *) echo "no modern tools/list: $l" >&2; exit 1;; esac; cat >/dev/null"#
        );
        let client = connect_sh(&script, PROBE_TIMEOUT).expect("the probe settles modern");
        assert_eq!(client.era, Era::Modern);
        let list = client.call("tools/list", json!({})).expect("tools/list");
        assert_eq!(parse_tools(&list)[0].name, "echo");
    }

    /// A legacy server rejects the probe and gets the `initialize` handshake,
    /// then plain requests with no `_meta`.
    #[test]
    fn a_legacy_server_that_rejects_the_probe_gets_initialize() {
        let script = r#"read l; echo '{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}'
            read l; case "$l" in *'"method":"initialize"'*) ;; *) exit 1;; esac
            echo '{"jsonrpc":"2.0","id":2,"result":{"protocolVersion":"2025-11-25","capabilities":{}}}'
            read l; case "$l" in *notifications/initialized*) ;; *) exit 1;; esac
            read l; case "$l" in *_meta*) exit 1;; esac
            echo '{"jsonrpc":"2.0","id":3,"result":{"tools":[]}}'; cat >/dev/null"#;
        let client = connect_sh(script, PROBE_TIMEOUT).expect("falls back to initialize");
        assert_eq!(client.era, Era::Legacy);
        client.call("tools/list", json!({})).expect("tools/list");
    }

    /// A legacy server that ignores the probe is legacy once it times out.
    #[test]
    fn a_legacy_server_silent_on_the_probe_gets_initialize() {
        let script = r#"read l; read l; case "$l" in *'"method":"initialize"'*) ;; *) exit 1;; esac
            echo '{"jsonrpc":"2.0","id":2,"result":{"protocolVersion":"2025-11-25","capabilities":{}}}'
            cat >/dev/null"#;
        let client =
            connect_sh(script, Duration::from_millis(100)).expect("falls back to initialize");
        assert_eq!(client.era, Era::Legacy);
    }

    /// A modern server too slow for the probe window answers it late, then
    /// rejects the fallback `initialize`: croft still ends up modern.
    #[test]
    fn a_modern_server_slow_to_answer_the_probe_is_still_modern() {
        let script = format!(
            r#"read l; sleep 1; echo '{DISCOVERED}'; read l
            echo '{{"jsonrpc":"2.0","id":2,"error":{{"code":-32601,"message":"Method not found"}}}}'
            cat >/dev/null"#
        );
        let client = connect_sh(&script, Duration::from_millis(100)).expect("recovers modern");
        assert_eq!(client.era, Era::Modern);
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

        let list = client.call("tools/list", json!({})).expect("tools/list");
        let tools = parse_tools(&list);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");

        let result = client
            .call_tool("echo", json!({"text": "hello mcp"}))
            .expect("tools/call echo");
        assert_eq!(tool_result_text(&result), "hello mcp");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
