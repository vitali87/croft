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
use std::process::{Child, Command, Stdio};
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

/// A running debug adapter connection plus its message plumbing. The adapter is
/// reached either over a child process's stdio (debugpy, lldb-dap) or over a TCP
/// socket to a debug server (vscode-js-debug). Both share one decode/encode path;
/// only the byte source differs, abstracted behind the boxed writer + reader.
pub struct DapTransport {
    /// The adapter / debug-server process, when this transport owns one. A child
    /// TCP session connecting to an already-running server holds `None`.
    child: Option<Child>,
    writer: Mutex<Box<dyn Write + Send>>,
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
        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Detach the adapter into its own session before exec. debugpy launches
        // the debuggee and does terminal job control (`tcsetpgrp`) on whatever
        // controlling tty it inherits; sharing croft's tty would background
        // croft and suspend it with SIGTTIN the moment the main loop next reads
        // input. `setsid` gives the adapter (and every process it spawns) a
        // brand-new session with no controlling terminal, so it physically
        // cannot touch croft's tty. DAP itself rides the piped stdio, never the
        // tty, so nothing is lost. Mirrors `lsp::install`'s detached probe.
        //
        // SAFETY: `setsid` is async-signal-safe and the only call in the
        // pre-exec hook; the forked child is never a process-group leader (its
        // pid differs from croft's pgid), so the call always succeeds.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = cmd
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
            child: Some(child),
            writer: Mutex::new(Box::new(stdin)),
            seq: AtomicI64::new(0),
            incoming: rx,
        })
    }

    /// Spawn a DAP debug *server* (`node dapDebugServer.js <port> <host>`) and
    /// connect to it over TCP. Used by vscode-js-debug, whose adapter is a
    /// socket server rather than a stdio child. The server's own stdout/stderr
    /// are discarded (DAP rides the socket); croft retries the connect for a
    /// short window while the server binds its port.
    pub fn connect_tcp_server(
        program: &str,
        args: &[String],
        cwd: &std::path::Path,
        host: &str,
        port: u16,
    ) -> Result<DapTransport> {
        let child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawning debug server `{program}`"))?;
        let stream = connect_with_retry(host, port)?;
        Self::from_stream(stream, Some(child))
    }

    /// Connect to an already-running DAP debug server over TCP, owning no child
    /// process. vscode-js-debug's child sessions reuse the parent's server, so
    /// the child transport just opens a second socket to the same `host:port`.
    pub fn connect_tcp(host: &str, port: u16) -> Result<DapTransport> {
        let stream = connect_with_retry(host, port)?;
        Self::from_stream(stream, None)
    }

    /// Wire a connected TCP stream into a transport: a reader thread frames the
    /// read half into `incoming`, the write half is the boxed writer.
    fn from_stream(stream: std::net::TcpStream, child: Option<Child>) -> Result<DapTransport> {
        let reader = stream.try_clone().context("cloning DAP socket for reads")?;
        let writer = stream;
        let (tx, rx): (Sender<Value>, Receiver<Value>) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("dap-reader".into())
            .spawn(move || reader_loop(reader, tx))
            .context("spawning dap-reader thread")?;
        Ok(DapTransport {
            child,
            writer: Mutex::new(Box::new(writer)),
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
        super::log::log(&format!(
            "send seq={seq} {}",
            message
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("?")
        ));
        let mut writer = self.writer.lock().expect("dap writer mutex poisoned");
        writer.write_all(&bytes).context("writing to adapter")?;
        writer.flush().context("flushing adapter")?;
        Ok(seq)
    }

    /// Best-effort terminate the adapter process, if this transport owns one.
    pub fn kill(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
    }
}

/// Connect to `host:port`, retrying for a short window while the freshly-spawned
/// debug server binds its listener. Fails after the window with the last error.
fn connect_with_retry(host: &str, port: u16) -> Result<std::net::TcpStream> {
    use std::net::{TcpStream, ToSocketAddrs};
    let addr = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("resolving {host}:{port}"))?
        .next()
        .with_context(|| format!("no address for {host}:{port}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut last_err = None;
    while std::time::Instant::now() < deadline {
        match TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500)) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    Err(anyhow::anyhow!(
        "could not connect to debug server at {host}:{port}: {}",
        last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| String::from("timed out"))
    ))
}

/// Pick a currently-free TCP port on localhost by binding to port 0 and reading
/// back the assigned port. There is a tiny race between releasing it here and
/// the debug server binding it, but it is the standard way editors hand a port
/// to a DAP server (Zed/nvim do the same).
pub fn free_port() -> Result<u16> {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").context("binding to find free port")?;
    let port = listener.local_addr().context("reading bound port")?.port();
    Ok(port)
}

impl Drop for DapTransport {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Read framed messages off the adapter's stdout until EOF, forwarding each to
/// the session. Exits quietly when the channel receiver is dropped or stdout
/// closes.
fn reader_loop<R: Read>(source: R, tx: Sender<Value>) {
    let mut reader = BufReader::new(source);
    let mut decoder = FrameDecoder::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                decoder.feed(&chunk[..n]);
                while let Some(msg) = decoder.next_message() {
                    super::log::log(&format!(
                        "recv type={} key={}",
                        msg.get("type").and_then(Value::as_str).unwrap_or("?"),
                        msg.get("event")
                            .or_else(|| msg.get("command"))
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                    ));
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

    /// The adapter (and the debuggee it launches) must NOT share croft's
    /// controlling terminal: debugpy's launcher does terminal job control
    /// (`tcsetpgrp`), which would background croft and suspend it with SIGTTIN
    /// the next time the main loop reads input. `spawn` therefore `setsid`s the
    /// child into its own session. Assert the spawned child's session id differs
    /// from ours. (Spawns `sleep` as a stand-in adapter; the reader thread just
    /// hits EOF when it exits.)
    #[test]
    fn spawned_adapter_is_detached_into_its_own_session() {
        let cwd = std::env::temp_dir();
        let t =
            DapTransport::spawn("sleep", &["3".to_string()], &cwd).expect("spawn stand-in adapter");
        let child_pid = t.child.as_ref().expect("stdio adapter owns a child").id() as libc::pid_t;
        // SAFETY: getsid is a pure query with no side effects.
        let child_sid = unsafe { libc::getsid(child_pid) };
        let our_sid = unsafe { libc::getsid(0) };
        assert!(child_sid > 0, "getsid(child) failed: {child_sid}");
        assert_ne!(
            child_sid, our_sid,
            "adapter must be in its own session, detached from croft's tty"
        );
    }
}
