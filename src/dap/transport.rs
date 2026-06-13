//! DAP wire transport.
//!
//! The Debug Adapter Protocol frames messages exactly like LSP
//! (`Content-Length: N\r\n\r\n<json>`), but the JSON envelope is *not* JSON-RPC:
//! it carries a monotonic `seq` and a `type` of `request` | `response` |
//! `event`. `async-lsp` hard-codes the JSON-RPC envelope, so croft hand-rolls
//! this ~framing layer and lets the session interpret the decoded values.
//!
//! Transport is deliberately blocking + thread-based (not tokio): the adapter is
//! a single stdio child, so one reader thread that frames stdout into an mpsc
//! channel — plus a stdin writer behind a mutex — is simpler and correct. Every
//! incoming message (response, event, reverse-request alike) is forwarded
//! verbatim; the [`super::session`] layer matches responses to requests by
//! `request_seq` and reacts to events.

use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use anyhow::{Context, Result};
use serde_json::Value;

const CONTENT_LENGTH: &str = "Content-Length";
const HEADER_SEP: &[u8] = b"\r\n\r\n";

/// Frame a DAP message body for the wire: `Content-Length: N\r\n\r\n<json>`.
pub fn encode(message: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(message).unwrap_or_default();
    let mut out = format!("{CONTENT_LENGTH}: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Incremental decoder: feed it bytes as they arrive off the adapter's stdout
/// and drain whole messages. Tolerates messages split across reads and several
/// messages in one read.
#[derive(Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Append freshly-read bytes to the internal buffer.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Pop the next complete message, or `None` if one isn't fully buffered yet.
    /// Call in a loop after each [`feed`](Self::feed) until it returns `None`.
    pub fn next_message(&mut self) -> Option<Value> {
        let sep = find_subslice(&self.buf, HEADER_SEP)?;
        let header = std::str::from_utf8(&self.buf[..sep]).ok()?;
        let len = content_length(header)?;
        let body_start = sep + HEADER_SEP.len();
        let body_end = body_start + len;
        if self.buf.len() < body_end {
            return None; // body not fully arrived yet
        }
        let value = serde_json::from_slice(&self.buf[body_start..body_end]).ok();
        self.buf.drain(..body_end);
        value
    }
}

/// Parse the `Content-Length` value out of a header block (case-insensitive key,
/// other headers ignored).
fn content_length(header: &str) -> Option<usize> {
    header.lines().find_map(|line| {
        let (key, val) = line.split_once(':')?;
        if key.trim().eq_ignore_ascii_case(CONTENT_LENGTH) {
            val.trim().parse().ok()
        } else {
            None
        }
    })
}

/// Index of the first occurrence of `needle` in `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// A running debug adapter process plus its message plumbing.
pub struct DapTransport {
    child: Child,
    stdin: Mutex<ChildStdin>,
    seq: AtomicI64,
    /// Drained by the session: every decoded incoming message (responses,
    /// events, reverse-requests).
    pub incoming: Receiver<Value>,
}

impl DapTransport {
    /// Spawn `program args...` as a debug adapter speaking DAP on stdio (e.g.
    /// `python -m debugpy.adapter`). A reader thread frames stdout into the
    /// `incoming` channel until the adapter exits.
    pub fn spawn(program: &str, args: &[String], cwd: &std::path::Path) -> Result<DapTransport> {
        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning debug adapter `{program}`"))?;

        let stdin = child.stdin.take().context("adapter stdin missing")?;
        let stdout = child.stdout.take().context("adapter stdout missing")?;

        let (tx, rx): (Sender<Value>, Receiver<Value>) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("dap-reader".into())
            .spawn(move || reader_loop(stdout, tx))
            .context("spawning dap-reader thread")?;

        Ok(DapTransport {
            child,
            stdin: Mutex::new(stdin),
            seq: AtomicI64::new(0),
            incoming: rx,
        })
    }

    /// Next monotonic sequence number for an outgoing message.
    pub fn next_seq(&self) -> i64 {
        self.seq.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Write an already-built message to the adapter, stamping its `seq`.
    pub fn send(&self, mut message: Value) -> Result<i64> {
        let seq = self.next_seq();
        if let Some(obj) = message.as_object_mut() {
            obj.insert("seq".into(), Value::from(seq));
        }
        let bytes = encode(&message);
        let mut stdin = self.stdin.lock().expect("dap stdin mutex poisoned");
        stdin
            .write_all(&bytes)
            .context("writing to adapter stdin")?;
        stdin.flush().context("flushing adapter stdin")?;
        Ok(seq)
    }

    /// Best-effort terminate the adapter process.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

impl Drop for DapTransport {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Read framed messages off the adapter's stdout until EOF, forwarding each to
/// the session. Exits quietly when the channel receiver is dropped or stdout
/// closes.
fn reader_loop(stdout: std::process::ChildStdout, tx: Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    let mut decoder = FrameDecoder::new();
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
    fn encode_prepends_content_length_header() {
        let bytes = encode(&json!({"seq": 1, "type": "request"}));
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("Content-Length: "));
        assert!(text.contains("\r\n\r\n"));
        let (header, body) = text.split_once("\r\n\r\n").unwrap();
        let declared: usize = header
            .strip_prefix("Content-Length: ")
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(declared, body.len());
    }

    #[test]
    fn decodes_a_single_message() {
        let mut d = FrameDecoder::new();
        d.feed(&encode(&json!({"type": "event", "event": "stopped"})));
        let msg = d.next_message().unwrap();
        assert_eq!(msg["event"], "stopped");
        assert!(d.next_message().is_none());
    }

    #[test]
    fn decodes_two_messages_in_one_feed() {
        let mut d = FrameDecoder::new();
        let mut bytes = encode(&json!({"seq": 1}));
        bytes.extend(encode(&json!({"seq": 2})));
        d.feed(&bytes);
        assert_eq!(d.next_message().unwrap()["seq"], 1);
        assert_eq!(d.next_message().unwrap()["seq"], 2);
        assert!(d.next_message().is_none());
    }

    #[test]
    fn reassembles_a_message_split_across_feeds() {
        let bytes = encode(&json!({"command": "initialize", "type": "request"}));
        let split = bytes.len() / 2;
        let mut d = FrameDecoder::new();
        d.feed(&bytes[..split]);
        assert!(d.next_message().is_none(), "incomplete: nothing yet");
        d.feed(&bytes[split..]);
        assert_eq!(d.next_message().unwrap()["command"], "initialize");
    }

    #[test]
    fn content_length_header_is_case_insensitive() {
        assert_eq!(content_length("content-length: 42"), Some(42));
        assert_eq!(content_length("Content-Length: 7\r\nX: y"), Some(7));
        assert_eq!(content_length("X-Other: 1"), None);
    }
}
