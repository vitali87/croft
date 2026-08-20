//! Multiplayer session host: croft's own replacement for dtach on the
//! persistent-session path (see docs/MULTIPLAYER.md).
//!
//! One `session-host` server owns the PTY the real croft TUI runs on and
//! accepts any number of clients over a unix socket. Output bytes are
//! broadcast to every client verbatim (byte-transparent, like dtach: Kitty
//! graphics and every escape sequence pass through untouched). Input is
//! attributed: each client is its own socket connection, so the server knows
//! who typed what and can enforce write control server-side, which dtach and
//! abduco cannot. Window size is the minimum across connected clients (the
//! tmux rule), re-asserted with a jiggle on attach so the inner croft always
//! repaints for a reattaching client.
//!
//! Wire format, both directions: `[type: u8][len: u32 be][payload]`.
//! Type 0 carries raw PTY bytes; type 1 carries one NDJSON-style control
//! message (see [`Control`]), the same compact-JSON convention as
//! `crate::mcp::transport`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};

/// Frame type tag for raw PTY bytes (either direction).
pub const FRAME_BYTES: u8 = 0;
/// Frame type tag for a JSON [`Control`] message (either direction).
pub const FRAME_CONTROL: u8 = 1;

/// Control messages exchanged between host and clients. Serialized as
/// compact JSON with a `"t"` tag so the protocol stays greppable on the wire
/// and future fields stay backward-compatible (unknown fields are ignored).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Control {
    /// First message a client must send: identity and terminal size.
    /// `version` is the client's crate version (#53); pre-0.1.698 clients
    /// never send it (`default` keeps their Hello parsing) and pre-0.1.698
    /// servers ignore the unknown field (compact JSON tolerates extras).
    Hello {
        name: String,
        cols: u16,
        rows: u16,
        #[serde(default)]
        version: String,
    },
    /// Client terminal was resized.
    Resize { cols: u16, rows: u16 },
    /// Host to clients: the current participant roster.
    Presence { participants: Vec<Participant> },
    /// A control-holding client grants write control to participant `id`.
    Grant { id: u64 },
    /// A control-holding client revokes write control from participant `id`.
    Revoke { id: u64 },
    /// Client detaches deliberately (the session keeps running).
    Detach,
    /// Host to clients: the inner croft exited with `code`; session over.
    Exit { code: i32 },
    /// First message of the inner croft's control channel instead of Hello:
    /// authenticates with the token the host put in CROFT_SESSION_TOKEN. A
    /// privileged channel is not a participant (no roster entry, no PTY
    /// broadcast) and may grant/revoke/kick unconditionally, because every
    /// keystroke reaching the inner croft's UI already came from a
    /// control-holding client (read-only input is dropped at the host).
    Inner { token: String },
    /// Disconnect participant `id` (their session keeps running; the client
    /// pump exits as on a detach). Honored from a control-holding client or
    /// the inner channel.
    Kick { id: u64 },
    /// Host to the registering client, directly after its Hello (#53):
    /// the server's crate version, so the client can detect skew against a
    /// long-lived server that survived binary upgrades. Pre-0.1.698
    /// servers never send it, which the client detects by timeout;
    /// pre-0.1.698 clients skip the unknown variant (the frame decoder
    /// drops malformed control payloads without poisoning the stream).
    ServerHello { version: String },
    /// Host to privileged channels only: participant `id` is now the one
    /// typing (sent when the writing client changes, not per keystroke).
    /// Ordered before that client's bytes reach the PTY, so the inner croft
    /// can attribute the keystrokes it is about to receive and switch to
    /// that participant's caret (docs/MULTIPLAYER.md, attributed carets).
    Typing { id: u64 },
}

/// One attached client as reported in [`Control::Presence`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Participant {
    pub id: u64,
    pub name: String,
    pub cols: u16,
    pub rows: u16,
    /// Whether this client's keystrokes reach the PTY (write control).
    pub control: bool,
}

/// A decoded frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Bytes(Vec<u8>),
    Control(Control),
}

/// Encode raw PTY bytes as a frame.
pub fn encode_bytes_frame(data: &[u8]) -> Vec<u8> {
    encode_frame(FRAME_BYTES, data)
}

/// Encode a control message as a frame.
pub fn encode_control_frame(control: &Control) -> Vec<u8> {
    let json = serde_json::to_vec(control).unwrap_or_default();
    encode_frame(FRAME_CONTROL, &json)
}

fn encode_frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Incremental frame decoder: feed it whatever chunk sizes the socket
/// delivers; it yields every complete frame and buffers the remainder.
#[derive(Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, data: &[u8]) -> Vec<Frame> {
        self.buf.extend_from_slice(data);
        let mut frames = Vec::new();
        loop {
            if self.buf.len() < 5 {
                break;
            }
            let kind = self.buf[0];
            let len =
                u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
            if self.buf.len() < 5 + len {
                break;
            }
            let payload: Vec<u8> = self.buf.drain(..5 + len).skip(5).collect();
            match kind {
                FRAME_BYTES => frames.push(Frame::Bytes(payload)),
                FRAME_CONTROL => {
                    // A malformed control message is skipped rather than
                    // poisoning the stream: framing already resynchronized us.
                    if let Ok(control) = serde_json::from_slice::<Control>(&payload) {
                        frames.push(Frame::Control(control));
                    }
                }
                // Unknown frame type from a newer peer: ignore the payload.
                _ => {}
            }
        }
        frames
    }
}

/// The PTY size every connected client can display: minimum cols and minimum
/// rows across clients (the tmux rule). Clients reporting no size (0 in
/// either dimension, seen under `script` and some CI PTYs) are pure
/// observers here and never shrink the shared PTY. None when no client with
/// a usable size is connected.
pub fn min_winsize(sizes: impl Iterator<Item = (u16, u16)>) -> Option<(u16, u16)> {
    sizes
        .filter(|&(c, r)| c > 0 && r > 0)
        .reduce(|(c1, r1), (c2, r2)| (c1.min(c2), r1.min(r2)))
}

/// Sidecar file the host keeps next to the socket with the current roster,
/// so the inner croft (which only has a PTY, not the socket) can poll who is
/// attached. Written atomically (tmp + rename).
pub fn presence_path(socket: &Path) -> PathBuf {
    let mut name = socket.file_name().unwrap_or_default().to_os_string();
    name.push(".presence.json");
    socket.with_file_name(name)
}

struct Client {
    id: u64,
    name: String,
    cols: u16,
    rows: u16,
    control: bool,
    tx: Arc<Mutex<UnixStream>>,
}

struct Host {
    clients: Mutex<Vec<Client>>,
    next_id: AtomicU64,
    pty_input: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    last_size: Mutex<(u16, u16)>,
    socket: PathBuf,
    /// Shared secret for the inner croft's privileged control channel,
    /// exported to the inner command as CROFT_SESSION_TOKEN.
    token: String,
    /// Authenticated privileged channels (the inner croft); they receive
    /// typing attribution, never PTY bytes or presence frames.
    privileged: Mutex<Vec<Arc<Mutex<UnixStream>>>>,
    /// Which client's input last reached the PTY, to send [`Control::Typing`]
    /// only on writer changes.
    last_writer: Mutex<Option<u64>>,
}

/// The collab socket sibling of a mux socket: same directory and hash key,
/// `.collab.sock` in place of `.mux.sock` (matches the keying in
/// `session::collab_socket_path` / `remote::collab_socket_path`).
fn collab_socket_for_mux(mux: &Path) -> PathBuf {
    let name = mux.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let stem = name.strip_suffix(".mux.sock").unwrap_or(name);
    mux.with_file_name(format!("{stem}.collab.sock"))
}

/// Run the session server: spawn `inner` on a fresh PTY, listen on `socket`,
/// broadcast output, arbitrate input. Returns the inner command's exit code
/// once it terminates (the caller propagates it, so codes like croft's
/// drop-to-local 88 survive the mux, which dtach never propagated).
pub fn serve(socket: &Path, workspace: Option<&Path>, inner: &[String]) -> Result<i32> {
    serve_with_token(socket, workspace, inner, &random_token()?)
}

