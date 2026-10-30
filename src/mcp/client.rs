//! MCP client: JSON-RPC 2.0 envelope + the MCP lifecycle over a [`McpTransport`].
//!
//! The client is dual-era. It first probes with `server/discover` at the
//! stateless 2026-07-28 revision: a server that answers is modern, and every
//! later request names that revision, croft's identity and its capabilities in
//! `_meta`, with no handshake. A server that rejects the probe, or stays silent
//! past [`PROBE_TIMEOUT`], predates it and gets the 2025-11-25 handshake:
//! `initialize` (request) -> server result -> `notifications/initialized`
//! (notification). Either way `tools/list` and `tools/call` follow. Requests
//! carry a monotonic integer `id`; the client matches a response to its request
//! by that id, discarding interleaved server-initiated notifications (e.g.
//! `notifications/message` logging).
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
use serde_json::{Value, json};

use crate::mcp::transport::McpTransport;

/// The stateless MCP revision croft prefers: a server that answers
/// `server/discover` with it gets it named in every request's `_meta`.
pub const PROTOCOL_VERSION: &str = "2026-07-28";

/// The handshake-era revision croft sends in `initialize` to a server that
/// predates [`PROTOCOL_VERSION`]. The server echoes a (possibly older)
/// `protocolVersion` in its result; croft accepts whatever the server
/// negotiates rather than hard-failing on an exact match.
pub const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";

/// Every handshake-era revision. A stateless server that refuses
/// [`PROTOCOL_VERSION`] but lists one of these still answers `initialize`.
const HANDSHAKE_VERSIONS: [&str; 4] = [
    "2024-11-05",
    "2025-03-26",
    "2025-06-18",
    LEGACY_PROTOCOL_VERSION,
];

/// The `_meta` key a stateless request names its revision under.
pub const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";

/// The `_meta` key a stateless request names its client under.
pub const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";

/// The `_meta` key a stateless request declares its client capabilities under.
pub const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";

/// The `_meta` key a stateless result names its server under.
pub const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// `UnsupportedProtocolVersion`: the request's revision is one the server does
/// not speak, and `data.supported` lists the ones it does.
pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// The stateless revision's own error codes (`HeaderMismatch`,
/// `MissingRequiredClientCapability`, `UnsupportedProtocolVersion`). Any of
/// them in reply to the probe marks a modern server, which `initialize` would
/// not reach.
const STATELESS_ERROR_CODES: [i64; 3] = [-32020, -32021, UNSUPPORTED_PROTOCOL_VERSION];

/// How long the `server/discover` probe waits before taking silence for a
/// handshake-era server (some drop any request sent before `initialize`). It
/// is the C# SDK's default. A modern server slower than this is not lost: its
/// late answer still counts while croft awaits `initialize`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a single request waits for its matching response before erroring,
/// so a wedged server can't hang croft's invocation forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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

/// The `initialize` params: the protocol revision, croft's (empty) client
/// capabilities, and its identity.
pub fn initialize_params(client_name: &str, client_version: &str) -> Value {
    json!({
        "protocolVersion": LEGACY_PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": client_name, "version": client_version },
    })
}

/// The `_meta` every stateless request carries in place of a handshake: the
/// revision, croft's identity, and its client capabilities (none, so a server
/// never asks croft for input mid-request).
pub fn request_meta(client_name: &str, client_version: &str) -> Value {
    json!({
        META_PROTOCOL_VERSION: PROTOCOL_VERSION,
        META_CLIENT_INFO: { "name": client_name, "version": client_version },
        META_CLIENT_CAPABILITIES: {},
    })
}

/// The `tools/call` params: the tool name and its arguments object.
pub fn tools_call_params(name: &str, arguments: Value) -> Value {
    json!({ "name": name, "arguments": arguments })
}

/// Extract the `result` from a JSON-RPC response, turning a JSON-RPC `error`
/// object into an `Err` carrying its message. A response missing both is
/// malformed. So is a result whose `resultType` is not `complete`: croft
/// declares no capabilities, so it cannot answer an `input_required` round
/// trip. A handshake-era result carries no `resultType`, which means complete.
pub fn response_result(message: &Value) -> Result<Value> {
    if let Some(err) = message.get("error") {
        return Err(rpc_error(err));
    }
    let result = message
        .get("result")
        .cloned()
        .context("JSON-RPC response has neither result nor error")?;
    match result.get("resultType") {
        None => Ok(result),
        Some(kind) if kind == "complete" => Ok(result),
        Some(kind) => {
            bail!("the server answered with resultType {kind}, which croft cannot act on")
        }
    }
}

/// A JSON-RPC `error` object as an error carrying its code and message.
fn rpc_error(err: &Value) -> anyhow::Error {
    let msg = err
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
    anyhow::anyhow!("MCP error {code}: {msg}")
}

