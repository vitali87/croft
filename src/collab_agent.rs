//! Headless collab participant exposed as an MCP server (docs/MULTIPLAYER.md,
//! Phase D). `croft collab-agent --workspace <path>` joins the workspace's
//! collab relay as a regular guest and serves five `collab_*` tools over
//! JSON-RPC 2.0 on stdio, so an external agent (e.g. Claude Code, registered
//! with `claude mcp add`) co-edits with a live named caret. Croft stays free
//! of LLM code: the intelligence is whatever drives the tools; this module is
//! just a seat at the table, and it inherits every guest property for free —
//! owner-only disk writes, per-file site ids, convergence via the CRDT.
//!
//! Concurrency: one background thread pumps the session (drains the socket,
//! integrates remote ops, tracks peer carets) so the replica is always fresh
//! when a tool call diffs against it; the main thread blocks on stdin. Both
//! share the state behind a mutex that is never held across a sleep.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::collab::{CollabChannel, CollabEvent, CollabRole, CollabSession};

/// How long `collab_open` waits for the owner's snapshot before reporting
/// that nobody serves this workspace: a hair past the session's own 3s
/// bootstrap deadline, so the session's verdict always lands first.
const OPEN_TIMEOUT: Duration = Duration::from_secs(4);

/// The guest session plus peers' last-seen carets (for `collab_status`).
struct AgentState {
    session: CollabSession,
    peers: HashMap<u64, PeerCaret>,
}

struct PeerCaret {
    name: String,
    file: String,
    row: usize,
    col: usize,
}

/// Connect to the relay as a guest, retrying briefly (the dispatch just
/// ensured the relay, but its accept loop may still be coming up).
fn connect_state(socket: &Path, name: &str) -> Result<AgentState> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let channel = loop {
        if let Some(ch) = CollabChannel::connect(socket, CollabRole::Guest) {
            break ch;
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "collab relay never came up at {}",
            socket.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    Ok(AgentState {
        session: CollabSession::new(channel, name.to_string()),
        peers: HashMap::new(),
    })
}

/// One session pump: drain the socket and fold peer carets into the map.
/// Remote edits need nothing further — the replica inside the session is
/// already updated, and the agent has no editor buffers to patch.
fn pump(state: &Mutex<AgentState>) {
    let mut st = state.lock().unwrap();
    let events = st.session.poll(|_| None);
    for event in events {
        if let CollabEvent::Caret {
            file,
            site,
            row,
            col,
            name,
        } = event
        {
            st.peers.insert(
                site,
                PeerCaret {
                    name,
                    file,
                    row,
                    col,
                },
            );
        }
    }
}

/// Answer one inbound JSON-RPC message; `None` for notifications (and for
/// malformed lines with no `id` to answer to).
fn handle(state: &Mutex<AgentState>, msg: &Value) -> Option<Value> {
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": crate::mcp::client::PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "croft-collab",
                "version": env!("CARGO_PKG_VERSION"),
            },
        })),
        "notifications/initialized" | "notifications/cancelled" => return None,
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => {
            let name = msg
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let args = msg
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call_tool(state, &name, &args) {
                Some(result) => Ok(result),
                None => Err((-32602, format!("unknown tool: {name}"))),
            }
        }
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    let id = msg.get("id").cloned()?;
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }),
    })
}

/// A successful `tools/call` result: one text content block.
fn text_result(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }] })
}

/// A failed tool execution: per MCP, an `isError` result (the model reads
/// and recovers from it), not a JSON-RPC protocol error.
fn error_result(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }], "isError": true })
}

