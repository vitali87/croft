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
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;

const CONTENT_LENGTH: &str = "Content-Length";
/// How long a dropped session's adapter gets to answer `disconnect` and end
/// its debuggee before croft kills it (#1536).
const DISCONNECT_GRACE: Duration = Duration::from_secs(2);
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
        // A complete frame whose body does not parse is skipped, not
        // returned as `None`: the reader stops at the first `None`, and the
        // good frames already buffered behind a bad one (a `stopped` event)
        // would wait for bytes the adapter never sends.
        loop {
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
            if value.is_some() {
                return value;
            }
        }
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
    /// What the teardown on drop needs from the wire, shared with the reader.
    teardown: Arc<(Mutex<Teardown>, Condvar)>,
}

/// What the reader and `send` have seen that decides how a dropped
/// transport ends its adapter (#1536).
#[derive(Default)]
struct Teardown {
    /// `Some(terminateDebuggee)` once a `disconnect` went out.
    disconnect: Option<bool>,
    /// The adapter answered `disconnect`, or its output closed.
    answered: bool,
    /// The debuggee's pid from the adapter's `process` event.
    debuggee: Option<i32>,
    /// An `exited` / `terminated` event: the debuggee is gone, so its pid
    /// may already belong to something else.
    debuggee_ended: bool,
}

impl Teardown {
    fn observe(&mut self, msg: &Value) {
        let kind = msg.get("type").and_then(Value::as_str);
        let name = msg
            .get("event")
            .or_else(|| msg.get("command"))
            .and_then(Value::as_str);
        match (kind, name) {
            (Some("event"), Some("process")) => {
                // A remote target's pid names no local process: signalling
                // it could hit an unrelated one that shares the number.
                let local = msg["body"]["isLocalProcess"].as_bool() != Some(false);
                self.debuggee = msg["body"]["systemProcessId"]
                    .as_i64()
                    .filter(|_| local)
                    .and_then(|p| i32::try_from(p).ok())
                    .filter(|p| *p > 0);
            }
            (Some("event"), Some("exited" | "terminated")) => self.debuggee_ended = true,
            (Some("response"), Some("disconnect")) => self.answered = true,
            _ => {}
        }
    }
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

        // The adapter's stderr (its own diagnostics, e.g. debugpy tracebacks)
        // rode nowhere before; tee it line-by-line into the OUTPUT panel's
        // "Debug Adapter" channel so a failing adapter is visible in-app.
        if let Some(stderr) = child.stderr.take() {
            std::thread::Builder::new()
                .name("dap-stderr".into())
                .spawn(move || {
                    use std::io::BufRead;
                    for line in std::io::BufReader::new(stderr)
                        .lines()
                        .map_while(Result::ok)
                    {
                        crate::output::push(
                            crate::output::CHANNEL_DAP,
                            crate::output::OutputLevel::Info,
                            &line,
                        );
                    }
                })
                .ok();
        }

        let (tx, rx): (Sender<Value>, Receiver<Value>) = std::sync::mpsc::channel();
        let teardown = Arc::new((Mutex::new(Teardown::default()), Condvar::new()));
        let seen = teardown.clone();
        std::thread::Builder::new()
            .name("dap-reader".into())
            .spawn(move || reader_loop(stdout, tx, &seen))
            .context("spawning dap-reader thread")?;