/// What a `server/discover` reply says about the server: `true` for a
/// stateless server that speaks [`PROTOCOL_VERSION`], `false` for anything
/// that is no evidence of one, which falls back to `initialize`. That covers
/// any error outside the stateless codes (handshake-era servers answer an
/// unknown method with whatever code they like, or a mangled result), and a
/// server whose versions include a handshake-era one. A stateless server that
/// shares no revision with croft is an error, since `initialize` cannot reach
/// it either.
fn probe_says_modern(reply: &Value) -> Result<bool> {
    let supported = match reply.get("error") {
        None => {
            let supported = string_list(reply.pointer("/result/supportedVersions"));
            if supported.contains(&PROTOCOL_VERSION) {
                return Ok(true);
            }
            supported
        }
        Some(err) => {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            if !STATELESS_ERROR_CODES.contains(&code) {
                return Ok(false);
            }
            let supported = string_list(err.pointer("/data/supported"));
            if supported.is_empty() {
                return Err(rpc_error(err));
            }
            supported
        }
    };
    if supported.is_empty() || supported.iter().any(|v| HANDSHAKE_VERSIONS.contains(v)) {
        return Ok(false);
    }
    bail!(
        "the server speaks MCP {} only, and croft speaks {PROTOCOL_VERSION} or {LEGACY_PROTOCOL_VERSION}",
        supported.join(", ")
    )
}

/// The strings in a JSON array, such as a list of protocol versions.
fn string_list(value: Option<&Value>) -> Vec<&str> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
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

/// Which revision a connected server speaks, settled once per process by the
/// probe in [`McpClient::connect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Era {
    /// Stateless: every request names [`PROTOCOL_VERSION`] in its `_meta`.
    Modern,
    /// Handshake: `initialize` ran, and requests carry no protocol `_meta`.
    Legacy,
}

/// A live MCP session: a connected server, its era, and the id counter for
/// requests.
pub struct McpClient {
    transport: McpTransport,
    next_id: AtomicI64,
    era: Era,
    /// What a stateless request carries in `_meta` ([`request_meta`]).
    meta: Value,
}

impl McpClient {
    /// Spawn an MCP server and settle its era (probe, then `initialize` for a
    /// handshake-era server), returning the ready client. `env` is the
    /// least-privilege environment for the server.
    pub fn connect(
        program: &str,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        client_name: &str,
        client_version: &str,
    ) -> Result<McpClient> {
        Self::connect_probing_for(
            program,
            args,
            cwd,
            env,
            client_name,
            client_version,
            PROBE_TIMEOUT,
        )
    }

    /// [`McpClient::connect`] with the probe waiting `probe_timeout`.
    fn connect_probing_for(
        program: &str,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        client_name: &str,
        client_version: &str,
        probe_timeout: Duration,
    ) -> Result<McpClient> {
        let transport = McpTransport::spawn(program, args, cwd, env)?;
        let mut client = McpClient {
            transport,
            next_id: AtomicI64::new(0),
            era: Era::Legacy,
            meta: request_meta(client_name, client_version),
        };
        client.era = client.negotiate(client_name, client_version, probe_timeout)?;
        Ok(client)
    }

    fn next_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Send a request in the negotiated era and block until its matching
    /// response arrives, returning the `result` or an error.
    pub fn call(&self, method: &str, mut params: Value) -> Result<Value> {
        if self.era == Era::Modern {
            params["_meta"] = self.meta.clone();
        }
        let id = self.send_request(method, params)?;
        let (_, reply) = self
            .await_response(&[id], REQUEST_TIMEOUT)?
            .context("MCP server timed out before responding")?;
        response_result(&reply)
    }