/// Run one collab tool; `None` means the tool name is unknown.
fn call_tool(state: &Mutex<AgentState>, tool: &str, args: &Value) -> Option<Value> {
    let file = args
        .get("file")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let needs_file = tool != "collab_status";
    if needs_file && file.is_empty() {
        return Some(error_result("missing required argument: file"));
    }
    let not_live = || error_result(format!("{file} is not live; call collab_open first"));
    Some(match tool {
        "collab_open" => {
            // rejoin, not request: an earlier open that timed out latched
            // the file local-only, and a plain request respects the latch —
            // the caller retrying after starting croft would never join.
            state.lock().unwrap().session.rejoin_file(&file);
            let deadline = Instant::now() + OPEN_TIMEOUT;
            loop {
                {
                    let st = state.lock().unwrap();
                    if let Some(text) = st.session.doc_text(&file) {
                        break text_result(text);
                    }
                    if !st.session.is_bootstrapping(&file) || Instant::now() >= deadline {
                        break error_result(format!(
                            "no live croft session answered for {file}; start croft in \
                             this workspace (the owner serves the file), then retry"
                        ));
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        "collab_read" => match state.lock().unwrap().session.doc_text(&file) {
            Some(text) => text_result(text),
            None => not_live(),
        },
        "collab_replace" => {
            let Some(text) = args.get("text").and_then(Value::as_str) else {
                return Some(error_result("missing required argument: text"));
            };
            let mut st = state.lock().unwrap();
            if !st.session.is_live(&file) {
                not_live()
            } else {
                st.session.local_change(&file, text);
                text_result(st.session.doc_text(&file).unwrap_or_default())
            }
        }
        "collab_caret" => {
            let row = args.get("row").and_then(Value::as_u64);
            let col = args.get("col").and_then(Value::as_u64);
            let (Some(row), Some(col)) = (row, col) else {
                return Some(error_result("row and col are required integers (0-based)"));
            };
            let mut st = state.lock().unwrap();
            if !st.session.is_live(&file) {
                not_live()
            } else {
                st.session.send_caret(&file, row as usize, col as usize);
                text_result(format!("caret at {row}:{col} in {file}"))
            }
        }
        "collab_status" => {
            let st = state.lock().unwrap();
            let files: serde_json::Map<String, Value> = st
                .session
                .tracked_files()
                .into_iter()
                .map(|(f, live)| (f, Value::from(if live { "live" } else { "bootstrapping" })))
                .collect();
            let peers: Vec<Value> = st
                .peers
                .iter()
                .map(|(site, p)| {
                    json!({
                        "site": site,
                        "name": p.name,
                        "file": p.file,
                        "row": p.row,
                        "col": p.col,
                    })
                })
                .collect();
            text_result(json!({ "files": files, "peers": peers }).to_string())
        }
        _ => return None,
    })
}

/// The five tools this server advertises. Coordinates are 0-based character
/// positions, matching the collab wire protocol.
fn tool_definitions() -> Value {
    let file_prop = json!({
        "type": "string",
        "description": "Workspace-relative path, e.g. src/main.rs",
    });
    json!([
        {
            "name": "collab_open",
            "description": "Join a workspace file in the live croft session and \
                return its current text. Call this once per file before reading \
                or editing it.",
            "inputSchema": {
                "type": "object",
                "properties": { "file": file_prop },
                "required": ["file"],
            },
        },
        {
            "name": "collab_read",
            "description": "The file's current text, including edits other \
                participants just made.",
            "inputSchema": {
                "type": "object",
                "properties": { "file": file_prop },
                "required": ["file"],
            },
        },
        {
            "name": "collab_replace",
            "description": "Replace the file's entire text; the diff is broadcast \
                to every participant as live edits (the session owner persists them \
                to disk). Returns the post-apply text. Read or open first so the \
                replacement builds on the latest text.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": file_prop,
                    "text": { "type": "string", "description": "The complete new file text" },
                },
                "required": ["file", "text"],
            },
        },
        {
            "name": "collab_caret",
            "description": "Move this agent's visible caret so humans see where it \
                is working (a named colored caret in their editors). 0-based row \
                and character column. Move it near each edit you make.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": file_prop,
                    "row": { "type": "integer", "description": "0-based line" },
                    "col": { "type": "integer", "description": "0-based character column" },
                },
                "required": ["file", "row", "col"],
            },
        },
        {
            "name": "collab_status",
            "description": "Session overview: every tracked file with its state \
                (live or bootstrapping) and each peer's last-seen caret.",
            "inputSchema": { "type": "object", "properties": {} },
        },
    ])
}