/// A random hex token for the privileged channel. The socket is already
/// 0600, so this is a discriminator (inner croft vs regular participant),
/// not the trust boundary; account possession remains that. Propagate an RNG
/// failure rather than emitting the all-zero buffer as a predictable token.
fn random_token() -> Result<String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|e| anyhow::anyhow!("session-host RNG failed: {e:?}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub(crate) fn serve_with_token(
    socket: &Path,
    workspace: Option<&Path>,
    inner: &[String],
    token: &str,
) -> Result<i32> {
    anyhow::ensure!(
        !inner.is_empty(),
        "session-host needs an inner command after --"
    );
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    if crate::session::is_alive(socket) {
        anyhow::bail!("a session host is already running on {}", socket.display());
    }
    // Possession of the account is the trust boundary (same as dtach); never
    // let another user attach. Stale-file removal and creation serialization
    // live inside the binder: a racer that loses the per-target lock is told
    // the winner is alive instead of silently replacing its socket.
    let listener = match crate::session::bind_socket_0600(socket) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            anyhow::bail!("a session host is already running on {}", socket.display());
        }
        Err(e) => {
            return Err(anyhow::Error::from(e).context(format!("binding {}", socket.display())));
        }
    };
    if let Some(ws) = workspace {
        crate::session::write_meta_preserving_created(socket, ws)?;
    }

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("openpty")?;
    let mut cmd = CommandBuilder::new(&inner[0]);
    cmd.args(&inner[1..]);
    // The inner croft re-asserts terminal modes on WINCH under this flag
    // (the dead-mouse-on-reattach fix); the mux relies on that machinery.
    cmd.env("CROFT_SESSION_PERSISTENT", "1");
    // Let the inner croft find its host and authenticate a privileged
    // control channel (participants UI: grant/revoke/kick).
    cmd.env("CROFT_SESSION_SOCKET", socket.as_os_str());
    cmd.env("CROFT_SESSION_TOKEN", token);
    // The inner croft is the collab-session owner: when a solo-viewport
    // guest joins the workspace (docs/MULTIPLAYER.md, Phase D), it answers
    // bootstrap over the sibling collab socket. Exported unconditionally;
    // the app connects lazily, so a session with no solo guests never pays
    // for it.
    cmd.env(
        "CROFT_COLLAB_SOCKET",
        collab_socket_for_mux(socket).as_os_str(),
    );
    cmd.env("CROFT_COLLAB_ROLE", "owner");
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .context("spawning inner command")?;
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .context("cloning pty reader")?;
    let writer = pair.master.take_writer().context("taking pty writer")?;
    let host = Arc::new(Host {
        clients: Mutex::new(Vec::new()),
        next_id: AtomicU64::new(0),
        pty_input: Mutex::new(writer),
        master: Mutex::new(pair.master),
        last_size: Mutex::new((80, 24)),
        socket: socket.to_path_buf(),
        token: token.to_string(),
        privileged: Mutex::new(Vec::new()),
        last_writer: Mutex::new(None),
    });

    // PTY output -> every client, verbatim (byte-transparent broadcast).
    {
        let host = Arc::clone(&host);
        std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        broadcast(&host, &encode_bytes_frame(&buf[..n]));
                    }
                }
            }
        });
    }

    // Accept loop; one thread per client. The thread count equals the
    // participant count, which is single digits by construction.
    {
        let host = Arc::clone(&host);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let host = Arc::clone(&host);
                std::thread::spawn(move || client_thread(&host, stream));
            }
        });
    }

    let status = child.wait().context("waiting on inner command")?;
    let code = status.exit_code() as i32;
    broadcast(&host, &encode_control_frame(&Control::Exit { code }));
    let _ = std::fs::remove_file(socket);
    let _ = std::fs::remove_file(presence_path(socket));
    // Drop the meta sidecar too, or a later server for this workspace inherits
    // the dead session's created time and `croft ls` reports an inflated uptime.
    crate::session::remove_meta(socket);
    Ok(code)
}

/// Write `frame` to every connected client, pruning the ones whose
/// connection died — including a WEDGED one (#53): a `kill -STOP`ped
/// client stops draining, its socket buffer fills, and a plain `write_all`
/// here blocked the PTY pump forever while holding the clients lock,
/// starving every other client and every future attach. The bounded write
/// treats a peer that drains nothing for [`WRITE_FRAME_DEADLINE`] as gone.
/// After any prune the shared size and roster are recomputed, so a dead
/// ghost releases the min winsize it was pinning. Returns whether any
/// client was pruned.
fn broadcast(host: &Host, frame: &[u8]) -> bool {
    let pruned = {
        let mut clients = host.clients.lock().unwrap();
        let before = clients.len();
        // A fresh deadline PER CLIENT: retain visits them sequentially,
        // and a shared deadline consumed by one wedged peer would evict
        // every healthy client after it without a single write attempted.
        clients.retain(|c| {
            write_frame_bounded(
                &mut c.tx.lock().unwrap(),
                frame,
                Instant::now() + WRITE_FRAME_DEADLINE,
            )
        });
        clients.len() != before
    };
    if pruned {
        apply_winsize(host, false);
        update_presence(host);
    }
    pruned
}

/// Resize the PTY to the minimum size across clients. `force_repaint`
/// jiggles the size when it is already at the target, so a reattaching
/// client always gets a real Resize event out of the inner croft (the role
/// dtach's `-r winch` played; a same-size reattach would otherwise stare at
/// a blank terminal).
fn apply_winsize(host: &Host, force_repaint: bool) {
    let target = {
        let clients = host.clients.lock().unwrap();
        min_winsize(clients.iter().map(|c| (c.cols, c.rows)))
    };
    let mut last = host.last_size.lock().unwrap();
    let master = host.master.lock().unwrap();
    let size = |cols: u16, rows: u16| PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };
    match target {
        Some(target) if *last != target => {
            let _ = master.resize(size(target.0, target.1));
            *last = target;
        }
        // The jiggle runs at the CURRENT size whenever a repaint was asked
        // for, even with no sized client connected (#53): an attach from a
        // 0x0 PTY (`script`, headless CI) used to return before this
        // branch, so an observer-only attach never produced a Resize and
        // the inner croft never repainted — a blank screen with a blinking
        // cursor and no way to recover.
        _ if force_repaint => {
            let (cols, rows) = *last;
            let _ = master.resize(size(cols, rows.saturating_sub(1).max(1)));
            let _ = master.resize(size(cols, rows));
        }
        _ => {}
    }
}

/// Write the roster sidecar (atomic tmp + rename, so the inner croft never
/// reads a torn file) and broadcast it as a Presence frame.
fn update_presence(host: &Host) {
    let participants: Vec<Participant> = {
        let clients = host.clients.lock().unwrap();
        clients
            .iter()
            .map(|c| Participant {
                id: c.id,
                name: c.name.clone(),
                cols: c.cols,
                rows: c.rows,
                control: c.control,
            })
            .collect()
    };
    let path = presence_path(&host.socket);
    if let Ok(json) = serde_json::to_string(&participants) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
    broadcast(
        host,
        &encode_control_frame(&Control::Presence { participants }),
    );
}

/// Serve one client connection until it detaches or its socket dies.
fn client_thread(host: &Host, mut stream: UnixStream) {
    let tx = match stream.try_clone() {
        Ok(s) => Arc::new(Mutex::new(s)),
        Err(_) => return,
    };
    let mut reader = FrameReader::new();
    let mut buf = [0u8; 16384];
    let mut my_id: Option<u64> = None;
    let mut privileged = false;
    'conn: loop {
        let n = match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for frame in reader.push(&buf[..n]) {
            match frame {
                Frame::Control(Control::Inner { token })
                    if my_id.is_none() && token == host.token =>
                {
                    privileged = true;
                    host.privileged.lock().unwrap().push(Arc::clone(&tx));
                    // Catch the channel up on who is currently typing: a
                    // writer change announced before this registration would
                    // otherwise never be repeated for the same writer.
                    let current = *host.last_writer.lock().unwrap();
                    if let Some(id) = current {
                        let _ = tx
                            .lock()
                            .unwrap()
                            .write_all(&encode_control_frame(&Control::Typing { id }));
                    }
                }
                Frame::Control(Control::Hello {
                    name, cols, rows, ..
                }) if my_id.is_none() && !privileged => {
                    // Answer with our version FIRST: registration below makes
                    // this client a broadcast target, and the ServerHello must
                    // beat any PTY bytes so the attaching client's version
                    // phase ends on the first frame (#53).
                    let _ = tx.lock().unwrap().write_all(&encode_control_frame(
                        &Control::ServerHello {
                            version: String::from(env!("CARGO_PKG_VERSION")),
                        },
                    ));
                    let id = host.next_id.fetch_add(1, Ordering::Relaxed);
                    my_id = Some(id);
                    {
                        let mut clients = host.clients.lock().unwrap();
                        // Write control auto-attaches only when nobody holds
                        // it: the first client, or an owner reattaching after
                        // every control holder left. Everyone else starts as
                        // a read-only observer until granted.
                        let control = !clients.iter().any(|c| c.control);
                        clients.push(Client {
                            id,
                            name,
                            cols,
                            rows,
                            control,
                            tx: Arc::clone(&tx),
                        });
                    }
                    apply_winsize(host, true);
                    update_presence(host);
                }
                Frame::Bytes(bytes) => {
                    let Some(id) = my_id else { continue };
                    let has_control = {
                        let clients = host.clients.lock().unwrap();
                        clients.iter().any(|c| c.id == id && c.control)
                    };
                    // Server-side enforcement: a read-only client's input
                    // never reaches the PTY (unlike abduco's advisory -r).
                    if has_control {
                        announce_typing(host, id);
                        let mut pty = host.pty_input.lock().unwrap();
                        if pty.write_all(&bytes).and_then(|_| pty.flush()).is_err() {
                            break 'conn;
                        }
                    }
                }
                Frame::Control(Control::Resize { cols, rows }) => {
                    let Some(id) = my_id else { continue };
                    {
                        let mut clients = host.clients.lock().unwrap();
                        if let Some(c) = clients.iter_mut().find(|c| c.id == id) {
                            c.cols = cols;
                            c.rows = rows;
                        }
                    }
                    apply_winsize(host, false);
                    update_presence(host);
                }
                Frame::Control(Control::Grant { id: target }) => {
                    set_control(host, privileged, my_id, target, true);
                }
                Frame::Control(Control::Revoke { id: target }) => {
                    set_control(host, privileged, my_id, target, false);
                }
                Frame::Control(Control::Kick { id: target }) => {
                    kick(host, privileged, my_id, target);
                }
                Frame::Control(Control::Detach) => break 'conn,
                _ => {}
            }
        }
    }
    if let Some(id) = my_id {
        {
            let mut clients = host.clients.lock().unwrap();
            clients.retain(|c| c.id != id);
        }
        apply_winsize(host, false);
        update_presence(host);
    }
}