    /// Send a request under a fresh id, returning that id.
    fn send_request(&self, method: &str, params: Value) -> Result<i64> {
        let id = self.next_id();
        if let Err(e) = self.transport.send(&request(id, method, params)) {
            // A server that already died closed its stdin: say why it died.
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

    /// Block until the response to one of `ids` arrives (discarding interleaved
    /// server notifications), returning which id it answers, or `None` once
    /// `timeout` passes without one.
    fn await_response(&self, ids: &[i64], timeout: Duration) -> Result<Option<(i64, Value)>> {
        let deadline = Instant::now() + timeout;
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(None);
            };
            let msg = match self.transport.incoming.recv_timeout(remaining) {
                Ok(msg) => msg,
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                // stdout closed: the server died, and its stderr says why.
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("{}", self.transport.exit_reason(EXIT_REASON_WAIT))
                }
            };
            if let Some(&id) = ids.iter().find(|&&id| is_response_to(&msg, id)) {
                return Ok(Some((id, msg)));
            }
            // Otherwise a server notification/request: ignore and keep waiting.
        }
    }

    /// Send a fire-and-forget notification.
    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.transport.send(&notification(method, params))
    }

    /// Settle the server's era the way the stdio binding prescribes: probe with
    /// `server/discover`, and when the reply is no evidence of a stateless
    /// server ([`probe_says_modern`]) or none comes within `probe_timeout`, run
    /// the handshake: `initialize`, then the `notifications/initialized`
    /// acknowledgement. The probe's answer still counts while `initialize` is
    /// pending, so a modern server that took longer than the probe to start is
    /// not misread as handshake-era.
    fn negotiate(
        &self,
        client_name: &str,
        client_version: &str,
        probe_timeout: Duration,
    ) -> Result<Era> {
        let probe = self.send_request("server/discover", json!({ "_meta": self.meta }))?;
        if let Some((_, reply)) = self.await_response(&[probe], probe_timeout)?
            && probe_says_modern(&reply)?
        {
            return Ok(Era::Modern);
        }
        let init =
            self.send_request("initialize", initialize_params(client_name, client_version))?;
        loop {
            let (id, reply) = self
                .await_response(&[probe, init], REQUEST_TIMEOUT)?
                .context("MCP server timed out before responding")?;
            if id == init {
                response_result(&reply)?;
                self.notify("notifications/initialized", json!({}))?;
                return Ok(Era::Legacy);
            }
            if probe_says_modern(&reply)? {
                return Ok(Era::Modern);
            }
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
        assert_eq!(p["protocolVersion"], LEGACY_PROTOCOL_VERSION);
        assert_eq!(p["clientInfo"]["name"], "croft");
        assert_eq!(p["clientInfo"]["version"], "0.1.0");
    }

    /// A stateless request carries in `_meta` what the handshake used to
    /// establish, under the spec's reserved keys.
    #[test]
    fn request_meta_names_the_revision_client_and_capabilities() {
        let meta = request_meta("croft", "0.1.0");
        assert_eq!(
            meta,
            json!({
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {"name": "croft", "version": "0.1.0"},
                "io.modelcontextprotocol/clientCapabilities": {},
            })
        );
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

    /// A stateless result says it is complete; one asking for input croft
    /// never declared it could give, or of a type croft does not know, is an
    /// error rather than an empty answer.
    #[test]
    fn response_result_accepts_only_complete_results() {
        let complete = json!({"id": 1, "result": {"resultType": "complete", "tools": []}});
        assert_eq!(response_result(&complete).unwrap()["tools"], json!([]));

        let input =
            json!({"id": 1, "result": {"resultType": "input_required", "inputRequests": {}}});
        let e = response_result(&input).unwrap_err().to_string();
        assert!(e.contains("\"input_required\""), "{e}");

        let unknown = json!({"id": 1, "result": {"resultType": "someday"}});
        assert!(response_result(&unknown).is_err());
    }

    /// The probe's verdict on each reply the stdio binding names: a discover
    /// result is modern, any non-stateless error or unreadable result is a
    /// handshake-era server, and a stateless refusal falls back only when the
    /// server lists a handshake revision.
    #[test]
    fn the_probe_falls_back_only_without_evidence_of_a_stateless_server() {
        let result = |r: Value| json!({"jsonrpc": "2.0", "id": 1, "result": r});
        let error = |e: Value| json!({"jsonrpc": "2.0", "id": 1, "error": e});

        let discovered = result(json!({
            "resultType": "complete",
            "supportedVersions": ["2026-07-28", "2025-11-25"],
            "capabilities": {"tools": {}},
            "ttlMs": 0,
            "cacheScope": "public",
        }));
        assert!(probe_says_modern(&discovered).unwrap());

        // Handshake-era servers: no fallback is keyed to one code.
        for code in [-32601, -32602, -32600, -32000] {
            let rejected = error(json!({"code": code, "message": "nope"}));
            assert!(!probe_says_modern(&rejected).unwrap(), "{code}");
        }
        assert!(!probe_says_modern(&result(json!({}))).unwrap());
        // Answers discover but advertises handshake revisions only.
        let handshake_only = result(json!({"supportedVersions": ["2025-06-18"]}));
        assert!(!probe_says_modern(&handshake_only).unwrap());

        // Dual-era but refusing the stateless revision: `initialize` works.
        let refused = error(json!({
            "code": -32022,
            "message": "Unsupported protocol version",
            "data": {"supported": ["2025-11-25"], "requested": "2026-07-28"},
        }));
        assert!(!probe_says_modern(&refused).unwrap());

        // Stateless only, and not a revision croft speaks: say so.
        let disjoint = error(json!({
            "code": -32022,
            "message": "Unsupported protocol version",
            "data": {"supported": ["2027-03-01"], "requested": "2026-07-28"},
        }));
        let e = probe_says_modern(&disjoint).unwrap_err().to_string();
        assert_eq!(
            e,
            "the server speaks MCP 2027-03-01 only, and croft speaks 2026-07-28 or 2025-11-25"
        );
        let future_only = result(json!({"supportedVersions": ["2027-03-01"]}));
        assert!(probe_says_modern(&future_only).is_err());
        let capability = error(json!({"code": -32021, "message": "needs roots"}));
        let e = probe_says_modern(&capability).unwrap_err().to_string();
        assert_eq!(e, "MCP error -32021: needs roots");
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
        let script = "head -c 300000 /dev/zero | tr '\\000' x >&2; read l; \
             printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"supportedVersions\":[\"2026-07-28\"]}}'; \
             cat >/dev/null";
        McpClient::connect(
            "/bin/sh",
            &[String::from("-c"), script.to_string()],
            &std::env::temp_dir(),
            &path_env(),
            "croft",
            "test",
        )
        .expect("a chatty server still completes the handshake");
    }

    /// A `/bin/sh` MCP server for the era tests. `@DISCOVER@` and
    /// `@INITIALIZE@` are replaced with shell run on those requests, where
    /// `reply` answers the request's `$id`. `tools/list` answers one tool
    /// named for the era the request arrived in: `modern` when it carries
    /// croft's stateless `_meta`, `legacy` after a completed handshake, and
    /// `unnegotiated` otherwise.
    const ERA_SERVER: &str = r#"
reply() { printf '{"jsonrpc":"2.0","id":%s,%s}\n' "$id" "$1"; }
while IFS= read -r l; do
  id=$(printf '%s' "$l" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$l" in
    *'"server/discover"'*) @DISCOVER@ ;;
    *'"initialize"'*) @INITIALIZE@ ;;
    *'"notifications/initialized"'*) inited=1 ;;
    *'"tools/list"'*)
      case "$l" in
        *'"io.modelcontextprotocol/protocolVersion":"2026-07-28"'*) era=modern ;;
        *) era=${inited:+legacy} ;;
      esac
      reply "\"result\":{\"tools\":[{\"name\":\"${era:-unnegotiated}\"}]}" ;;
  esac