        Ok(DapTransport {
            child: Some(child),
            writer: Mutex::new(Box::new(stdin)),
            seq: AtomicI64::new(0),
            incoming: rx,
            teardown,
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
        path_prepend: Option<&std::path::Path>,
    ) -> Result<DapTransport> {
        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // js-debug spawns the debuggee with a bare `node`, so when croft resolved
        // node outside PATH (nvm/fnm/etc.) its directory must be on the server's
        // PATH or the child target can't launch.
        if let Some(dir) = path_prepend {
            let existing = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{existing}", dir.display()));
        }
        // Detach the server into its own session/process group, exactly as the
        // stdio `spawn` path does. This is what lets `kill` later signal the
        // whole group (server + the debuggee node it forks) in one shot rather
        // than leaking the debuggee. The server's pid becomes its own pgid, so a
        // group kill targets only this tree, never croft's.
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
        let child = cmd
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
        let teardown = Arc::new((Mutex::new(Teardown::default()), Condvar::new()));
        let seen = teardown.clone();
        std::thread::Builder::new()
            .name("dap-reader".into())
            .spawn(move || reader_loop(reader, tx, &seen))
            .context("spawning dap-reader thread")?;
        Ok(DapTransport {
            child,
            writer: Mutex::new(Box::new(writer)),
            seq: AtomicI64::new(0),
            incoming: rx,
            teardown,
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
        if message.get("command").and_then(Value::as_str) == Some("disconnect") {
            let terminate = message["arguments"]["terminateDebuggee"]
                .as_bool()
                .unwrap_or(true);
            self.teardown.0.lock().unwrap().disconnect = Some(terminate);
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
}

/// Best-effort terminate an adapter process tree. The adapter was `setsid`'d
/// at spawn, so its pid is its own process-group id; signalling the whole
/// group (`killpg`) reaps what it forked into that group too, not just the
/// direct child. `child.kill()` then covers the direct child
/// belt-and-suspenders, and `wait()` reaps the zombie so a long-lived croft
/// never accumulates defunct adapters. debugpy's launcher starts the
/// debuggee in a group of its own, which `finish_teardown` covers (#1536).
///
/// vscode-js-debug's `watchdog` `setsid`s into a *separate* session, so it
/// is deliberately outside this group; the graceful `disconnect` handshake
/// and the startup orphan sweep ([`crate::dap::reaper`]) are what retire it.
fn kill_tree(child: &mut Child) {
    let pid = child.id() as i32;
    // SAFETY: `killpg` is a plain libc call; `pid` is this child's pgid
    // (it `setsid`'d), distinct from croft's, so only the adapter tree
    // is signalled.
    unsafe {
        libc::killpg(pid, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Whether `pid` is still running. A zombie (exited, not yet reaped by
/// whoever inherited it) counts as gone.
fn is_running(pid: i32) -> bool {
    // SAFETY: signal 0 sends nothing; it only checks that the pid exists.
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        Err(_) => true,
    }
}

/// End a disconnected session's adapter (#1536). Killing it the moment
/// `disconnect` is written gives it no chance to act on it, and debugpy's
/// launcher puts the debuggee in its own process group, out of reach of
/// `killpg`: the program ran on under PID 1. So wait for the answer (or
/// the deadline), let the debuggee finish exiting, then kill the adapter's
/// group, and, for a launch, the debuggee the adapter reported if it is
/// still there.
fn finish_teardown(mut child: Child, teardown: &(Mutex<Teardown>, Condvar), grace: Duration) {
    let deadline = Instant::now() + grace;
    let (lock, answered) = teardown;
    let mut seen = lock.lock().unwrap();
    while !seen.answered {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        seen = answered.wait_timeout(seen, left).unwrap().0;
    }
    let debuggee = match (seen.disconnect, seen.debuggee) {
        (Some(true), Some(pid)) if !seen.debuggee_ended => Some(pid),
        _ => None,
    };
    drop(seen);
    if let Some(pid) = debuggee {
        while is_running(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    kill_tree(&mut child);
    // Read after the wait: an `exited` that landed meanwhile means the pid
    // is no longer the debuggee's.
    if let Some(pid) = debuggee
        && !lock.lock().unwrap().debuggee_ended
        && is_running(pid)
    {
        // SAFETY: `pid` is the debuggee the adapter reported for this
        // launch, still running and not reported as ended.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

/// Teardown threads `Drop` detached; the exit path joins them so croft
/// does not quit halfway through ending a debuggee.
static TEARDOWNS: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

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

/// Join every disconnect teardown a dropped transport detached. Bounded by
/// `DISCONNECT_GRACE`, since they run side by side.
pub fn join_teardowns() {
    let handles: Vec<_> = std::mem::take(&mut *TEARDOWNS.lock().unwrap());
    for h in handles {
        let _ = h.join();
    }
}

impl Drop for DapTransport {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if self.teardown.0.lock().unwrap().disconnect.is_none() {
            kill_tree(&mut child);
            return;
        }
        // Off the UI thread: Stop Debugging stays instant on screen.
        // The child rides in a shared slot so a failed spawn, which drops
        // the closure, still leaves it here to kill rather than orphan.
        let teardown = self.teardown.clone();
        let slot = Arc::new(Mutex::new(Some(child)));
        let moved = slot.clone();
        let spawned = std::thread::Builder::new()
            .name("dap-teardown".into())
            .spawn(move || {
                if let Some(child) = moved.lock().unwrap().take() {
                    finish_teardown(child, &teardown, DISCONNECT_GRACE);
                }
            });
        match spawned {
            Ok(handle) => {
                let mut reg = TEARDOWNS.lock().unwrap();
                reg.retain(|h| !h.is_finished());
                reg.push(handle);
            }
            Err(_) => {
                if let Some(mut child) = slot.lock().unwrap().take() {
                    kill_tree(&mut child);
                }
            }
        }
    }
}

/// Read framed messages off the adapter's stdout until EOF, forwarding each to
/// the session. Exits quietly when the channel receiver is dropped or stdout
/// closes.
fn reader_loop<R: Read>(source: R, tx: Sender<Value>, teardown: &(Mutex<Teardown>, Condvar)) {
    let mut reader = BufReader::new(source);
    let mut decoder = FrameDecoder::new();
    let mut chunk = [0u8; 8192];
    // Reading goes on after the session is dropped (#1536): the teardown
    // waits on the `disconnect` answer that arrives then.
    let mut forward = true;
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
                    let mut seen = teardown.0.lock().unwrap();
                    seen.observe(&msg);
                    if seen.answered {
                        teardown.1.notify_all();
                    }
                    drop(seen);
                    if forward && tx.send(msg).is_err() {
                        forward = false;
                    }
                }
            }
        }
    }
    teardown.0.lock().unwrap().answered = true;
    teardown.1.notify_all();
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
    fn a_malformed_frame_does_not_strand_the_frames_behind_it() {
        let mut d = FrameDecoder::new();
        let mut bytes = b"Content-Length: 5\r\n\r\n{bad}".to_vec();
        bytes.extend(encode(&json!({"type": "event", "event": "stopped"})));
        d.feed(&bytes);
        assert_eq!(d.next_message().unwrap()["event"], "stopped");
        assert!(d.next_message().is_none());
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

    /// A DAP adapter stand-in (#1536). It starts a "debuggee" (`sleep`) in
    /// its own session, as debugpy's launcher does, reports its pid in a
    /// `process` event, then serves `disconnect` per `mode`: `answer` waits
    /// 100 ms, ends the debuggee when asked to, and replies; `silent`
    /// never replies.
    const FAKE_ADAPTER: &str = r#"
import json, os, subprocess, sys, time
mode = sys.argv[1]
deb = subprocess.Popen(["sleep", "30"], start_new_session=True)
def send(m):
    b = json.dumps(m).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(b) + b)
    sys.stdout.buffer.flush()
def read():
    hdr = b""
    while not hdr.endswith(b"\r\n\r\n"):
        c = sys.stdin.buffer.read(1)
        if not c:
            sys.exit(0)
        hdr += c
    n = int(hdr.split(b":")[1].split(b"\r\n")[0])
    return json.loads(sys.stdin.buffer.read(n))
send({"seq": 1, "type": "event", "event": "process",
      "body": {"name": "loop.py", "systemProcessId": deb.pid}})
while True:
    m = read()
    if m.get("command") == "disconnect" and mode == "answer":
        time.sleep(0.1)
        if m["arguments"].get("terminateDebuggee"):
            deb.kill()
            deb.wait()
        send({"seq": 2, "type": "response", "request_seq": m["seq"],
              "command": "disconnect", "success": True})
"#;

    /// `join_teardowns` joins every test's teardowns; one at a time, so
    /// none returns before its own thread is done.
    static TEARDOWN_TESTS: Mutex<()> = Mutex::new(());

    /// Spawn the stand-in and return it with its debuggee's pid.
    fn fake_session(mode: &str) -> (DapTransport, i32, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("adapter.py");
        std::fs::write(&script, FAKE_ADAPTER).unwrap();
        let t = DapTransport::spawn(
            "python3",
            &[script.display().to_string(), mode.to_string()],
            dir.path(),
        )
        .expect("spawn fake adapter");
        let event = t
            .incoming
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("process event");
        let pid = event["body"]["systemProcessId"].as_i64().unwrap() as i32;
        (t, pid, dir)
    }

    /// Alive and not a zombie (an unreaped child of a killed adapter).
    fn running(pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the pid exists.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return false;
        }
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => !stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
            Err(_) => true,
        }
    }

    /// SIGKILL lands asynchronously: give a just-killed pid a moment.
    fn ends_soon(pid: i32) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while running(pid) {
            if std::time::Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        true
    }

    fn end(pid: i32) {
        // SAFETY: test cleanup of a pid this test started.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }

    fn disconnect(terminate: bool) -> Value {
        json!({"type": "request", "command": "disconnect",
               "arguments": {"terminateDebuggee": terminate}})
    }

    /// #1536: Stop Debugging sends `disconnect` and drops the session at
    /// once. The adapter must get to answer before it is killed, or it
    /// never ends the debuggee, which runs on under PID 1.
    #[test]
    fn a_dropped_session_lets_the_adapter_end_the_debuggee() {
        let _one = TEARDOWN_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (t, pid, _dir) = fake_session("answer");
        assert!(running(pid));
        t.send(disconnect(true)).unwrap();
        let started = std::time::Instant::now();
        drop(t);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(50),
            "dropping must not block the UI thread"
        );
        join_teardowns();
        let ok = ends_soon(pid);
        end(pid);
        assert!(ok, "the debuggee outlived Stop Debugging");
    }

    /// #1536 backstop: an adapter that never answers `disconnect` is
    /// killed after the grace period, and the debuggee it reported is
    /// ended too (its own session puts it out of the adapter's group).
    #[test]
    fn a_silent_adapter_does_not_leave_its_debuggee_running() {
        let _one = TEARDOWN_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (t, pid, _dir) = fake_session("silent");
        t.send(disconnect(true)).unwrap();
        drop(t);
        join_teardowns();
        let ok = ends_soon(pid);
        end(pid);
        assert!(ok, "the reported debuggee survived the teardown");
    }

    /// #1536: a `process` event for a remote target carries a pid that
    /// names no local process, so the teardown must never signal it.
    #[test]
    fn a_remote_process_pid_is_not_recorded() {
        let event = |body: Value| json!({"type": "event", "event": "process", "body": body});
        let mut seen = Teardown::default();
        seen.observe(&event(
            json!({"systemProcessId": 4242, "isLocalProcess": false}),
        ));
        assert_eq!(seen.debuggee, None, "a remote pid was kept");
        seen.observe(&event(
            json!({"systemProcessId": 4242, "isLocalProcess": true}),
        ));
        assert_eq!(seen.debuggee, Some(4242));
        seen.observe(&event(json!({"systemProcessId": 77})));
        assert_eq!(seen.debuggee, Some(77), "an unmarked pid is local");
    }

    /// #1536 negative: an attach session disconnects with
    /// `terminateDebuggee: false`. croft did not start that process, so the
    /// teardown leaves it running.
    #[test]
    fn an_attached_process_is_left_running() {
        let _one = TEARDOWN_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (t, pid, _dir) = fake_session("silent");
        t.send(disconnect(false)).unwrap();
        drop(t);
        join_teardowns();
        let ok = running(pid);
        end(pid);
        assert!(ok, "the attached process was killed");
    }

    /// #1536 negative: without a `disconnect` there is nothing to wait for,
    /// so the adapter is killed at once, as before, and no reported pid is
    /// signalled.
    #[test]
    fn a_session_dropped_without_disconnect_is_killed_at_once() {
        let _one = TEARDOWN_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (t, pid, _dir) = fake_session("silent");
        let adapter = t.child.as_ref().unwrap().id() as i32;
        drop(t);
        join_teardowns();
        let adapter_gone = ends_soon(adapter);
        let debuggee_kept = running(pid);
        end(pid);
        assert!(adapter_gone, "the adapter outlived its transport");
        assert!(debuggee_kept, "no disconnect, so no debuggee kill");
    }
}