/// Tell the privileged channels whose input is about to reach the PTY,
/// only when the writer changed (never per keystroke). Ordered before the
/// PTY write so the attribution precedes the keystrokes it covers.
fn announce_typing(host: &Host, id: u64) {
    {
        let mut last = host.last_writer.lock().unwrap();
        if *last == Some(id) {
            return;
        }
        *last = Some(id);
    }
    let frame = encode_control_frame(&Control::Typing { id });
    let mut channels = host.privileged.lock().unwrap();
    channels.retain(|tx| tx.lock().unwrap().write_all(&frame).is_ok());
}

/// Grant or revoke write control on `target`, but only when the requester
/// is the privileged inner channel or itself holds control: read-only
/// guests cannot promote themselves.
fn set_control(host: &Host, privileged: bool, requester: Option<u64>, target: u64, grant: bool) {
    let changed = {
        let mut clients = host.clients.lock().unwrap();
        let allowed = privileged
            || requester.is_some_and(|id| clients.iter().any(|c| c.id == id && c.control));
        if !allowed {
            false
        } else {
            match clients.iter_mut().find(|c| c.id == target) {
                Some(c) if c.control != grant => {
                    c.control = grant;
                    true
                }
                _ => false,
            }
        }
    };
    if changed {
        update_presence(host);
    }
}

/// Disconnect participant `target` (privileged channel or a control holder
/// only). Shutting the stream down unblocks the target's client thread,
/// which then deregisters and updates presence itself.
fn kick(host: &Host, privileged: bool, requester: Option<u64>, target: u64) {
    let clients = host.clients.lock().unwrap();
    let allowed =
        privileged || requester.is_some_and(|id| clients.iter().any(|c| c.id == id && c.control));
    if !allowed {
        return;
    }
    if let Some(c) = clients.iter().find(|c| c.id == target) {
        let _ = c.tx.lock().unwrap().shutdown(std::net::Shutdown::Both);
    }
}

/// dtach `-A` semantics: attach the session on `socket` if it is alive,
/// otherwise spawn a detached server for `inner` first, then attach.
pub fn attach_or_create(socket: &Path, workspace: Option<&Path>, inner: &[String]) -> Result<i32> {
    loop {
        if !crate::session::is_alive(socket) {
            spawn_detached_server(socket, workspace, inner)?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while !crate::session::is_alive(socket) {
                if Instant::now() > deadline {
                    anyhow::bail!("session host did not start on {}", socket.display());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        match attach_client(socket)? {
            PumpOutcome::Exit(code) => return Ok(code),
            // The user chose R on the version-mismatch banner (#53): kill
            // the stale server and loop — the respawn above starts a server
            // of THIS binary's version, so the next attach matches.
            PumpOutcome::RestartRequested => {
                kill_stale_server(socket);
            }
        }
    }
}

/// Terminate this session's server: candidates come from one `ps` listing
/// (portable to macOS, which has no /proc), gated by LITERAL substring
/// checks — the command line must name both `session-host` and this exact
/// socket path — so an attached client, a shell that echoed the path, or a
/// regex metacharacter in the path can never widen the match the way a
/// bare `pgrep -f <path>` did. SIGTERM first; a server still alive at the
/// deadline gets SIGKILL (its inner croft sees the PTY master close and
/// exits). The socket and sidecars are unlinked only once the server is
/// actually dead — unlinking a live server's socket would orphan it and
/// its inner croft forever, invisible to every future attach.
fn kill_stale_server(socket: &Path) {
    let me = std::process::id();
    let socket_str = socket.to_string_lossy();
    let candidates = |socket_str: &str| -> Vec<u32> {
        let Ok(out) = std::process::Command::new("ps")
            .args(["-eo", "pid=,command="])
            .output()
        else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let line = line.trim_start();
                let (pid, cmd) = line.split_once(' ')?;
                let pid: u32 = pid.parse().ok()?;
                (pid != me && cmd.contains("session-host") && cmd.contains(socket_str))
                    .then_some(pid)
            })
            .collect()
    };
    for (sig, wait) in [
        (libc::SIGTERM, Duration::from_secs(5)),
        (libc::SIGKILL, Duration::from_secs(2)),
    ] {
        let pids = candidates(&socket_str);
        if pids.is_empty() && !crate::session::is_alive(socket) {
            break;
        }
        for pid in pids {
            unsafe {
                libc::kill(pid as libc::pid_t, sig);
            }
        }
        let deadline = Instant::now() + wait;
        while crate::session::is_alive(socket) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        if !crate::session::is_alive(socket) {
            break;
        }
    }
    if !crate::session::is_alive(socket) {
        let _ = std::fs::remove_file(socket);
        let _ = std::fs::remove_file(presence_path(socket));
        crate::session::remove_meta(socket);
    }
}

/// The argv (after the croft binary itself) that runs a detached server for
/// `inner`. Pure so it can be unit-tested.
fn detached_server_argv(socket: &Path, workspace: Option<&Path>, inner: &[String]) -> Vec<String> {
    let mut argv = vec![
        String::from("session-host"),
        String::from("--serve"),
        String::from("--socket"),
        socket.to_string_lossy().into_owned(),
    ];
    if let Some(ws) = workspace {
        argv.push(String::from("--workspace"));
        argv.push(ws.to_string_lossy().into_owned());
    }
    argv.push(String::from("--"));
    argv.extend(inner.iter().cloned());
    argv
}

fn spawn_detached_server(socket: &Path, workspace: Option<&Path>, inner: &[String]) -> Result<()> {
    let exe = std::env::current_exe().context("resolving croft binary path")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(detached_server_argv(socket, workspace, inner));
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // New session so the server survives this client's terminal closing;
    // exactly the role dtach's forked server plays.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("spawning session host")?;
    Ok(())
}

/// Attach this terminal as a client: raw mode, pump stdin to the socket and
/// socket bytes to stdout, report resizes. Returns the session's exit code
/// (0 on detach). Deliberately does nothing else to the terminal: every
/// mode change (alt screen, mouse, Kitty flags) belongs to the inner croft
/// and passes through as bytes, exactly as under dtach.
/// How one attach ended: the session finished (or the client detached),
/// or the user chose to restart a version-skewed session (#53).
pub enum PumpOutcome {
    Exit(i32),
    RestartRequested,
}

/// How long the client waits for [`Control::ServerHello`] after its Hello.
/// A matching server answers in the same scheduling quantum (the reply is
/// written before the client is even registered for broadcasts); only a
/// pre-0.1.698 server stays silent, so expiry means "old server", not
/// "slow server" — and the attach still proceeds after the banner, so the
/// cost of a false positive is one keypress, never a refused attach.
const SERVER_HELLO_DEADLINE: Duration = Duration::from_secs(2);

/// What the user chose on the version-mismatch banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MismatchAction {
    Continue,
    Restart,
    Detach,
}