done
"#;

    const DISCOVERED: &str = r#"reply '"result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"public"}'"#;
    const NO_SUCH_METHOD: &str = r#"reply '"error":{"code":-32601,"message":"Method not found"}'"#;
    const INITIALIZED: &str = r#"reply '"result":{"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"mock","version":"0"}}'"#;

    /// Connect to an [`ERA_SERVER`] and name the era its `tools/list` saw.
    fn era_after_connect(on_discover: &str, on_initialize: &str, probe: Duration) -> String {
        let script = ERA_SERVER
            .replace("@DISCOVER@", on_discover)
            .replace("@INITIALIZE@", on_initialize);
        let client = McpClient::connect_probing_for(
            "/bin/sh",
            &[String::from("-c"), script],
            &std::env::temp_dir(),
            &path_env(),
            "croft",
            "test",
            probe,
        )
        .expect("the client settles the server's era");
        let list = client.call("tools/list", json!({})).expect("tools/list");
        parse_tools(&list)[0].name.clone()
    }

    /// A stateless server answers the probe, and croft then speaks to it with
    /// per-request `_meta` and no handshake, which it would reject.
    #[test]
    fn a_server_answering_discover_is_spoken_to_statelessly() {
        let era = era_after_connect(DISCOVERED, NO_SUCH_METHOD, PROBE_TIMEOUT);
        assert_eq!(era, "modern");
    }

    /// A handshake-era server rejects the unknown probe method, so croft runs
    /// `initialize` and `notifications/initialized`, then sends plain requests.
    #[test]
    fn a_server_rejecting_discover_gets_the_initialize_handshake() {
        let era = era_after_connect(NO_SUCH_METHOD, INITIALIZED, PROBE_TIMEOUT);
        assert_eq!(era, "legacy");
    }

    /// A handshake-era server that ignores requests sent before `initialize`
    /// leaves the probe unanswered, and croft falls back once it times out.
    #[test]
    fn a_silent_probe_falls_back_to_the_handshake() {
        let era = era_after_connect(":", INITIALIZED, Duration::from_millis(100));
        assert_eq!(era, "legacy");
    }

    /// A stateless server that answers the probe only after it timed out is
    /// still stateless: its late answer wins over the `initialize` croft sent
    /// meanwhile, which such a server rejects.
    #[test]
    fn a_slow_stateless_server_is_not_misread_as_handshake_era() {
        let slow = format!("sleep 0.5; {DISCOVERED}");
        let era = era_after_connect(&slow, NO_SUCH_METHOD, Duration::from_millis(100));
        assert_eq!(era, "modern");
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
