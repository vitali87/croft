//! MCP wire transport: JSON-RPC 2.0 over newline-delimited JSON (NDJSON).
//!
//! Each message is one compact JSON object on its own line: `<json>\n`. The MCP
//! stdio spec requires messages to be newline-delimited and to contain no
//! embedded newlines, which `serde_json::to_vec` satisfies for free (compact
//! output never emits a bare newline). This is strictly simpler than the DAP
//! `Content-Length` framer — there is no header to parse, just split on `\n`.
//!
//! Like the DAP transport this is deliberately blocking + thread-based (not
//! tokio): an MCP server is a single stdio child, so one reader thread that
//! frames stdout lines into an mpsc channel — plus a stdin writer behind a mutex
//! — is the simplest correct shape. The server's stderr is captured but never
//! treated as an error (the spec reserves stderr for free-form server logging).

use std::collections::BTreeMap;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender};

use anyhow::{Context, Result};
use serde_json::Value;

/// Frame one JSON-RPC message for the wire: compact JSON followed by `\n`.
pub fn encode(message: &Value) -> Vec<u8> {
    let mut out = serde_json::to_vec(message).unwrap_or_default();
    out.push(b'\n');
    out
}

/// Incremental NDJSON decoder: feed it bytes as they arrive off the server's
/// stdout and drain whole messages. Tolerates a line split across reads and
/// several lines in one read. A line that is not valid JSON is skipped (server
/// noise on stdout), not fatal.
#[derive(Default)]
pub struct LineDecoder {
    buf: Vec<u8>,
}

impl LineDecoder {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Append freshly-read bytes to the internal buffer.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Pop the next complete line parsed as JSON, or `None` if no full line is
    /// buffered yet. Call in a loop after each [`feed`](Self::feed). Skips blank
    /// lines and lines that fail to parse (returns the next valid one, or `None`).
    pub fn next_message(&mut self) -> Option<Value> {
        loop {
            let nl = self.buf.iter().position(|&b| b == b'\n')?;
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            let trimmed = &line[..line.len() - 1]; // drop the '\n'
            if trimmed.iter().all(|b| b.is_ascii_whitespace()) {
                continue; // blank keep-alive line
            }
            if let Ok(value) = serde_json::from_slice::<Value>(trimmed) {
                return Some(value);
            }
            // Non-JSON line on stdout: skip it and try the next line.
        }
    }
}

/// A running MCP server connection plus its message plumbing. Owns the child
/// process, a line-framed reader thread feeding `incoming`, and a stdin writer.
pub struct McpTransport {
    child: Child,
    writer: Mutex<Box<dyn Write + Send>>,
    /// Drained by the client: every decoded incoming message (responses,
    /// server-initiated notifications and requests alike).
    pub incoming: Receiver<Value>,
}