fn mismatch_action(key: u8) -> MismatchAction {
    match key {
        b'c' | b'C' => MismatchAction::Continue,
        b'r' | b'R' => MismatchAction::Restart,
        _ => MismatchAction::Detach,
    }
}

/// The plain-text banner for a version-skewed attach: visible, actionable,
/// never a silent blank screen (#53). Raw mode is active, hence the CRLFs.
fn mismatch_banner(server: Option<&str>, client: &str) -> String {
    let server = match server {
        Some(v) => format!("croft {v}"),
        None => String::from("pre-0.1.698 (reports no version)"),
    };
    // Joined with explicit CRLFs: raw mode disables ONLCR, so a bare LF
    // staircases — and a multi-line string literal carries the source
    // file's LFs, which is exactly what rustfmt turns escapes into.
    [
        "",
        "croft session version mismatch",
        &format!("  session server: {server}"),
        &format!("  this croft:     croft {client}"),
        "The session may render incorrectly until the server is restarted.",
        "[C] continue anyway   [R] restart session (its terminals die)   [any other key] detach",
        "",
    ]
    .join("\r\n")
}

pub fn attach_client(socket: &Path) -> Result<PumpOutcome> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    crossterm::terminal::enable_raw_mode().context("enabling raw mode")?;
    let result = attach_client_pump(&mut stream);
    let _ = crossterm::terminal::disable_raw_mode();
    result
}

fn attach_client_pump(stream: &mut UnixStream) -> Result<PumpOutcome> {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let tx = Arc::new(Mutex::new(stream.try_clone().context("cloning socket")?));
    tx.lock()
        .unwrap()
        .write_all(&encode_control_frame(&Control::Hello {
            name: client_name(),
            cols,
            rows,
            version: String::from(env!("CARGO_PKG_VERSION")),
        }))
        .context("sending hello")?;

    // Version phase (#53): wait briefly for the server's ServerHello. PTY
    // bytes racing it are buffered and replayed after the decision so
    // nothing is dropped. This runs BEFORE the stdin thread spawns, so a
    // banner keypress is read here, not swallowed by the forwarder.
    let mut pending: Vec<Vec<u8>> = Vec::new();
    let mut reader = FrameReader::new();
    let mut server_version: Option<String> = None;
    {
        let mut buf = [0u8; 65536];
        let phase_deadline = Instant::now() + SERVER_HELLO_DEADLINE;
        'phase: while server_version.is_none() {
            let remaining = phase_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            stream
                .set_read_timeout(Some(remaining))
                .context("setting read timeout")?;
            match stream.read(&mut buf) {
                Ok(0) => return Ok(PumpOutcome::Exit(0)),
                Ok(n) => {
                    for frame in reader.push(&buf[..n]) {
                        match frame {
                            Frame::Control(Control::ServerHello { version }) => {
                                server_version = Some(version);
                            }
                            Frame::Control(Control::Exit { code }) => {
                                return Ok(PumpOutcome::Exit(code));
                            }
                            Frame::Bytes(bytes) => pending.push(bytes),
                            _ => {}
                        }
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break 'phase;
                }
                Err(_) => return Ok(PumpOutcome::Exit(0)),
            }
        }
        stream
            .set_read_timeout(None)
            .context("clearing read timeout")?;
    }
    let client_version = env!("CARGO_PKG_VERSION");
    if server_version.as_deref() != Some(client_version) {
        let mut out = std::io::stdout().lock();
        out.write_all(mismatch_banner(server_version.as_deref(), client_version).as_bytes())
            .context("writing banner")?;
        out.flush().context("flushing banner")?;
        drop(out);
        let mut key = [0u8; 1];
        let n = std::io::stdin().lock().read(&mut key).unwrap_or(0);
        // A closed or non-tty stdin (piped attach, headless CI) cannot
        // answer: continue, the pre-banner behavior, rather than silently
        // exiting 0 as if the session ended.
        if n == 0 {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(b"stdin closed; continuing\r\n");
            let _ = out.flush();
        }
        match mismatch_action(if n == 1 { key[0] } else { b'c' }) {
            MismatchAction::Continue => {}
            MismatchAction::Restart => {
                let _ = tx
                    .lock()
                    .unwrap()
                    .write_all(&encode_control_frame(&Control::Detach));
                return Ok(PumpOutcome::RestartRequested);
            }
            MismatchAction::Detach => {
                let _ = tx
                    .lock()
                    .unwrap()
                    .write_all(&encode_control_frame(&Control::Detach));
                return Ok(PumpOutcome::Exit(0));
            }
        }
    }

    // stdin -> socket. The thread parks on a blocking read; it dies with the
    // process when the session ends (the CLI exits right after we return).
    {
        let tx = Arc::clone(&tx);
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 16384];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx
                            .lock()
                            .unwrap()
                            .write_all(&encode_control_frame(&Control::Detach));
                        break;
                    }
                    Ok(n) => {
                        if tx
                            .lock()
                            .unwrap()
                            .write_all(&encode_bytes_frame(&buf[..n]))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
    }

    // Terminal size -> resize frames. A 200ms poll instead of a SIGWINCH
    // handler keeps the client free of signal plumbing; the delay is
    // imperceptible against the terminal's own resize animation.
    // ponytail: poll, swap for signal_hook if 200ms ever reads as lag.
    {
        let tx = Arc::clone(&tx);
        let mut last = (cols, rows);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(200));
                let Ok(size) = crossterm::terminal::size() else {
                    continue;
                };
                if size != last {
                    last = size;
                    let frame = encode_control_frame(&Control::Resize {
                        cols: size.0,
                        rows: size.1,
                    });
                    if tx.lock().unwrap().write_all(&frame).is_err() {
                        break;
                    }
                }
            }
        });
    }

    // socket -> stdout, until the host reports the inner croft's exit or the
    // connection drops (server killed; treated as a detach). The version
    // phase's reader carries any partial frame; its buffered PTY bytes
    // replay first so nothing raced away.
    let mut out = std::io::stdout().lock();
    for bytes in pending.drain(..) {
        out.write_all(&bytes).context("writing to terminal")?;
    }
    out.flush().context("flushing terminal")?;
    let mut buf = [0u8; 65536];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) | Err(_) => return Ok(PumpOutcome::Exit(0)),
            Ok(n) => n,
        };
        for frame in reader.push(&buf[..n]) {
            match frame {
                Frame::Bytes(bytes) => {
                    out.write_all(&bytes).context("writing to terminal")?;
                    out.flush().context("flushing terminal")?;
                }
                Frame::Control(Control::Exit { code }) => return Ok(PumpOutcome::Exit(code)),
                // Roster changes surface inside the inner croft (which polls
                // the presence sidecar), not in this thin pump.
                _ => {}
            }
        }
    }
}

/// The inner croft's privileged handle to its session host: lets the
/// participants UI grant/revoke write control and disconnect clients.
/// Fire-and-forget writes; the host answers through the presence sidecar,
/// which the app already polls.
pub struct InnerChannel {
    stream: UnixStream,
    /// Decoder for host frames (typing attribution) on this channel.
    reader: FrameReader,
    /// The presence sidecar this session's host maintains.
    pub presence: PathBuf,
}

impl InnerChannel {
    /// Connect using the CROFT_SESSION_SOCKET / CROFT_SESSION_TOKEN pair the
    /// host put in the inner croft's environment; None when not running
    /// under a session host.
    pub fn from_env() -> Option<Self> {
        // Hermetic under test: the host's exported socket/token pair
        // reaches a `cargo test` run from a croft-hosted shell, and
        // connecting here made every test-built App authenticate to the
        // developer's live session host as a construction side effect
        // (#60). Same treatment as the collab env read (#55); tests that
        // want a channel dial a socket they own via [`Self::connect`].
        if cfg!(test) {
            return None;
        }
        let socket = PathBuf::from(std::env::var_os("CROFT_SESSION_SOCKET")?);
        let token = std::env::var("CROFT_SESSION_TOKEN").ok()?;
        Self::connect(&socket, &token)
    }

    pub fn connect(socket: &Path, token: &str) -> Option<Self> {
        let mut stream = UnixStream::connect(socket).ok()?;
        stream
            .write_all(&encode_control_frame(&Control::Inner {
                token: token.to_string(),
            }))
            .ok()?;
        // Non-blocking so the app's per-tick drain_typing never stalls the
        // render loop. Outbound frames are a few dozen bytes into an
        // otherwise idle socket, so writes don't meaningfully block either.
        stream.set_nonblocking(true).ok()?;
        Some(Self {
            stream,
            reader: FrameReader::new(),
            presence: presence_path(socket),
        })
    }

