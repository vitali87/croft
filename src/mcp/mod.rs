//! Model Context Protocol (MCP) sidecar host.
//!
//! croft acts as a **deterministic** MCP host: a human invokes a contributed
//! command from the palette and croft calls one pre-known tool on one vetted
//! local server. There is no LLM choosing tools or reading tool descriptions as
//! instructions, which structurally removes MCP's entire semantic attack family
//! (prompt injection, tool poisoning, tool shadowing, the lethal trifecta). What
//! remains is the ordinary "running vetted third-party code" risk croft already
//! accepts for every LSP server, debug adapter, and provisioned binary, mitigated
//! the same way: stdio only (no network listener, so no OAuth / DNS-rebinding /
//! session attack surface), pinned + vendored provisioning (no fetch-at-launch),
//! a least-privilege env, and re-verifying the approved tool definition on spawn.
//!
//! Transport is JSON-RPC 2.0 over newline-delimited JSON on the server's stdio —
//! the MCP stdio framing, the convergent local-RPC pattern (Zed LSP/MCP, Neovim
//! msgpack-RPC). It is hand-rolled over `serde_json` to stay consistent with how
//! croft already hand-vendors its LSP (`async-lsp`) and DAP (custom framing)
//! transports, and is simpler than the DAP `Content-Length` framer (no header).
//! Speaking real MCP keeps croft interoperable with the existing MCP server
//! ecosystem rather than inventing a private protocol.

pub mod catalog;
pub mod client;
pub mod registry;
pub mod transport;

/// The result of a backgrounded MCP command invocation, delivered to the app's
/// poll loop. `title` labels the scratch buffer the rendered output opens in;
/// `body` is the tool's text on success or a human-readable error on failure.
pub struct McpOutcome {
    pub title: String,
    pub body: Result<String, String>,
}