impl McpTransport {
    /// Spawn `program args...` as an MCP server speaking NDJSON JSON-RPC on stdio.
    /// `env` is the explicit, least-privilege environment handed to the server
    /// (the host does not leak its whole environment). A reader thread frames
    /// stdout lines into `incoming` until the server exits.
    pub fn spawn(
        program: &str,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<McpTransport> {
        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Detach into its own session before exec (mirrors the DAP transport):
        // a sidecar must never be able to touch croft's controlling tty. MCP
        // rides the piped stdio, so the detach loses nothing.
        //
        // SAFETY: `setsid` is async-signal-safe and the only call in the
        // pre-exec hook; the forked child is never a process-group leader, so
        // the call always succeeds.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning MCP server `{program}`"))?;

        let stdin = child.stdin.take().context("MCP server stdin missing")?;
        let stdout = child.stdout.take().context("MCP server stdout missing")?;

        let (tx, rx): (Sender<Value>, Receiver<Value>) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("mcp-reader".into())
            .spawn(move || reader_loop(stdout, tx))
            .context("spawning mcp-reader thread")?;

        Ok(McpTransport {
            child,
            writer: Mutex::new(Box::new(stdin)),
            incoming: rx,
        })
    }

    /// Write an already-built JSON-RPC message to the server.
    pub fn send(&self, message: &Value) -> Result<()> {
        let bytes = encode(message);
        let mut writer = self.writer.lock().expect("mcp writer mutex poisoned");
        writer.write_all(&bytes).context("writing to MCP server")?;
        writer.flush().context("flushing MCP server")?;
        Ok(())
    }

    /// Best-effort terminate the server. MCP's stdio shutdown is "close stdin,
    /// then signal": dropping the writer closes stdin, and `kill` is the SIGKILL
    /// backstop.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

impl Drop for McpTransport {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Read newline-framed messages off the server's stdout until EOF, forwarding
/// each to the client. Exits when the channel receiver is dropped or stdout
/// closes.
fn reader_loop<R: Read>(source: R, tx: Sender<Value>) {
    let mut reader = BufReader::new(source);
    let mut decoder = LineDecoder::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                decoder.feed(&chunk[..n]);
                while let Some(msg) = decoder.next_message() {
                    if tx.send(msg).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encode_is_compact_json_terminated_by_a_single_newline() {
        let bytes = encode(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}));
        // Exactly one trailing newline, none embedded (the NDJSON invariant).
        assert_eq!(bytes.iter().filter(|&&b| b == b'\n').count(), 1);
        assert_eq!(*bytes.last().unwrap(), b'\n');
        let parsed: Value = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(parsed["method"], "initialize");
    }

    #[test]
    fn decodes_a_single_line() {
        let mut d = LineDecoder::new();
        d.feed(&encode(&json!({"id": 1, "result": {}})));
        assert_eq!(d.next_message().unwrap()["id"], 1);
        assert!(d.next_message().is_none());
    }

    #[test]
    fn decodes_two_lines_in_one_feed() {
        let mut d = LineDecoder::new();
        let mut bytes = encode(&json!({"id": 1}));
        bytes.extend(encode(&json!({"id": 2})));
        d.feed(&bytes);
        assert_eq!(d.next_message().unwrap()["id"], 1);
        assert_eq!(d.next_message().unwrap()["id"], 2);
        assert!(d.next_message().is_none());
    }

    #[test]
    fn reassembles_a_line_split_across_feeds() {
        let bytes = encode(&json!({"method": "tools/list"}));
        let split = bytes.len() / 2;
        let mut d = LineDecoder::new();
        d.feed(&bytes[..split]);
        assert!(d.next_message().is_none(), "incomplete: nothing yet");
        d.feed(&bytes[split..]);
        assert_eq!(d.next_message().unwrap()["method"], "tools/list");
    }

    #[test]
    fn skips_blank_and_non_json_lines() {
        let mut d = LineDecoder::new();
        d.feed(b"\n");
        d.feed(b"not json at all\n");
        d.feed(&encode(&json!({"id": 7})));
        // The blank and the noise line are skipped; the valid message survives.
        assert_eq!(d.next_message().unwrap()["id"], 7);
        assert!(d.next_message().is_none());
    }

    /// A sidecar must not share croft's controlling terminal — `spawn` `setsid`s
    /// it into its own session. Assert the spawned child's session id differs
    /// from ours. (Spawns `sleep` as a stand-in server; the reader thread just
    /// hits EOF when it exits.)
    #[test]
    fn spawned_server_is_detached_into_its_own_session() {
        let cwd = std::env::temp_dir();
        let env = BTreeMap::new();
        let t = McpTransport::spawn("sleep", &["3".to_string()], &cwd, &env)
            .expect("spawn stand-in server");
        let child_pid = t.child.id() as libc::pid_t;
        // SAFETY: getsid is a pure query with no side effects.
        let child_sid = unsafe { libc::getsid(child_pid) };
        let our_sid = unsafe { libc::getsid(0) };
        assert!(child_sid > 0, "getsid(child) failed: {child_sid}");
        assert_ne!(
            child_sid, our_sid,
            "server must be in its own session, detached from croft's tty"
        );
    }
}