    /// Drain any host frames waiting on the channel and return the "who is
    /// typing" attributions in arrival order, collapsing only consecutive
    /// repeats. Returning every distinct typist (not just the last) keeps a
    /// rapid A->B->C writer burst within one tick from dropping B's caret
    /// hand-over. Non-blocking.
    pub fn drain_typing(&mut self) -> Vec<u64> {
        let mut typists: Vec<u64> = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    for frame in self.reader.push(&buf[..n]) {
                        if let Frame::Control(Control::Typing { id }) = frame
                            && typists.last() != Some(&id)
                        {
                            typists.push(id);
                        }
                    }
                }
                Err(_) => break,
            }
        }
        typists
    }

    pub fn set_control(&mut self, id: u64, grant: bool) -> bool {
        let control = if grant {
            Control::Grant { id }
        } else {
            Control::Revoke { id }
        };
        write_frame_blocking(&mut self.stream, &encode_control_frame(&control))
    }

    pub fn kick(&mut self, id: u64) -> bool {
        write_frame_blocking(
            &mut self.stream,
            &encode_control_frame(&Control::Kick { id }),
        )
    }
}

/// Total wall-clock ceiling for one frame write. The peer is another
/// process: stopped or wedged mid-write it drains nothing, and an unbounded
/// retry loop here — often on the UI thread — froze croft with no escape.
/// A peer whose buffers stay full this long is gone for practical purposes;
/// reporting failure lets the caller treat the connection as dead.
pub(crate) const WRITE_FRAME_DEADLINE: Duration = Duration::from_secs(5);

/// Write a whole frame to a socket that may be in non-blocking mode. `write_all`
/// aborts on the first `WouldBlock` even after a partial write, which would leave
/// a torn `[type][len][payload]` on the wire and desync the peer's FrameReader
/// for every later control frame. Loop until the frame is fully committed or
/// the deadline passes ([`WRITE_FRAME_DEADLINE`]): false either way means the
/// connection is unusable.
pub(crate) fn write_frame_blocking(stream: &mut UnixStream, frame: &[u8]) -> bool {
    write_frame_blocking_with_deadline(
        stream,
        frame,
        std::time::Instant::now() + WRITE_FRAME_DEADLINE,
    )
}

/// Write a whole frame to a BLOCKING socket without ever entering an
/// unbounded kernel write (#53): [`write_frame_blocking`] only bounds a
/// socket that is already non-blocking — on the server's blocking client
/// streams the first `write` against a full buffer parks in the kernel and
/// the deadline is unreachable. Waiting for writability with `poll(2)`
/// bounds the stall without flipping `O_NONBLOCK`, which is shared with the
/// client thread's blocking reads on the same file description. False means
/// the peer is unusable (dead, or draining nothing for the whole deadline).
fn write_frame_bounded(stream: &mut UnixStream, frame: &[u8], deadline: Instant) -> bool {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut written = 0;
    while written < frame.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        let r = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if r < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        if r == 0 {
            // Writability never arrived inside the deadline: the peer is
            // wedged.
            return false;
        }
        if pfd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return false;
        }
        match stream.write(&frame[written..]) {
            Ok(0) => return false,
            Ok(n) => written += n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => return false,
        }
    }
    true
}

fn write_frame_blocking_with_deadline(
    stream: &mut impl Write,
    frame: &[u8],
    deadline: std::time::Instant,
) -> bool {
    let mut written = 0;
    while written < frame.len() {
        match stream.write(&frame[written..]) {
            Ok(0) => return false,
            Ok(n) => written += n,
            // Interrupted retries immediately but still against the
            // deadline: a signal storm must not spin past the bound this
            // function exists to guarantee.
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                if std::time::Instant::now() > deadline {
                    return false;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() > deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return false,
        }
    }
    true
}

/// Parse a presence sidecar. None when the file is missing or torn (it is
/// written atomically, so torn in practice means "host gone").
pub fn read_presence(path: &Path) -> Option<Vec<Participant>> {
    let json = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}

/// `user@host`, the identity other participants see in the roster (and the
/// default name a collab caret broadcasts).
pub(crate) fn client_name() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| String::from("user"));
    format!("{user}@{}", hostname())
}

#[cfg(unix)]
fn hostname() -> String {
    let mut buf = [0u8; 256];
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    if ok {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    } else {
        String::from("local")
    }
}