/// Serve the MCP loop on stdio until the driving client hangs up (EOF).
pub fn run(socket: &Path, name: String) -> Result<()> {
    let state = std::sync::Arc::new(Mutex::new(connect_state(socket, &name)?));
    {
        let state = std::sync::Arc::clone(&state);
        std::thread::spawn(move || {
            loop {
                pump(&state);
                std::thread::sleep(Duration::from_millis(30));
            }
        });
    }
    let mut decoder = crate::mcp::transport::LineDecoder::new();
    let mut stdin = std::io::stdin().lock();
    let stdout = std::io::stdout();
    let mut buf = [0u8; 8192];
    loop {
        let n = stdin.read(&mut buf).context("reading MCP stdin")?;
        if n == 0 {
            return Ok(()); // the driver hung up; this seat leaves the session
        }
        decoder.feed(&buf[..n]);
        while let Some(msg) = decoder.next_message() {
            if let Some(resp) = handle(&state, &msg) {
                let mut bytes = serde_json::to_vec(&resp).unwrap_or_default();
                bytes.push(b'\n');
                let mut out = stdout.lock();
                let _ = out.write_all(&bytes);
                let _ = out.flush();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::{CollabChannel, CollabEvent, CollabRole, CollabSession, relay_serve};
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn rpc(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn call(state: &Mutex<AgentState>, id: i64, tool: &str, args: Value) -> Value {
        handle(
            state,
            &rpc(id, "tools/call", json!({ "name": tool, "arguments": args })),
        )
        .expect("tools/call always answers")
    }

    fn text_of(resp: &Value) -> String {
        crate::mcp::client::tool_result_text(&resp["result"])
    }

    /// A live relay plus a connected agent state, ready to drive `handle`.
    fn agent_over_relay() -> (tempfile::TempDir, std::path::PathBuf, Mutex<AgentState>) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("a.collab.sock");
        {
            let s = socket.clone();
            std::thread::spawn(move || {
                let _ = relay_serve(&s);
            });
        }
        let state = connect_state(&socket, "claude").expect("agent connects");
        (dir, socket, Mutex::new(state))
    }

    /// The MCP surface: initialize/tools-list shapes mirror what croft's own
    /// client sends, unknown methods get JSON-RPC errors, and tools on files
    /// that are not live fail as tool errors (isError), not protocol errors.
    #[test]
    fn collab_agent_answers_the_mcp_surface() {
        let (_dir, _socket, state) = agent_over_relay();

        let init = handle(&state, &rpc(1, "initialize", json!({}))).unwrap();
        assert_eq!(
            init["result"]["protocolVersion"],
            crate::mcp::client::PROTOCOL_VERSION
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "croft-collab");
        assert!(
            handle(
                &state,
                &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
            )
            .is_none(),
            "notifications draw no response"
        );

        let tools = handle(&state, &rpc(2, "tools/list", json!({}))).unwrap();
        let names: Vec<&str> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "collab_open",
                "collab_read",
                "collab_replace",
                "collab_caret",
                "collab_status"
            ]
        );

        let unknown = handle(&state, &rpc(3, "bogus/method", json!({}))).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);

        let read = call(&state, 4, "collab_read", json!({ "file": "src/f.rs" }));
        assert_eq!(read["result"]["isError"], true);
        assert!(
            text_of(&read).contains("collab_open"),
            "the error teaches the fix: {}",
            text_of(&read)
        );
    }

    /// End to end over a real relay: the agent opens a file served by a live
    /// owner, replaces its text, and moves its caret; the owner's replica
    /// converges and its events carry the caret under the agent's name.
    #[test]
    fn collab_agent_opens_edits_and_carets_through_the_relay() {
        let (_dir, socket, state) = agent_over_relay();
        let state = Arc::new(state);

        let deadline = Instant::now() + Duration::from_secs(5);
        let owner = loop {
            if let Some(ch) = CollabChannel::connect(&socket, CollabRole::Owner) {
                break CollabSession::new(ch, "owner".into());
            }
            assert!(Instant::now() < deadline, "relay never came up");
            std::thread::sleep(Duration::from_millis(10));
        };
        let owner = Arc::new(Mutex::new(owner));
        let owner_events: Arc<Mutex<Vec<CollabEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        // Owner pump: serves one file's text and collects events.
        let owner_pump = {
            let (owner, events, stop) = (owner.clone(), owner_events.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    {
                        let mut o = owner.lock().unwrap();
                        let ev = o.poll(|_| Some("hello world".to_string()));
                        events.lock().unwrap().extend(ev);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };
        // Agent pump, exactly as `run` spawns it.
        let agent_pump = {
            let (state, stop) = (state.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    pump(&state);
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };

        let opened = call(&state, 1, "collab_open", json!({ "file": "src/f.rs" }));
        assert_eq!(text_of(&opened), "hello world");

        let replaced = call(
            &state,
            2,
            "collab_replace",
            json!({ "file": "src/f.rs", "text": "hello brave world" }),
        );
        assert_eq!(text_of(&replaced), "hello brave world");
        let deadline = Instant::now() + Duration::from_secs(5);
        while owner.lock().unwrap().doc_text("src/f.rs") != Some("hello brave world") {
            assert!(Instant::now() < deadline, "owner never converged");
            std::thread::sleep(Duration::from_millis(5));
        }

        call(
            &state,
            3,
            "collab_caret",
            json!({ "file": "src/f.rs", "row": 0, "col": 6 }),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let seen = owner_events.lock().unwrap().iter().any(|e| {
                matches!(e, CollabEvent::Caret { name, row: 0, col: 6, .. } if name == "claude")
            });
            if seen {
                break;
            }
            assert!(Instant::now() < deadline, "owner never saw the agent caret");
            std::thread::sleep(Duration::from_millis(5));
        }

        let status = call(&state, 4, "collab_status", json!({}));
        let status_text = text_of(&status);
        assert!(status_text.contains("src/f.rs"), "status lists the file");
        assert!(status_text.contains("live"), "status shows the live state");

        stop.store(true, Ordering::Relaxed);
        let _ = (owner_pump.join(), agent_pump.join());
    }
}