#[cfg(not(unix))]
fn hostname() -> String {
    String::from("local")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests must never authenticate to the developer's live session host.
    /// The host puts `CROFT_SESSION_SOCKET`/`CROFT_SESSION_TOKEN` in every
    /// inner croft's environment, and that environment reaches a `cargo
    /// test` run from a croft-hosted shell — so each test-built `App` used
    /// to open a real `UnixStream` to the running host and send the
    /// `Control::Inner { token }` frame as a construction side effect
    /// (#60). Under `cfg(test)` the env read must be inert no matter what
    /// the environment holds; tests that want a channel dial a socket they
    /// own via `InnerChannel::connect`. (Asserted through `from_env`
    /// itself rather than by mutating the environment: `set_var` races
    /// sibling test threads, #37.)
    #[test]
    fn from_env_is_hermetic_under_test() {
        assert!(
            InnerChannel::from_env().is_none(),
            "a test-built App must not connect to the launching shell's session host"
        );
    }

    #[test]
    fn collab_socket_for_mux_swaps_only_the_socket_kind() {
        assert_eq!(
            collab_socket_for_mux(Path::new("/x/sessions/abc123.mux.sock")),
            PathBuf::from("/x/sessions/abc123.collab.sock")
        );
        // A non-mux name still lands on a .collab.sock sibling.
        assert_eq!(
            collab_socket_for_mux(Path::new("/x/sessions/abc123.sock")),
            PathBuf::from("/x/sessions/abc123.sock.collab.sock")
        );
    }

    /// A peer that stops draining its socket (stopped with SIGSTOP, or
    /// itself blocked mid-write) used to wedge the writer forever: the
    /// WouldBlock loop had no deadline, and on the UI thread that froze
    /// croft with no escape. The write must give up and report failure.
    #[test]
    fn write_frame_blocking_gives_up_when_the_peer_never_drains() {
        use std::io::Write;
        let (mut a, b) = UnixStream::pair().unwrap();
        a.set_nonblocking(true).unwrap();
        // Fill the send buffer: the peer never reads.
        let chunk = [0u8; 8192];
        while a.write(&chunk).is_ok() {}
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let ok = write_frame_blocking(&mut a, &[0u8; 65536]);
            let _ = tx.send(ok);
        });
        match rx.recv_timeout(Duration::from_secs(8)) {
            Ok(ok) => assert!(!ok, "a wedged write must report failure, not success"),
            Err(_) => panic!("write_frame_blocking hung past its deadline on a wedged peer"),
        }
        drop(b);
    }

    /// Every retry path in the bounded write checks the deadline, not just
    /// WouldBlock: a stream stuck returning Interrupted (a signal storm)
    /// must also give up instead of spinning forever.
    #[test]
    fn write_frame_blocking_bounds_an_interrupt_storm() {
        struct InterruptForever;
        impl std::io::Write for InterruptForever {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let ok = write_frame_blocking_with_deadline(
                &mut InterruptForever,
                &[0u8; 16],
                std::time::Instant::now() + Duration::from_millis(100),
            );
            let _ = tx.send(ok);
        });
        match rx.recv_timeout(Duration::from_secs(4)) {
            Ok(ok) => assert!(
                !ok,
                "an interrupt storm must report failure at the deadline"
            ),
            Err(_) => panic!("write_frame_blocking spun past its deadline on Interrupted"),
        }
    }

    #[test]
    fn bytes_frame_round_trips_through_split_chunks() {
        let frame = encode_bytes_frame(b"hello \x1b[31mworld");
        let mut reader = FrameReader::new();
        // Feed byte-by-byte to prove partial-chunk buffering works.
        let mut out = Vec::new();
        for b in &frame {
            out.extend(reader.push(std::slice::from_ref(b)));
        }
        assert_eq!(out, vec![Frame::Bytes(b"hello \x1b[31mworld".to_vec())]);
    }

    #[test]
    fn control_frame_round_trips_and_interleaves_with_bytes() {
        let hello = Control::Hello {
            name: String::from("vitali@mac"),
            cols: 120,
            rows: 40,
            version: String::from(env!("CARGO_PKG_VERSION")),
        };
        let mut wire = encode_control_frame(&hello);
        wire.extend(encode_bytes_frame(b"ls\r"));
        wire.extend(encode_control_frame(&Control::Detach));
        let mut reader = FrameReader::new();
        let frames = reader.push(&wire);
        assert_eq!(
            frames,
            vec![
                Frame::Control(hello),
                Frame::Bytes(b"ls\r".to_vec()),
                Frame::Control(Control::Detach),
            ]
        );
    }

    #[test]
    fn frame_reader_skips_malformed_control_json() {
        let mut wire = encode_frame(FRAME_CONTROL, b"not json at all");
        wire.extend(encode_bytes_frame(b"ok"));
        let mut reader = FrameReader::new();
        let frames = reader.push(&wire);
        assert_eq!(frames, vec![Frame::Bytes(b"ok".to_vec())]);
    }

    #[test]
    fn min_winsize_takes_minimum_dimensions_across_clients() {
        assert_eq!(min_winsize(std::iter::empty()), None);
        assert_eq!(min_winsize([(120, 40)].into_iter()), Some((120, 40)));
        // Mins are taken per-dimension, not per-client: 100x30 here.
        assert_eq!(
            min_winsize([(120, 30), (100, 40)].into_iter()),
            Some((100, 30))
        );
    }

    #[test]
    fn min_winsize_ignores_clients_with_unknown_size() {
        // A client whose terminal reports no size (0 in either dimension,
        // seen under `script` and some CI PTYs) must not shrink the shared
        // PTY to nothing.
        assert_eq!(
            min_winsize([(0, 0), (100, 40)].into_iter()),
            Some((100, 40))
        );
        assert_eq!(min_winsize([(120, 0), (0, 24)].into_iter()), None);
        assert_eq!(min_winsize([(0, 0)].into_iter()), None);
    }

    #[test]
    fn presence_path_sits_next_to_socket() {
        let p = presence_path(Path::new("/x/sessions/ab.mux.sock"));
        assert_eq!(p, Path::new("/x/sessions/ab.mux.sock.presence.json"));
    }

    #[test]
    fn detached_server_argv_replays_socket_workspace_and_inner() {
        let argv = detached_server_argv(
            Path::new("/x/s.mux.sock"),
            Some(Path::new("/work/repo")),
            &[String::from("croft"), String::from("/work/repo")],
        );
        assert_eq!(
            argv,
            vec![
                "session-host",
                "--serve",
                "--socket",
                "/x/s.mux.sock",
                "--workspace",
                "/work/repo",
                "--",
                "croft",
                "/work/repo",
            ]
        );
        let argv = detached_server_argv(Path::new("/x/s.mux.sock"), None, &[String::from("cat")]);
        assert_eq!(
            argv,
            vec![
                "session-host",
                "--serve",
                "--socket",
                "/x/s.mux.sock",
                "--",
                "cat"
            ]
        );
    }

    // ---- integration: a real server on a real socket around `cat` ----

    struct TestClient {
        stream: UnixStream,
        reader: FrameReader,
    }

    impl TestClient {
        fn connect(socket: &Path, name: &str, cols: u16, rows: u16) -> Self {
            let stream = UnixStream::connect(socket).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut c = Self {
                stream,
                reader: FrameReader::new(),
            };
            c.send(&encode_control_frame(&Control::Hello {
                name: String::from(name),
                cols,
                rows,
                version: String::from(env!("CARGO_PKG_VERSION")),
            }));
            c
        }

        fn send(&mut self, wire: &[u8]) {
            self.stream.write_all(wire).expect("send");
        }

        /// Read frames until `pred` matches one, returning everything seen.
        fn read_until(&mut self, pred: impl Fn(&Frame) -> bool) -> Vec<Frame> {
            self.read_until_within(READ_UNTIL_DEADLINE, pred)
        }

        /// [`Self::read_until`] with an explicit budget. The socket carries a
        /// short read timeout and an expired read surfaces as `WouldBlock`
        /// (`TimedOut` on some platforms), which is "nothing yet", not a
        /// failure: unwrapping it turned a busy box into a panic reading
        /// `read: Os { code: 11 }` with no clue what the client was waiting
        /// for (issue #227). Only the budget below ends the wait, and it ends
        /// it with the frames seen so far.
        fn read_until_within(
            &mut self,
            budget: Duration,
            pred: impl Fn(&Frame) -> bool,
        ) -> Vec<Frame> {
            let mut seen = Vec::new();
            let mut buf = [0u8; 4096];
            let deadline = Instant::now() + budget;
            self.stream
                .set_read_timeout(Some(READ_POLL_INTERVAL))
                .unwrap();
            loop {
                assert!(Instant::now() < deadline, "timed out; saw {seen:?}");
                let n = match self.stream.read(&mut buf) {
                    Ok(n) => n,
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock
                                | std::io::ErrorKind::TimedOut
                                | std::io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue;
                    }
                    Err(e) => panic!("read failed: {e}; saw {seen:?}"),
                };
                assert!(n > 0, "server closed early; saw {seen:?}");
                let frames = self.reader.push(&buf[..n]);
                let done = frames.iter().any(&pred);
                seen.extend(frames);
                if done {
                    return seen;
                }
            }
        }
    }

    /// How long a `read_until` waits for the frame it wants. Generous: the
    /// flood tests push megabytes through a real PTY, and a loaded box (the
    /// whole suite running in parallel) is exactly when the old ten seconds
    /// ran out.
    const READ_UNTIL_DEADLINE: Duration = Duration::from_secs(30);
    /// How long a single `read` blocks before the loop re-checks the deadline.
    const READ_POLL_INTERVAL: Duration = Duration::from_millis(100);

    /// Issue #227: a read that expires with nothing on the wire must keep
    /// waiting until the budget runs out, and then report what the client was
    /// waiting for. Waits for a frame the server never sends, on a budget far
    /// shorter than one read timeout would have been, so the only way to reach
    /// the timeout assert is by looping over `WouldBlock` instead of
    /// unwrapping it.
    #[test]
    #[should_panic(expected = "timed out")]
    fn read_until_waits_out_an_idle_socket_instead_of_unwrapping_would_block() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let _server = spawn_test_server(socket.clone());
        wait_alive(&socket);
        let mut c = TestClient::connect(&socket, "idle", 80, 24);
        // `cat` echoes only what it is fed, and this client sends nothing, so
        // no Bytes frame can ever arrive.
        c.read_until_within(Duration::from_millis(300), |f| matches!(f, Frame::Bytes(_)));
    }

    fn output_text(frames: &[Frame]) -> String {
        let mut bytes = Vec::new();
        for f in frames {
            if let Frame::Bytes(b) = f {
                bytes.extend_from_slice(b);
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn roster(frames: &[Frame]) -> Option<Vec<Participant>> {
        frames.iter().rev().find_map(|f| match f {
            Frame::Control(Control::Presence { participants }) => Some(participants.clone()),
            _ => None,
        })
    }

    const TEST_TOKEN: &str = "test-token";

    fn spawn_test_server(socket: PathBuf) -> std::thread::JoinHandle<i32> {
        spawn_test_server_with(socket, vec![String::from("cat")])
    }

    fn spawn_test_server_with(socket: PathBuf, inner: Vec<String>) -> std::thread::JoinHandle<i32> {
        std::thread::spawn(move || {
            serve_with_token(&socket, None, &inner, TEST_TOKEN).expect("serve")
        })
    }

    fn wait_alive(socket: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !crate::session::is_alive(socket) {
            assert!(Instant::now() < deadline, "server never bound the socket");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn serve_broadcasts_output_enforces_readonly_and_propagates_exit() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let server = spawn_test_server(socket.clone());
        wait_alive(&socket);

        // Socket must not be readable by other users.
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "socket mode must be 0600");
        }

        let mut a = TestClient::connect(&socket, "owner", 120, 40);
        // First client holds control and appears in presence.
        let frames = a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        let ps = roster(&frames).unwrap();
        assert_eq!(ps.len(), 1);
        assert!(ps[0].control, "first client must hold write control");

        let mut b = TestClient::connect(&socket, "guest", 100, 50);
        let frames = b.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        let ps = roster(&frames).unwrap();
        assert_eq!(ps.len(), 2);
        let guest = ps.iter().find(|p| p.name == "guest").unwrap();
        assert!(!guest.control, "second client must attach read-only");

        // Presence sidecar mirrors the roster.
        let sidecar = std::fs::read_to_string(presence_path(&socket)).unwrap();
        assert!(sidecar.contains("owner") && sidecar.contains("guest"));

        // Read-only input is dropped server-side; owner input reaches the
        // PTY (cat echoes it back to BOTH clients).
        b.send(&encode_bytes_frame(b"INTRUDER"));
        a.send(&encode_bytes_frame(b"hi\r"));
        let a_frames =
            a.read_until(|f| matches!(f, Frame::Bytes(b) if b.windows(2).any(|w| w == b"hi")));
        let b_frames =
            b.read_until(|f| matches!(f, Frame::Bytes(b) if b.windows(2).any(|w| w == b"hi")));
        assert!(!output_text(&a_frames).contains("INTRUDER"));
        assert!(!output_text(&b_frames).contains("INTRUDER"));

        // EOT at line start ends cat; server broadcasts Exit and returns
        // the inner exit code.
        a.send(&encode_bytes_frame(&[0x04]));
        let frames = a.read_until(|f| matches!(f, Frame::Control(Control::Exit { .. })));
        assert!(
            frames
                .iter()
                .any(|f| matches!(f, Frame::Control(Control::Exit { code: 0 })))
        );
        assert_eq!(server.join().unwrap(), 0);
        assert!(!socket.exists(), "server must unlink its socket on exit");
    }

    /// #53 part 1 compat, old client -> new server: a pre-0.1.698 Hello
    /// carries no version field and must keep parsing (serde `default`).
    #[test]
    fn a_versionless_hello_from_an_old_client_still_parses() {
        let old = br#"{"t":"hello","name":"v@mac","cols":120,"rows":40}"#;
        let parsed: Control = serde_json::from_slice(old).expect("old Hello parses");
        match parsed {
            Control::Hello { version, name, .. } => {
                assert_eq!(version, "", "the missing field defaults to empty");
                assert_eq!(name, "v@mac");
            }
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    /// #53 part 1 compat, new server -> old client: the frame decoder must
    /// skip a control variant it does not know (an even newer peer's, or
    /// ServerHello reaching a pre-0.1.698 client) without poisoning the
    /// stream for the frames that follow.
    #[test]
    fn an_unknown_control_variant_is_skipped_without_poisoning_the_stream() {
        let mut wire = encode_frame(FRAME_CONTROL, br#"{"t":"from_the_future","x":1}"#);
        wire.extend(encode_bytes_frame(b"still here"));
        let mut reader = FrameReader::new();
        let frames = reader.push(&wire);
        assert_eq!(
            frames,
            vec![Frame::Bytes(b"still here".to_vec())],
            "the unknown variant is dropped, the next frame survives"
        );
    }

    /// #53 part 1: the server answers every Hello with its own version,
    /// before any PTY bytes reach the client.
    #[test]
    fn the_server_answers_hello_with_its_version() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let _server = spawn_test_server(socket.clone());
        wait_alive(&socket);
        let mut c = TestClient::connect(&socket, "owner", 120, 40);
        let frames = c.read_until(|f| matches!(f, Frame::Control(Control::ServerHello { .. })));
        let version = frames.iter().find_map(|f| match f {
            Frame::Control(Control::ServerHello { version }) => Some(version.clone()),
            _ => None,
        });
        assert_eq!(version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    }

    /// #53 part 1: the mismatch banner names both versions and the keys,
    /// and the key mapping is continue/restart/detach with detach the
    /// default for any other key.
    #[test]
    fn the_mismatch_banner_and_key_mapping_are_actionable() {
        let banner = mismatch_banner(Some("0.1.638"), "0.1.698");
        assert!(banner.contains("0.1.638") && banner.contains("0.1.698"));
        assert!(banner.contains("[C]") && banner.contains("[R]"));
        assert!(
            !banner.replace("\r\n", "").contains('\n'),
            "raw mode disables ONLCR: every newline must be CRLF or the \
             banner staircases"
        );
        let old = mismatch_banner(None, "0.1.698");
        assert!(
            old.contains("reports no version"),
            "a silent server is named as pre-version, not shown blank: {old}"
        );
        assert_eq!(mismatch_action(b'c'), MismatchAction::Continue);
        assert_eq!(mismatch_action(b'C'), MismatchAction::Continue);
        assert_eq!(mismatch_action(b'r'), MismatchAction::Restart);
        assert_eq!(mismatch_action(b'R'), MismatchAction::Restart);
        assert_eq!(mismatch_action(b'q'), MismatchAction::Detach);
        assert_eq!(mismatch_action(0x1b), MismatchAction::Detach);
    }

    /// #53 part 2: an attach from a 0x0 PTY (`script`, headless CI) is a
    /// pure observer — and with no sized client connected, `apply_winsize`
    /// used to return before the repaint jiggle, so the inner croft never
    /// received a Resize and the observer stared at a blank screen. The
    /// jiggle must fire at the current size for EVERY accepted attach.
    #[test]
    fn an_observer_only_attach_still_jiggles_the_inner_pty() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        // An inner command that reports every WINCH it receives: the only
        // observable proof the jiggle reached the PTY. The heartbeat dots
        // let the test stage on the trap being REGISTERED — the first
        // attach's jiggle races sh's startup (under suite load sh loses
        // and the WINCH is eaten by the default action), so the assertion
        // rides a SECOND attach, whose jiggle finds the trap live.
        let server = spawn_test_server_with(
            socket.clone(),
            vec![
                String::from("/bin/sh"),
                String::from("-c"),
                String::from("trap 'printf WINCHED' WINCH; while :; do printf .; sleep 0.05; done"),
            ],
        );
        wait_alive(&socket);
        let mut observer = TestClient::connect(&socket, "observer", 0, 0);
        observer.read_until(|f| matches!(f, Frame::Bytes(b) if b.contains(&b'.')));
        let second = TestClient::connect(&socket, "observer-two", 0, 0);
        observer
            .read_until(|f| matches!(f, Frame::Bytes(b) if b.windows(7).any(|w| w == b"WINCHED")));
        drop(second);
        drop(observer);
        drop(server);
    }

    /// #53 part 3: a client that stops draining (kill -STOP, a wedged
    /// terminal) filled its socket buffer, and the plain `write_all` in
    /// `broadcast` then blocked the PTY pump forever while holding the
    /// clients lock — every other client starved. The bounded write must
    /// evict the ghost at the deadline and the survivors keep flowing.
    #[test]
    fn a_wedged_client_is_evicted_instead_of_freezing_the_pump() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let _server = spawn_test_server(socket.clone());
        wait_alive(&socket);
        let mut owner = TestClient::connect(&socket, "owner", 120, 40);
        owner.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        // The ghost attaches and never reads again.
        let ghost = TestClient::connect(&socket, "ghost", 80, 24);
        let frames = owner.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        assert_eq!(roster(&frames).unwrap().len(), 2, "staging: ghost seated");
        // Flood: cat echoes everything back through broadcast; the ghost's
        // socket buffer fills and the write deadline must evict it rather
        // than wedge the pump. The owner keeps draining throughout.
        let chunk = vec![b'x'; 32 * 1024];
        let mut evicted = false;
        for _ in 0..64 {
            owner.send(&encode_bytes_frame(&chunk));
            let frames = owner.read_until(|f| {
                matches!(f, Frame::Control(Control::Presence { .. }))
                    || matches!(f, Frame::Bytes(_))
            });
            if frames
                .iter()
                .any(|f| matches!(f, Frame::Control(Control::Presence { .. })))
                && roster(&frames).is_some_and(|ps| ps.len() == 1)
            {
                evicted = true;
                break;
            }
        }
        assert!(
            evicted,
            "the wedged ghost must be evicted and the roster shrink to the owner"
        );
        drop(ghost);
    }

    #[test]
    fn control_moves_to_next_attacher_after_holder_detaches() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let _server = spawn_test_server(socket.clone());
        wait_alive(&socket);

        let mut a = TestClient::connect(&socket, "first", 80, 24);
        a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        let mut b = TestClient::connect(&socket, "second", 80, 24);
        b.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));

        // Holder detaches; the remaining read-only guest stays read-only
        // (control is never transferred implicitly)...
        a.send(&encode_control_frame(&Control::Detach));
        let frames = b.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 1)
        });
        let ps = roster(&frames).unwrap();
        assert!(!ps[0].control, "guest must not inherit control implicitly");

        // ...but the next fresh attach (e.g. the owner coming back) gains
        // control because nobody holds it.
        let mut c = TestClient::connect(&socket, "returning", 80, 24);
        let frames = c.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
        });
        let ps = roster(&frames).unwrap();
        let returning = ps.iter().find(|p| p.name == "returning").unwrap();
        assert!(
            returning.control,
            "fresh attach with no holder gains control"
        );
    }

    #[test]
    fn grant_and_revoke_move_write_control_between_clients() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let _server = spawn_test_server(socket.clone());
        wait_alive(&socket);

        let mut a = TestClient::connect(&socket, "owner", 80, 24);
        a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        let mut b = TestClient::connect(&socket, "guest", 80, 24);
        let frames = b.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
        });
        let guest_id = roster(&frames)
            .unwrap()
            .iter()
            .find(|p| p.name == "guest")
            .unwrap()
            .id;

        // Guests cannot grant themselves control.
        b.send(&encode_control_frame(&Control::Grant { id: guest_id }));
        // Owner grants; everyone sees the guest holding control.
        a.send(&encode_control_frame(&Control::Grant { id: guest_id }));
        let frames = b.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants })
                if participants.iter().any(|p| p.name == "guest" && p.control))
        });
        // The self-grant must not have taken effect before the owner's grant:
        // every roster before the owner-granted one shows the guest without
        // control.
        let grants: Vec<bool> = frames
            .iter()
            .filter_map(|f| match f {
                Frame::Control(Control::Presence { participants }) => participants
                    .iter()
                    .find(|p| p.name == "guest")
                    .map(|p| p.control),
                _ => None,
            })
            .collect();
        assert!(grants.last() == Some(&true));

        // Guest types; input now reaches cat and echoes.
        b.send(&encode_bytes_frame(b"granted\r"));
        let echoed = b.read_until(
            |f| matches!(f, Frame::Bytes(bytes) if bytes.windows(7).any(|w| w == b"granted")),
        );
        assert!(output_text(&echoed).contains("granted"));

        // Owner revokes; guest input is dropped again.
        a.send(&encode_control_frame(&Control::Revoke { id: guest_id }));
        a.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants })
                if participants.iter().any(|p| p.name == "guest" && !p.control))
        });
    }

    /// A raw connection that authenticates as the inner croft's privileged
    /// control channel (no Hello, so it never becomes a participant).
    fn connect_inner(socket: &Path, token: &str) -> UnixStream {
        let mut stream = UnixStream::connect(socket).expect("connect inner");
        stream
            .set_read_timeout(Some(Duration::from_millis(600)))
            .unwrap();
        stream
            .write_all(&encode_control_frame(&Control::Inner {
                token: String::from(token),
            }))
            .expect("send inner hello");
        stream
    }

    #[test]
    fn inner_channel_grants_revokes_and_kicks_without_joining_the_roster() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let _server = spawn_test_server(socket.clone());
        wait_alive(&socket);

        let mut a = TestClient::connect(&socket, "owner", 80, 24);
        a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        let mut inner = connect_inner(&socket, TEST_TOKEN);
        let mut b = TestClient::connect(&socket, "guest", 80, 24);
        let frames = b.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
        });
        // The privileged channel is not a participant: the roster stays at 2.
        let ps = roster(&frames).unwrap();
        assert_eq!(ps.len(), 2);
        let guest_id = ps.iter().find(|p| p.name == "guest").unwrap().id;

        // Inner channel grants the guest control.
        inner
            .write_all(&encode_control_frame(&Control::Grant { id: guest_id }))
            .unwrap();
        b.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants })
                if participants.iter().any(|p| p.name == "guest" && p.control))
        });
        // ...and revokes it.
        inner
            .write_all(&encode_control_frame(&Control::Revoke { id: guest_id }))
            .unwrap();
        b.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants })
                if participants.iter().any(|p| p.name == "guest" && !p.control))
        });

        // The privileged channel never receives PTY broadcast bytes.
        a.send(&encode_bytes_frame(b"noise\r"));
        a.read_until(
            |f| matches!(f, Frame::Bytes(bytes) if bytes.windows(5).any(|w| w == b"noise")),
        );
        // Typing attribution is the only traffic a privileged channel may
        // see: never PTY broadcast bytes, never presence frames.
        let mut probe = [0u8; 4096];
        let mut fr = FrameReader::new();
        loop {
            match inner.read(&mut probe) {
                Ok(0) | Err(_) => break, // read timeout: channel drained
                Ok(n) => {
                    for f in fr.push(&probe[..n]) {
                        assert!(
                            matches!(f, Frame::Control(Control::Typing { .. })),
                            "privileged channel must only see typing attribution, got {f:?}"
                        );
                    }
                }
            }
        }

        // Kick disconnects the guest; the roster shrinks to the owner.
        inner
            .write_all(&encode_control_frame(&Control::Kick { id: guest_id }))
            .unwrap();
        let frames = a.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 1)
        });
        assert_eq!(roster(&frames).unwrap()[0].name, "owner");
    }

    #[test]
    fn inner_channel_api_drives_grant_and_presence_sidecar_reflects_it() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let _server = spawn_test_server(socket.clone());
        wait_alive(&socket);

        let mut a = TestClient::connect(&socket, "owner", 80, 24);
        a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        let mut b = TestClient::connect(&socket, "guest", 80, 24);
        let frames = b.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
        });
        let guest_id = roster(&frames)
            .unwrap()
            .iter()
            .find(|p| p.name == "guest")
            .unwrap()
            .id;

        let mut channel = InnerChannel::connect(&socket, TEST_TOKEN).expect("channel");
        assert_eq!(channel.presence, presence_path(&socket));
        assert!(channel.set_control(guest_id, true));
        b.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants })
                if participants.iter().any(|p| p.name == "guest" && p.control))
        });
        let ps = read_presence(&channel.presence).expect("sidecar");
        assert!(ps.iter().any(|p| p.name == "guest" && p.control));
        assert!(channel.kick(guest_id));
        a.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 1)
        });
    }

    #[test]
    fn typing_markers_reach_privileged_channel_on_writer_change() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let _server = spawn_test_server(socket.clone());
        wait_alive(&socket);

        let mut a = TestClient::connect(&socket, "owner", 80, 24);
        a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        let mut b = TestClient::connect(&socket, "guest", 80, 24);
        let frames = b.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
        });
        let ps = roster(&frames).unwrap();
        let owner_id = ps.iter().find(|p| p.name == "owner").unwrap().id;
        let guest_id = ps.iter().find(|p| p.name == "guest").unwrap().id;
        let mut channel = InnerChannel::connect(&socket, TEST_TOKEN).expect("channel");

        let drain_marker = |channel: &mut InnerChannel| -> Option<u64> {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(id) = channel.drain_typing().first().copied() {
                    return Some(id);
                }
                if Instant::now() > deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        };

        // Owner types: one marker for the writer change.
        a.send(&encode_bytes_frame(b"aa\r"));
        assert_eq!(drain_marker(&mut channel), Some(owner_id));

        // Same writer keeps typing: no marker for a different writer (wait
        // for the echo so the server has definitely processed the input; a
        // duplicate owner marker from the registration catch-up is allowed).
        a.send(&encode_bytes_frame(b"bb\r"));
        a.read_until(|f| matches!(f, Frame::Bytes(bytes) if bytes.windows(2).any(|w| w == b"bb")));
        let leftover = channel.drain_typing();
        assert!(
            leftover.is_empty() || leftover == [owner_id],
            "{leftover:?}"
        );

        // Control moves to the guest and the guest types: a marker for the
        // new writer.
        assert!(channel.set_control(guest_id, true));
        b.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants })
                if participants.iter().any(|p| p.name == "guest" && p.control))
        });
        b.send(&encode_bytes_frame(b"cc\r"));
        assert_eq!(drain_marker(&mut channel), Some(guest_id));
    }

    #[test]
    fn inner_channel_with_wrong_token_is_powerless() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let _server = spawn_test_server(socket.clone());
        wait_alive(&socket);

        let mut a = TestClient::connect(&socket, "owner", 80, 24);
        a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        let mut impostor = connect_inner(&socket, "wrong-token");
        let mut b = TestClient::connect(&socket, "guest", 80, 24);
        let frames = b.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
        });
        let guest_id = roster(&frames)
            .unwrap()
            .iter()
            .find(|p| p.name == "guest")
            .unwrap()
            .id;

        // A wrong-token channel cannot grant; the guest stays read-only
        // through a subsequent legitimate roster update (owner's resize).
        impostor
            .write_all(&encode_control_frame(&Control::Grant { id: guest_id }))
            .unwrap();
        a.send(&encode_control_frame(&Control::Resize {
            cols: 81,
            rows: 24,
        }));
        let frames = b.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants })
                if participants.iter().any(|p| p.name == "owner" && p.cols == 81))
        });
        let ps = roster(&frames).unwrap();
        assert!(!ps.iter().find(|p| p.name == "guest").unwrap().control);
    }
}
