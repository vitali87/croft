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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
        /// Stable identity of the attaching *client process*, so a
        /// reconnect can displace its own previous registration (#229).
        ///
        /// When an SSH transport dies the remote croft survives under the
        /// session host and the client reconnects, but the host cannot tell
        /// that the old socket is gone: a half-open SSH connection never
        /// returns EPIPE, it just stops draining. The stale entry lingers as
        /// a ghost participant — inflating the roster badge, pinning the
        /// shared winsize through `min_winsize`, and costing every broadcast
        /// its queue until it overflows.
        ///
        /// Empty (the `default`) from pre-0.1.701 clients and from any
        /// client that cannot determine one; an empty id never displaces
        /// anything, so those clients keep the old behavior exactly.
        #[serde(default)]
        client_id: String,
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
    /// A client asks for write control for itself, carrying no id: the host
    /// resolves the requester, so a client never needs to learn its own id.
    /// Granted only when NOBODY holds control (#235) — the claim exists so a
    /// roster that rests all-read-only (the #234 lockout, however a bug
    /// arrives at it) is recoverable from any attached client instead of
    /// being a one-way door. The pump sends it on seeing a vacant Presence;
    /// older hosts skip the unknown variant (the frame decoder drops
    /// malformed control payloads without poisoning the stream).
    Claim,
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
    /// Host to clients: this host is about to replace its own process image
    /// with the updated binary on disk (#238). Accepted connections cannot
    /// ride through the exec, but the listening socket does - so the client
    /// should reconnect and re-register instead of treating the coming EOF
    /// as session end. Pre-swap clients skip the unknown variant and keep
    /// the old behavior (EOF = detach), same tolerance as [`Control::Claim`].
    HostSwap,
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

/// Sidecar the host writes when it noticed its binary was replaced on disk
/// but could NOT re-exec into it (#238): the normal path is a live handoff
/// (see [`swap_to_new_image`]) - the marker is the fallback when the exec
/// fails or the handoff fds are unavailable, so the host is genuinely stuck
/// on the old image. It lets the inner croft's status bar and `croft ls`
/// say so instead of leaving the stale host undetectable (2026-08-22: a
/// freshly shipped fix was resident nowhere until the host was killed by
/// hand).
pub fn stale_marker_path(socket: &Path) -> PathBuf {
    let mut name = socket.file_name().unwrap_or_default().to_os_string();
    name.push(".host-stale");
    socket.with_file_name(name)
}

/// The (device, inode) pair that identifies the file currently at `path`.
/// An updated install replaces the binary (rsync/cargo write a new file and
/// rename it in), so the inode moving is the update signal — mtime alone
/// would also fire on a plain `touch`.
fn image_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

/// Watch this process's own binary path for replacement; when it happens,
/// re-exec into the updated image carrying the live session along (#238),
/// falling back to the #241 stale marker if the exec cannot happen. Resolved
/// at spawn time — on Linux `current_exe` reads /proc/self/exe, which is
/// still the real path here because the watcher starts before any update
/// could land.
fn spawn_stale_image_watcher(host: Arc<Host>, handoff: HandoffFds) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(start) = image_identity(&exe) else {
        return;
    };
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(30));
            // A missing file (mid-replace) reads as "not yet": the next
            // tick sees the settled state.
            let Some(now) = image_identity(&exe) else {
                continue;
            };
            if now != start {
                let err = swap_to_new_image(&host, &exe, &handoff);
                // Reached only when the exec failed: fall back to the
                // visible marker so the staleness is at least surfaced.
                let _ = std::fs::write(stale_marker_path(&host.socket), format!("{err:#}"));
                return;
            }
        }
    });
}

/// The #241 behavior, kept for hosts that cannot hand their session off
/// (no raw master fd from the pty backend): watch for replacement and
/// surface it via the marker only.
fn spawn_marker_only_watcher(socket: PathBuf) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(start) = image_identity(&exe) else {
        return;
    };
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(30));
            let Some(now) = image_identity(&exe) else {
                continue;
            };
            if now != start {
                let _ = std::fs::write(stale_marker_path(&socket), b"");
                return;
            }
        }
    });
}

/// Environment contract between a swapping host and its successor image:
/// the inherited fds/pid ride through `exec` by number, and the token must
/// survive so the inner croft's CROFT_SESSION_TOKEN still authenticates.
const RESUME_LISTENER_ENV: &str = "CROFT_SESSION_RESUME_LISTENER";
const RESUME_MASTER_ENV: &str = "CROFT_SESSION_RESUME_MASTER";
const RESUME_CHILD_ENV: &str = "CROFT_SESSION_RESUME_CHILD";
const RESUME_TOKEN_ENV: &str = "CROFT_SESSION_RESUME_TOKEN";

/// The raw handles a serve wrapper must carry across its own exec: the
/// listening socket (so the address never unbinds and reconnects queue in
/// the backlog during the swap), the PTY master (so the inner croft never
/// sees HUP), and the inner child's pid (exec preserves the parent/child
/// relationship, so the successor can still wait on it).
struct HandoffFds {
    listener: std::os::fd::RawFd,
    master: std::os::fd::RawFd,
    child_pid: u32,
}

fn set_cloexec(fd: std::os::fd::RawFd, on: bool) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    anyhow::ensure!(flags != -1, "F_GETFD({fd}) failed");
    let flags = if on {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, flags) };
    anyhow::ensure!(rc != -1, "F_SETFD({fd}) failed");
    Ok(())
}

/// Replace this serve wrapper's process image with the updated binary,
/// keeping the session alive: fds ride through the exec, the successor
/// adopts them (see [`resume_from`]) instead of binding and spawning anew,
/// and clients are told to reconnect first - their accepted connections are
/// the one thing that cannot survive. Returns only on failure.
fn swap_to_new_image(host: &Host, exe: &Path, handoff: &HandoffFds) -> anyhow::Error {
    // Shut the door BEFORE inviting anyone back (#321). The invitation below
    // sends every attached client round to reconnect within ~200ms, well
    // inside the delivery barrier and the exec that follow; a reconnect landing
    // on THIS host would be seated by a process about to vanish, and the
    // exec's EOF reads to a client as "the session ended" - which is how a
    // background update kicked everyone out of the remote session instead of
    // handing them to the successor. Latched under the clients lock so a
    // registration already in flight either finishes (and gets the broadcast)
    // or sees the latch and is refused.
    {
        let _clients = host.clients.lock().unwrap();
        host.swapping.store(true, Ordering::SeqCst);
    }
    let invited = broadcast_tracked(host, &encode_control_frame(&Control::HostSwap));
    // Wait for the invitation to LAND, not for a hopeful pause to elapse
    // (#321). Queueing is not delivery: each client's writer may be parked
    // mid-frame on an earlier chunk of PTY output for up to
    // WRITE_FRAME_DEADLINE, so a fixed beat can exec while HostSwap is still
    // sitting in a queue - and a client that never sees the invitation reads
    // the exec's EOF as the session ending, which is the whole bug. Ordering
    // does the rest: outboxes are FIFO, so PTY frames the pump keeps queueing
    // behind the invitation cannot overtake it, and the pump needs no muting.
    // Bounded, because one wedged peer must not strand the update forever.
    let deadline = Instant::now() + SWAP_INVITATION_DEADLINE;
    await_delivery(&invited, deadline);
    // Then the connections refused DURING the swap. They never joined the
    // roster, so the fence above knows nothing about them, yet their goodbye
    // is the only thing standing between them and an EOF they would read as
    // the session ending. Draining until the set is empty UNDER the clients
    // lock is what seals it: the refusal path registers its goodbye while
    // holding that same lock, so an empty set observed here cannot grow
    // before the exec below replaces this image. The guard is deliberately
    // held across the exec.
    let _sealed = loop {
        let pending = std::mem::take(&mut *host.farewells.lock().unwrap());
        if pending.is_empty() {
            let sealed = host.clients.lock().unwrap();
            if host.farewells.lock().unwrap().is_empty() {
                break sealed;
            }
            drop(sealed);
        } else {
            await_delivery(&pending, deadline);
        }
        if Instant::now() >= deadline {
            break host.clients.lock().unwrap();
        }
    };
    if let Err(e) =
        set_cloexec(handoff.listener, false).and_then(|()| set_cloexec(handoff.master, false))
    {
        // `and_then` short-circuits, so a failure on the master leaves the
        // listener already cleared: re-arm both, exactly as the post-exec
        // failure path does, or this host keeps serving with an inheritable
        // session socket that any later spawn would carry off.
        let _ = set_cloexec(handoff.listener, true);
        let _ = set_cloexec(handoff.master, true);
        // Every path that returns instead of exec'ing must reopen the door,
        // or this host refuses clients for the rest of its life.
        host.swapping.store(false, Ordering::SeqCst);
        return e;
    }
    let mut cmd = std::process::Command::new(exe);
    cmd.args(std::env::args_os().skip(1))
        .env(RESUME_LISTENER_ENV, handoff.listener.to_string())
        .env(RESUME_MASTER_ENV, handoff.master.to_string())
        .env(RESUME_CHILD_ENV, handoff.child_pid.to_string())
        .env(RESUME_TOKEN_ENV, &host.token);
    use std::os::unix::process::CommandExt;
    // exec never returns on success; on failure re-arm CLOEXEC so a later
    // unrelated spawn cannot leak the session fds.
    let err = anyhow::Error::from(cmd.exec()).context("exec of updated binary");
    let _ = set_cloexec(handoff.listener, true);
    let _ = set_cloexec(handoff.master, true);
    // No successor is coming: this host serves on (stale, and marked so by
    // the caller), which means it must seat clients again - refusing them
    // forever would leave every reconnect looping against a live session.
    host.swapping.store(false, Ordering::SeqCst);
    err
}

/// A session inherited from a predecessor image (#238).
struct ResumedSession {
    listener: std::os::unix::net::UnixListener,
    master: std::os::fd::RawFd,
    child_pid: u32,
    token: String,
}

/// Adopt a predecessor's session from the resume environment. `Ok(None)`
/// means a normal fresh start (no resume vars). Present-but-broken vars are
/// an error, not a fresh start: falling through to bind-and-spawn while a
/// predecessor's fds may still be open would run two hosts on one session.
fn resume_from(var: impl Fn(&str) -> Option<String>) -> Result<Option<ResumedSession>> {
    if var(RESUME_LISTENER_ENV).is_none() {
        return Ok(None);
    }
    let parse_fd = |name: &str| -> Result<std::os::fd::RawFd> {
        let raw = var(name).with_context(|| format!("resume var {name} missing"))?;
        let fd: std::os::fd::RawFd = raw
            .parse()
            .with_context(|| format!("resume var {name}={raw} is not an fd"))?;
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        anyhow::ensure!(
            flags != -1,
            "resume fd {fd} ({name}) did not survive the exec"
        );
        Ok(fd)
    };
    let listener_fd = parse_fd(RESUME_LISTENER_ENV)?;
    let master = parse_fd(RESUME_MASTER_ENV)?;
    let child_pid: u32 = var(RESUME_CHILD_ENV)
        .context("resume var CROFT_SESSION_RESUME_CHILD missing")?
        .parse()
        .context("resume child pid unparseable")?;
    let token = var(RESUME_TOKEN_ENV).context("resume var CROFT_SESSION_RESUME_TOKEN missing")?;
    // The fds were inherited without CLOEXEC by necessity; restore it so
    // they stop leaking into anything this image ever execs.
    set_cloexec(listener_fd, true)?;
    set_cloexec(master, true)?;
    use std::os::fd::FromRawFd;
    // SAFETY: the fd number came from our predecessor image which owned the
    // listener and cleared CLOEXEC on it expressly so it survives into this
    // process; validated live by fcntl above. Nothing else in this process
    // owns it (we are first: resume is checked before any other fd work).
    let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(listener_fd) };
    Ok(Some(ResumedSession {
        listener,
        master,
        child_pid,
        token,
    }))
}

/// The one master-side operation the host needs after startup (resize); a
/// fresh host holds the portable-pty master, a resumed host only the raw
/// master fd it inherited.
enum MasterHandle {
    Spawned(Box<dyn MasterPty + Send>),
    Adopted(std::os::fd::RawFd),
}

impl MasterHandle {
    fn resize(&self, size: PtySize) -> Result<()> {
        match self {
            Self::Spawned(master) => master.resize(size),
            Self::Adopted(fd) => {
                let ws = libc::winsize {
                    ws_row: size.rows,
                    ws_col: size.cols,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };
                let rc = unsafe { libc::ioctl(*fd, libc::TIOCSWINSZ, &ws) };
                anyhow::ensure!(rc == 0, "TIOCSWINSZ({fd}) failed");
                Ok(())
            }
        }
    }

    fn as_raw_fd(&self) -> Option<std::os::fd::RawFd> {
        match self {
            Self::Spawned(master) => master.as_raw_fd(),
            Self::Adopted(fd) => Some(*fd),
        }
    }
}

/// The inner command, waitable from either lifetime: the image that spawned
/// it (portable-pty child) or a successor image that inherited it (raw pid -
/// exec preserves the parent/child relationship, so waitpid still works).
enum ChildHandle {
    Spawned(Box<dyn portable_pty::Child + Send + Sync>),
    Adopted(u32),
}

impl ChildHandle {
    fn process_id(&self) -> Option<u32> {
        match self {
            Self::Spawned(child) => child.process_id(),
            Self::Adopted(pid) => Some(*pid),
        }
    }

    fn wait_code(&mut self) -> Result<i32> {
        match self {
            Self::Spawned(child) => Ok(child
                .wait()
                .context("waiting on inner command")?
                .exit_code() as i32),
            Self::Adopted(pid) => {
                let pid = *pid as libc::pid_t;
                loop {
                    let mut status: libc::c_int = 0;
                    let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
                    if rc == -1 {
                        let err = std::io::Error::last_os_error();
                        if err.raw_os_error() == Some(libc::EINTR) {
                            continue;
                        }
                        anyhow::bail!("waitpid({pid}): {err}");
                    }
                    if libc::WIFEXITED(status) {
                        return Ok(libc::WEXITSTATUS(status));
                    }
                    if libc::WIFSIGNALED(status) {
                        return Ok(128 + libc::WTERMSIG(status));
                    }
                }
            }
        }
    }
}

/// How many bytes of undelivered output one client may accumulate before it
/// is declared too slow to keep up. The PTY pump must never wait on a
/// socket (#228), so a peer that stops draining cannot be allowed to apply
/// backpressure — the only alternatives are dropping it or growing the
/// queue without bound. A wedged client's queue fills at the rate the inner
/// croft produces output, so this is a time bound in disguise: roughly a
/// full screen of dense repaints at a large window size, which a healthy
/// peer drains in milliseconds and a dead one never drains at all.
const CLIENT_QUEUE_LIMIT: usize = 4 * 1024 * 1024;

/// A client's outbound half: a bounded queue drained by that client's own
/// writer thread.
///
/// Senders ([`broadcast`], and every control frame the server originates)
/// only ever push here and wake the writer — they never block on the peer's
/// socket, which is what kept one wedged client from freezing everyone
/// else. When the queue exceeds [`CLIENT_QUEUE_LIMIT`] the client is marked
/// dead and its queue dropped; the writer thread then exits and the client
/// thread deregisters it, exactly as for a peer that closed the connection.
struct Outbox {
    queue: Mutex<OutboxState>,
    wake: std::sync::Condvar,
}

#[derive(Default)]
struct OutboxState {
    /// Frames awaiting delivery, oldest first.
    pending: std::collections::VecDeque<Vec<u8>>,
    /// Total bytes held in `pending`, tracked incrementally so the overflow
    /// check stays O(1) on the pump's path.
    bytes: usize,
    /// Set once the peer is gone (closed, wedged past the queue limit, or
    /// deliberately disconnected). Latches: a dead outbox never revives.
    dead: bool,
    /// Set once nothing further will be queued: the writer delivers what is
    /// already pending and then exits, instead of parking forever on a
    /// connection nobody owns (#321).
    closing: bool,
    /// Frames this outbox has accepted, and how many of them its writer has
    /// actually put on the wire. Queueing is not delivery: a writer can be
    /// parked mid-frame on a slow peer for up to [`WRITE_FRAME_DEADLINE`],
    /// so the only way to wait for ONE specific frame to land is to
    /// remember its number here and watch `delivered` reach it (#321).
    queued: u64,
    delivered: u64,
}

impl Outbox {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(OutboxState::default()),
            wake: std::sync::Condvar::new(),
        })
    }

    /// Queue one frame for delivery. Returns false once the peer is dead —
    /// either already marked, or newly over the queue limit. Never blocks
    /// on the socket, so this is safe to call from the PTY pump.
    fn push(&self, frame: &[u8]) -> bool {
        self.push_seq(frame).is_some()
    }

    /// [`push`](Self::push), reporting the frame's number in this outbox's
    /// delivery order so a caller can wait for that exact frame to reach the
    /// peer ([`delivered_through`](Self::delivered_through)). `None` means
    /// the peer is dead and nothing more will be delivered.
    fn push_seq(&self, frame: &[u8]) -> Option<u64> {
        let mut state = self.queue.lock().unwrap();
        if state.dead {
            return None;
        }
        // Overflow is judged against the BACKLOG the new frame joins, not
        // the total including it, so one frame larger than the whole limit
        // still reaches a healthy peer rather than killing it for a single
        // big repaint (a Kitty image, a full-screen redraw at a huge size).
        // What marks a peer dead is a backlog that was already at the limit
        // before this frame arrived — that only happens when it has stopped
        // draining.
        let backlog = state.bytes;
        state.bytes += frame.len();
        state.pending.push_back(frame.to_vec());
        state.queued += 1;
        let seq = state.queued;
        if backlog > CLIENT_QUEUE_LIMIT {
            state.dead = true;
            state.pending.clear();
            state.bytes = 0;
            self.wake.notify_all();
            return None;
        }
        self.wake.notify_all();
        Some(seq)
    }

    /// Count one frame as actually written to the socket. Called by the
    /// writer thread, which is the only place delivery is observable.
    fn mark_delivered(&self) {
        self.queue.lock().unwrap().delivered += 1;
    }

    /// Has the frame numbered `seq` reached the peer? A dead outbox answers
    /// true: it will never deliver anything, so a barrier waiting on it must
    /// stop waiting rather than hold everyone else up.
    fn delivered_through(&self, seq: u64) -> bool {
        let state = self.queue.lock().unwrap();
        state.dead || state.delivered >= seq
    }

    /// Deliver what is already queued, then end the connection: the writer
    /// drains the backlog and exits rather than parking on an empty queue.
    ///
    /// [`kill`](Self::kill) is the wrong tool for a goodbye frame - it
    /// clears `pending`, so the frame that says goodbye would never reach
    /// the peer. Used by the swap refusal (#321), whose whole point is that
    /// the client READS the HostSwap frame before the socket closes.
    fn close_when_drained(&self) {
        let mut state = self.queue.lock().unwrap();
        state.closing = true;
        self.wake.notify_all();
    }

    /// Mark the peer gone and wake its writer so the thread can exit.
    fn kill(&self) {
        let mut state = self.queue.lock().unwrap();
        state.dead = true;
        state.pending.clear();
        state.bytes = 0;
        self.wake.notify_all();
    }

    #[cfg(test)]
    fn is_dead(&self) -> bool {
        self.queue.lock().unwrap().dead
    }

    /// Block until a frame is available or the peer is dead. `None` means
    /// the writer thread should exit.
    fn pop_blocking(&self) -> Option<Vec<u8>> {
        let mut state = self.queue.lock().unwrap();
        loop {
            if state.dead {
                return None;
            }
            if let Some(frame) = state.pending.pop_front() {
                state.bytes -= frame.len();
                return Some(frame);
            }
            // Drained and closing: the goodbye frame is on the wire, so the
            // writer's work is done.
            if state.closing {
                return None;
            }
            state = self.wake.wait(state).unwrap();
        }
    }
}

/// Shut a client's socket down WITHOUT taking its stream lock, given the
/// raw fd captured at registration ([`Client::fd`]).
///
/// The lock is held by that client's writer thread for as long as a frame
/// write takes — up to [`WRITE_FRAME_DEADLINE`] against a wedged peer. Any
/// disconnect path that waited for it would inherit exactly the multi-second
/// stall the outbox design exists to remove (#228), and would do so on the
/// attach path, where a reconnecting client evicts its ghost (#229).
///
/// `shutdown(2)` is safe to call on a fd another thread is mid-write on, and
/// is precisely what unblocks that writer. The fd stays valid because the
/// `Client` holds an `Arc<Mutex<UnixStream>>` that owns it; worst case the
/// writer's in-flight `write` fails, which is the intent.
fn shutdown_now(fd: std::os::fd::RawFd) {
    unsafe { libc::shutdown(fd, libc::SHUT_RDWR) };
}

/// Drain `outbox` into `stream` until the peer dies. One of these runs per
/// client, so a slow socket stalls only its own thread.
fn spawn_writer(stream: Arc<Mutex<UnixStream>>, outbox: Arc<Outbox>, fd: std::os::fd::RawFd) {
    std::thread::spawn(move || {
        while let Some(frame) = outbox.pop_blocking() {
            // The deadline still applies per frame: it bounds how long this
            // thread lingers on a wedged peer before releasing the socket,
            // and no other client's output waits behind it.
            let ok = write_frame_bounded(
                &mut stream.lock().unwrap(),
                &frame,
                Instant::now() + WRITE_FRAME_DEADLINE,
            );
            if !ok {
                outbox.kill();
                break;
            }
            outbox.mark_delivered();
        }
        // Unblock the client thread's read so it deregisters promptly
        // instead of lingering until the peer happens to send something.
        shutdown_now(fd);
    });
}

struct Client {
    id: u64,
    name: String,
    cols: u16,
    rows: u16,
    control: bool,
    /// Owning handle on the socket. Nothing reads it any more — output goes
    /// through `outbox` and disconnects through `fd` — but it must stay:
    /// this `Arc` is what keeps the `UnixStream` alive, and therefore what
    /// keeps `fd` a valid descriptor rather than a recycled one.
    #[allow(dead_code)]
    tx: Arc<Mutex<UnixStream>>,
    /// This client's socket fd, captured at registration so a disconnect can
    /// call [`shutdown_now`] without waiting on the writer thread's lock.
    /// Valid for exactly as long as `tx` above is held.
    fd: std::os::fd::RawFd,
    /// Queued output for this client, drained by its own writer thread.
    outbox: Arc<Outbox>,
    /// Stable per-client-process identity from [`Control::Hello`], or empty
    /// when the client did not supply one. Used to evict this client's own
    /// stale registration on reconnect (#229).
    client_id: String,
}

struct Host {
    clients: Mutex<Vec<Client>>,
    next_id: AtomicU64,
    pty_input: Mutex<Box<dyn Write + Send>>,
    master: Mutex<MasterHandle>,
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
    /// Latched once this host has committed to replacing its own process
    /// image (#238). Every connection it has accepted dies at that `exec`,
    /// so from the latch on, a newly accepted client is told to reconnect
    /// instead of being seated (#321). Written under the `clients` lock so
    /// a registration in flight either completes (and receives the HostSwap
    /// broadcast) or sees the latch - never neither.
    swapping: AtomicBool,
    /// Goodbye frames queued for connections that arrived after the latch and
    /// were refused rather than seated. They are not in the roster, so the
    /// swap's delivery fence would not otherwise wait for them - and a refused
    /// client whose HostSwap is still queued when the exec closes its socket
    /// reads the EOF as the session ending, which is the very outcome the
    /// refusal exists to prevent (#321). Registered under the `clients` lock,
    /// so once the swap observes this empty while holding that lock, no
    /// further goodbye can appear before the exec.
    farewells: Mutex<Vec<(Arc<Outbox>, u64)>>,
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
    // A successor image after a host swap (#238): adopt the predecessor's
    // listener, PTY master, and inner child instead of binding and spawning.
    // The random token generated for this process is discarded in favor of
    // the inherited one, which the running inner croft still holds.
    if let Some(resumed) = resume_from(|name| std::env::var(name).ok())? {
        return run_resumed(socket, resumed);
    }
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
    // A fresh host runs the binary currently on disk; any stale marker left
    // by a predecessor is obsolete the moment the socket answers.
    let _ = std::fs::remove_file(stale_marker_path(socket));
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
    let child = pair
        .slave
        .spawn_command(cmd)
        .context("spawning inner command")?;
    drop(pair.slave);
    let reader = pair
        .master
        .try_clone_reader()
        .context("cloning pty reader")?;
    let writer = pair.master.take_writer().context("taking pty writer")?;
    run_host(
        socket,
        token,
        listener,
        Box::new(reader),
        writer,
        MasterHandle::Spawned(pair.master),
        ChildHandle::Spawned(child),
    )
}

/// Adopt a predecessor image's live session (#238): same socket (the fd
/// never unbound), same PTY, same inner child - only the host process is
/// new. Clients were told to reconnect; their re-Hellos rebuild the roster,
/// so the predecessor's presence sidecar and stale marker are dropped as
/// obsolete.
fn run_resumed(socket: &Path, resumed: ResumedSession) -> Result<i32> {
    let _ = std::fs::remove_file(presence_path(socket));
    let _ = std::fs::remove_file(stale_marker_path(socket));
    // Reader and writer are independent dups so each side owns its fd, same
    // shape as the fresh path's clone_reader/take_writer.
    let dup = |fd: std::os::fd::RawFd| -> Result<std::fs::File> {
        let d = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        anyhow::ensure!(d != -1, "dup of inherited master fd {fd} failed");
        use std::os::fd::FromRawFd;
        // SAFETY: freshly dup'd above, owned by nothing else.
        Ok(unsafe { std::fs::File::from_raw_fd(d) })
    };
    let reader = dup(resumed.master)?;
    let writer = dup(resumed.master)?;
    run_host(
        socket,
        &resumed.token,
        resumed.listener,
        Box::new(reader),
        Box::new(writer),
        MasterHandle::Adopted(resumed.master),
        ChildHandle::Adopted(resumed.child_pid),
    )
}

/// The host proper, agnostic of how its session came to be (freshly
/// spawned or adopted across a self-exec): pump PTY output to clients,
/// accept clients, arbitrate input, and wait on the inner command.
fn run_host(
    socket: &Path,
    token: &str,
    listener: std::os::unix::net::UnixListener,
    mut reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    master: MasterHandle,
    mut child: ChildHandle,
) -> Result<i32> {
    let host = Arc::new(Host {
        clients: Mutex::new(Vec::new()),
        next_id: AtomicU64::new(0),
        pty_input: Mutex::new(writer),
        master: Mutex::new(master),
        last_size: Mutex::new((80, 24)),
        socket: socket.to_path_buf(),
        token: token.to_string(),
        privileged: Mutex::new(Vec::new()),
        last_writer: Mutex::new(None),
        swapping: AtomicBool::new(false),
        farewells: Mutex::new(Vec::new()),
    });
    // Everything a successor image needs if THIS image is replaced on disk
    // in turn. Missing pieces (no master fd on some pty backend) fall back
    // to marker-only staleness reporting inside the watcher.
    {
        use std::os::fd::AsRawFd;
        let handoff = host
            .master
            .lock()
            .unwrap()
            .as_raw_fd()
            .zip(child.process_id())
            .map(|(master_fd, child_pid)| HandoffFds {
                listener: listener.as_raw_fd(),
                master: master_fd,
                child_pid,
            });
        match handoff {
            Some(handoff) => spawn_stale_image_watcher(Arc::clone(&host), handoff),
            None => spawn_marker_only_watcher(socket.to_path_buf()),
        }
    }

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

    let code = child.wait_code()?;
    broadcast(&host, &encode_control_frame(&Control::Exit { code }));
    let _ = std::fs::remove_file(socket);
    let _ = std::fs::remove_file(presence_path(socket));
    let _ = std::fs::remove_file(stale_marker_path(socket));
    // Drop the meta sidecar too, or a later server for this workspace inherits
    // the dead session's created time and `croft ls` reports an inflated uptime.
    crate::session::remove_meta(socket);
    Ok(code)
}

/// Queue `frame` for every connected client, pruning the ones whose
/// connection died — including a WEDGED one (#53, #228).
///
/// This runs on the PTY pump for every chunk the inner croft draws, so it
/// must never wait on a peer's socket. It doesn't: each client owns a
/// bounded [`Outbox`] drained by its own writer thread, and this only
/// pushes into those queues. A `kill -STOP`ped client stops draining, its
/// queue fills, and its writer thread — not the pump — absorbs the stall;
/// once the queue passes [`CLIENT_QUEUE_LIMIT`] the client is marked dead
/// and dropped here. Every other client keeps flowing at full speed
/// throughout, which the earlier bounded-write version could not do: it
/// paid up to [`WRITE_FRAME_DEADLINE`] inline, holding the clients lock,
/// freezing the session for everyone.
///
/// After any prune the shared size and roster are recomputed, so a dead
/// ghost releases the min winsize it was pinning. Returns whether any
/// client was pruned.
/// [`broadcast`], reporting for each client the frame number its outbox gave
/// this frame, so the caller can wait for the frame to actually LAND (see the
/// swap barrier in [`swap_to_new_image`]). Clients whose outbox refused it are
/// pruned exactly as in `broadcast`, and contribute nothing to wait on.
fn broadcast_tracked(host: &Host, frame: &[u8]) -> Vec<(Arc<Outbox>, u64)> {
    let mut targets = Vec::new();
    let pruned = {
        let mut clients = host.clients.lock().unwrap();
        let before = clients.len();
        clients.retain(|c| match c.outbox.push_seq(frame) {
            Some(seq) => {
                targets.push((Arc::clone(&c.outbox), seq));
                true
            }
            None => false,
        });
        clients.len() != before
    };
    if pruned {
        apply_winsize(host, false);
        update_presence(host);
    }
    targets
}

/// Block until every frame in `targets` has reached its peer, or `deadline`
/// passes. Bounded on purpose: a peer parked mid-write holds its own frame
/// for up to [`WRITE_FRAME_DEADLINE`], and the swap cannot wait on it
/// forever - past the deadline it is treated like any other client that
/// misses the handover.
fn await_delivery(targets: &[(Arc<Outbox>, u64)], deadline: Instant) {
    while Instant::now() < deadline {
        if targets.iter().all(|(o, seq)| o.delivered_through(*seq)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn broadcast(host: &Host, frame: &[u8]) -> bool {
    let pruned = {
        let mut clients = host.clients.lock().unwrap();
        let before = clients.len();
        clients.retain(|c| c.outbox.push(frame));
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
                        write_frame_bounded(
                            &mut tx.lock().unwrap(),
                            &encode_control_frame(&Control::Typing { id }),
                            Instant::now() + WRITE_FRAME_DEADLINE,
                        );
                    }
                }
                Frame::Control(Control::Hello {
                    name,
                    cols,
                    rows,
                    client_id,
                    ..
                }) if my_id.is_none() && !privileged => {
                    // Answer with our version FIRST: registration below makes
                    // this client a broadcast target, and the ServerHello must
                    // beat any PTY bytes so the attaching client's version
                    // phase ends on the first frame (#53). Queueing it before
                    // the client is registered preserves that ordering
                    // without writing to the socket from this thread: the
                    // outbox is FIFO and its writer is the only producer on
                    // the wire, so a direct write here could otherwise
                    // interleave mid-frame with a broadcast and desync the
                    // peer's FrameReader.
                    let outbox = Outbox::new();
                    outbox.push(&encode_control_frame(&Control::ServerHello {
                        version: String::from(env!("CARGO_PKG_VERSION")),
                    }));
                    let fd = {
                        use std::os::fd::AsRawFd;
                        tx.lock().unwrap().as_raw_fd()
                    };
                    spawn_writer(Arc::clone(&tx), Arc::clone(&outbox), fd);
                    let id = host.next_id.fetch_add(1, Ordering::Relaxed);
                    let mut refused = false;
                    let displaced = {
                        let mut clients = host.clients.lock().unwrap();
                        // This host is replacing its process image (#238):
                        // seating this client would hand it a session that
                        // ends in an EOF at the exec, which reads as "the
                        // session is over" and drops the user out of a
                        // perfectly live session (#321). Send it round to
                        // the successor instead - the listening socket
                        // never unbinds, so the reconnect lands there.
                        // Checked under this lock, the same one the latch is
                        // written under, so the decision cannot straddle it.
                        if host.swapping.load(Ordering::SeqCst) {
                            refused = true;
                            // Queue the goodbye and enrol it in the swap's
                            // delivery fence WHILE HOLDING this lock: the
                            // swap seals the fence by taking the same lock,
                            // so the pair cannot straddle the exec and leave
                            // this client with a bare EOF.
                            if let Some(seq) =
                                outbox.push_seq(&encode_control_frame(&Control::HostSwap))
                            {
                                host.farewells
                                    .lock()
                                    .unwrap()
                                    .push((Arc::clone(&outbox), seq));
                            }
                        }
                        // Evict this client's own earlier registration
                        // (#229). A reconnect after a dead SSH transport
                        // arrives as a brand-new connection while the stale
                        // one is still seated and undetectably half-open;
                        // the client_id match is what lets us tell "the same
                        // client came back" from "a second person joined".
                        // An empty id matches nothing, so clients that send
                        // none are unaffected.
                        // Partition rather than clone field by field: the
                        // ghosts are MOVED out, so a `Client` field added
                        // later cannot be silently dropped on this path.
                        let displaced: Vec<Client> = if refused || client_id.is_empty() {
                            Vec::new()
                        } else {
                            let (taken, kept): (Vec<Client>, Vec<Client>) =
                                std::mem::take(&mut *clients)
                                    .into_iter()
                                    .partition(|c| c.client_id == client_id);
                            *clients = kept;
                            taken
                        };
                        // Write control auto-attaches only when nobody holds
                        // it: the first client, or an owner reattaching after
                        // every control holder left. Everyone else starts as
                        // a read-only observer until granted. A reconnecting
                        // client inherits the control its ghost held, so a
                        // dropped transport never silently demotes the owner
                        // to read-only.
                        let control = displaced.iter().any(|c| c.control)
                            || !clients.iter().any(|c| c.control);
                        if !refused {
                            clients.push(Client {
                                id,
                                name,
                                cols,
                                rows,
                                control,
                                tx: Arc::clone(&tx),
                                fd,
                                outbox: Arc::clone(&outbox),
                                client_id,
                            });
                        }
                        displaced
                    };
                    if refused {
                        // The goodbye was queued above, under the clients
                        // lock. It rides this connection's own writer thread
                        // and the outbox is FIFO, so it lands after the
                        // ServerHello rather than interleaving mid-frame.
                        outbox.close_when_drained();
                        break 'conn;
                    }
                    my_id = Some(id);
                    // Tear the ghosts down outside the clients lock: killing
                    // an outbox wakes its writer, and shutdown unblocks the
                    // ghost's client thread so it deregisters (finding
                    // itself already gone) rather than lingering.
                    for ghost in displaced {
                        ghost.outbox.kill();
                        shutdown_now(ghost.fd);
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
                Frame::Control(Control::Claim) => {
                    if let Some(id) = my_id {
                        set_control(host, privileged, my_id, id, true);
                    }
                }
                Frame::Control(Control::Detach) => break 'conn,
                _ => {}
            }
        }
    }
    if let Some(id) = my_id {
        {
            let mut clients = host.clients.lock().unwrap();
            // Kill the outbox on the way out so this client's writer thread
            // wakes and exits instead of parking on an empty queue forever.
            if let Some(c) = clients.iter().find(|c| c.id == id) {
                c.outbox.kill();
            }
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
    // Bounded, like every other server-originated write (#228): this sits
    // directly in front of the PTY write on the keystroke path, so an inner
    // channel that stopped draining would otherwise block the typing client
    // forever on a plain `write_all`.
    channels.retain(|tx| {
        write_frame_bounded(
            &mut tx.lock().unwrap(),
            &frame,
            Instant::now() + WRITE_FRAME_DEADLINE,
        )
    });
}

/// Grant or revoke write control on `target`, but only when the requester
/// is the privileged inner channel or itself holds control: read-only
/// guests cannot promote themselves — with one exception. When NOBODY holds
/// control (the #234 lockout state, however a future bug arrives at it), any
/// participant may claim control for themselves (#235): a vacant roster
/// makes the self-claim safe, and without it the state is a one-way door
/// that only a host kill reopens. Promoting *others* from the floor stays
/// privileged.
fn set_control(host: &Host, privileged: bool, requester: Option<u64>, target: u64, grant: bool) {
    let changed = {
        let mut clients = host.clients.lock().unwrap();
        let allowed = control_change_allowed(
            privileged,
            requester,
            target,
            grant,
            clients.iter().map(|c| (c.id, c.control)),
        );
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

/// The permission rule of [`set_control`], over `(id, holds_control)` pairs
/// so it is testable without a live host. The vacant-claim arm grants no
/// privilege that a vacant roster does not already offer: a fresh attach
/// when nobody holds control gains control by the registration rule, so a
/// claim is the same acquisition without the detach/reattach churn.
fn control_change_allowed(
    privileged: bool,
    requester: Option<u64>,
    target: u64,
    grant: bool,
    roster: impl Iterator<Item = (u64, bool)> + Clone,
) -> bool {
    let vacant = !roster.clone().any(|(_, control)| control);
    privileged
        || requester.is_some_and(|id| roster.clone().any(|(cid, control)| cid == id && control))
        || (grant && vacant && requester == Some(target))
}

/// #234 defense in depth: while at least one participant is attached,
/// someone must hold write control. A roster resting all-read-only is a
/// total lockout — read-only input is dropped server-side, and
/// `set_control` normally requires a control holder, so no client could
/// recover the session (observed live 2026-08-22: a single attached
/// participant with `control: false`; only killing the host recovered it).
/// Called after every roster mutation that can remove a holder; grants to
/// the most recent attach (ids are monotonic).
/// The pump's claim trigger: this client is the ONLY attached participant
/// and holds no control — the #234 lockout state, where read-only input is
/// dropped at the host and the participants UI (inner croft, behind that
/// same gate) is unreachable, so no keystroke could ever recover the
/// session. Sole-participant only, on purpose: with others attached,
/// control transfer stays an explicit act (grant, or the fresh-attach rule
/// — see `control_moves_to_next_attacher_after_holder_detaches`), and a
/// guest's pump must never outrace a returning owner. A lone client
/// receiving the roster IS its one participant, so no id bookkeeping is
/// needed.
fn sole_participant_lacks_control(participants: &[Participant]) -> bool {
    matches!(participants, [p] if !p.control)
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
        // Kill the outbox too: the writer thread may be parked on a wedged
        // socket, and shutdown alone would leave it holding the stream lock
        // until its deadline expired.
        c.outbox.kill();
        shutdown_now(c.fd);
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
            // attach_client resolves swap handovers internally and returns
            // Exit when reconnection is exhausted; this arm is exhaustiveness
            // only.
            PumpOutcome::HostSwapped => return Ok(0),
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
        let _ = std::fs::remove_file(stale_marker_path(socket));
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
#[derive(Debug)]
pub enum PumpOutcome {
    Exit(i32),
    RestartRequested,
    /// The host announced a self-exec into an updated binary (#238): the
    /// connection is about to drop while the socket stays bound. Internal
    /// to the attach loop, which reconnects; callers never see it.
    HostSwapped,
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
    let result = attach_client_loop(socket, &mut stream);
    let _ = crossterm::terminal::disable_raw_mode();
    result
}

/// Pump the attach, reconnecting across host swaps (#238): a HostSwap frame
/// means the host is re-execing into the updated binary and the socket
/// stays bound, so the EOF that follows is a handover, not a session end.
/// The stdin/resize forwarders are spawned once (first pump) and write
/// through a shared handle that reconnection re-points at the new stream.
fn attach_client_loop(socket: &Path, stream: &mut UnixStream) -> Result<PumpOutcome> {
    let tx = Arc::new(Mutex::new(stream.try_clone().context("cloning socket")?));
    let mut first_attach = true;
    loop {
        match attach_client_pump(stream, &tx, first_attach)? {
            PumpOutcome::HostSwapped => {
                let Some(fresh) = reconnect_after_swap(socket) else {
                    // The successor never came up: from here the session is
                    // as gone as a killed server, which is an Exit(0) detach.
                    return Ok(PumpOutcome::Exit(0));
                };
                *tx.lock().unwrap() = fresh.try_clone().context("cloning socket")?;
                *stream = fresh;
                first_attach = false;
            }
            outcome => return Ok(outcome),
        }
    }
}

/// The successor host inherits the bound socket fd, so a live handover
/// answers within milliseconds - the window covers exec + adopt, not a
/// rebind; connect attempts during the gap queue in the listener backlog.
fn reconnect_after_swap(socket: &Path) -> Option<UnixStream> {
    for _ in 0..25 {
        std::thread::sleep(Duration::from_millis(200));
        if let Ok(stream) = UnixStream::connect(socket) {
            return Some(stream);
        }
    }
    None
}

fn attach_client_pump(
    stream: &mut UnixStream,
    tx: &Arc<Mutex<UnixStream>>,
    first_attach: bool,
) -> Result<PumpOutcome> {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    tx.lock()
        .unwrap()
        .write_all(&encode_control_frame(&Control::Hello {
            name: client_name(),
            cols,
            rows,
            version: String::from(env!("CARGO_PKG_VERSION")),
            client_id: client_identity(),
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
            // Best-effort for the same reason as the clear below: Darwin
            // EINVALs setsockopt once the peer closed. Buffered frames and
            // the EOF still arrive through read().
            let _ = stream.set_read_timeout(Some(remaining));
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
                            // A swap can race an attach: coalesced with the
                            // ServerHello in one read, the HostSwap would
                            // fall to the catch-all and the following EOF
                            // would read as session end.
                            Frame::Control(Control::HostSwap) => {
                                return Ok(PumpOutcome::HostSwapped);
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
        // Best-effort: Darwin refuses setsockopt (EINVAL) once the peer
        // has closed - which is exactly the state a host swap (#238) leaves
        // this socket in when the HostSwap frame raced the version phase.
        // Any buffered frames still drain below; a timeout left armed is
        // handled by the read loop tolerating WouldBlock.
        let _ = stream.set_read_timeout(None);
    }
    let client_version = env!("CARGO_PKG_VERSION");
    if server_version.as_deref() != Some(client_version) && !first_attach {
        // Reconnected across a host swap: the server is now NEWER than this
        // still-running attach client. A one-line notice, not the
        // interactive banner - parking a live handover on a keypress would
        // freeze the session for a formality.
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(
            format!(
                "\r\nsession host updated to croft {}; detach and reattach to update this client\r\n",
                server_version.as_deref().unwrap_or("?")
            )
            .as_bytes(),
        );
        let _ = out.flush();
    } else if server_version.as_deref() != Some(client_version) {
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
    // Spawned once: on a swap reconnect (#238) the shared handle is
    // re-pointed at the new stream, and a second forwarder would race the
    // first for stdin bytes.
    if first_attach {
        let tx = Arc::clone(tx);
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
                        // A write error is not fatal: mid-swap the socket is
                        // dead only for the beat the successor host takes to
                        // adopt, and the handle is re-pointed on reconnect.
                        // The dropped bytes are what any dead transport
                        // would lose.
                        let _ = tx.lock().unwrap().write_all(&encode_bytes_frame(&buf[..n]));
                    }
                }
            }
        });
    }

    // Terminal size -> resize frames. A 200ms poll instead of a SIGWINCH
    // handler keeps the client free of signal plumbing; the delay is
    // imperceptible against the terminal's own resize animation.
    // ponytail: poll, swap for signal_hook if 200ms ever reads as lag.
    if first_attach {
        let tx = Arc::clone(tx);
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
                    // Swap-tolerant like the stdin forwarder: the handle is
                    // re-pointed on reconnect, and the next size change
                    // resends through it.
                    let _ = tx.lock().unwrap().write_all(&frame);
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
            Ok(0) => return Ok(PumpOutcome::Exit(0)),
            Ok(n) => n,
            // A read timeout can survive the version phase when clearing it
            // failed (see above): an expiry on a quiet session is "nothing
            // yet", never "session over".
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => return Ok(PumpOutcome::Exit(0)),
        };
        for frame in reader.push(&buf[..n]) {
            match frame {
                Frame::Bytes(bytes) => {
                    out.write_all(&bytes).context("writing to terminal")?;
                    out.flush().context("flushing terminal")?;
                }
                Frame::Control(Control::Exit { code }) => return Ok(PumpOutcome::Exit(code)),
                // The host is re-execing into an updated binary (#238): the
                // EOF about to follow is a handover, not the session ending.
                Frame::Control(Control::HostSwap) => return Ok(PumpOutcome::HostSwapped),
                // Alone on a roster with no control holder = the #234
                // lockout (observed live 2026-08-22: one attached
                // participant, control false, and no way to type). Claim
                // control — the same right a detach/reattach already
                // grants, without asking the user to know that trick.
                // Bounded by arrival: one claim per matching Presence
                // frame, and a refused claim produces no new Presence.
                Frame::Control(Control::Presence { participants })
                    if sole_participant_lacks_control(&participants) =>
                {
                    let _ = tx
                        .lock()
                        .unwrap()
                        .write_all(&encode_control_frame(&Control::Claim));
                }
                // Other roster changes surface inside the inner croft (which
                // polls the presence sidecar), not in this thin pump.
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
    /// The stream reported EOF or a fatal error: the host this channel
    /// authenticated to is gone (killed, or swapped to a successor image,
    /// #238). The app revives a dead channel by connecting again.
    dead: bool,
    /// The presence sidecar this session's host maintains.
    pub presence: PathBuf,
    /// The host's stale-image marker (#238): present once the host noticed
    /// its binary was replaced on disk while it kept running the old image.
    pub stale_marker: PathBuf,
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
            dead: false,
            presence: presence_path(socket),
            stale_marker: stale_marker_path(socket),
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
                // EOF on a non-blocking socket means the peer closed: the
                // host died or swapped to a successor image (#238).
                Ok(0) => {
                    self.dead = true;
                    break;
                }
                Ok(n) => {
                    for frame in self.reader.push(&buf[..n]) {
                        if let Frame::Control(Control::Typing { id }) = frame
                            && typists.last() != Some(&id)
                        {
                            typists.push(id);
                        }
                    }
                }
                // WouldBlock is the normal "nothing waiting" case; anything
                // else is the connection failing under us.
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.dead = true;
                    break;
                }
            }
        }
        typists
    }

    /// True once the host side of this channel is gone; the connection never
    /// recovers by itself, so the owner should connect a fresh channel.
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    pub fn set_control(&mut self, id: u64, grant: bool) -> bool {
        let control = if grant {
            Control::Grant { id }
        } else {
            Control::Revoke { id }
        };
        let ok = write_frame_blocking(&mut self.stream, &encode_control_frame(&control));
        self.dead |= !ok;
        ok
    }

    pub fn kick(&mut self, id: u64) -> bool {
        let ok = write_frame_blocking(
            &mut self.stream,
            &encode_control_frame(&Control::Kick { id }),
        );
        self.dead |= !ok;
        ok
    }
}

/// Total wall-clock ceiling for one frame write. The peer is another
/// process: stopped or wedged mid-write it drains nothing, and an unbounded
/// retry loop here — often on the UI thread — froze croft with no escape.
/// A peer whose buffers stay full this long is gone for practical purposes;
/// reporting failure lets the caller treat the connection as dead.
pub(crate) const WRITE_FRAME_DEADLINE: Duration = Duration::from_secs(5);

/// How long a swapping host waits for its HostSwap invitation to reach the
/// clients it just sent it to (#321). One [`WRITE_FRAME_DEADLINE`] plus a
/// margin: that is the longest a writer can be parked on the frame ahead of
/// the invitation, so this covers the worst honest case while still bounding
/// the swap against a peer that never drains at all.
const SWAP_INVITATION_DEADLINE: Duration = Duration::from_secs(6);

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

/// Stable identity for this attaching client process (#229), used by the
/// host to displace the same client's earlier, now-dead registration.
///
/// The requirement is narrow and two-sided: it must be IDENTICAL across
/// reconnects of one client, and DISTINCT between genuinely separate
/// clients — two terminals on the same machine attaching to one session are
/// two real participants and neither may evict the other.
///
/// `CROFT_RELAY_KEY` alone satisfies only the first half. It is
/// `hash(launch arg)` (`remote::relay_session_id`), so two terminals running
/// `croft remote host /same/path` derive the SAME key, and the second would
/// evict the first — turning a ghost fix into a way to kick a colleague off.
/// The launch key is therefore combined with `CROFT_CLIENT_NONCE`, which
/// `remote::run_croft_session` mints once per client process, outside its
/// reconnect loop: constant across every reattach that loop performs, and
/// different in any other process.
///
/// Off the remote path both are absent and this returns empty, which
/// displaces nothing. That is deliberate: a local attach has no ghost
/// problem, because a dead unix socket reports EPIPE at once and is pruned
/// on the next broadcast.
pub(crate) fn client_identity() -> String {
    compose_client_identity(
        &std::env::var("CROFT_RELAY_KEY").unwrap_or_default(),
        &std::env::var("CROFT_CLIENT_NONCE").unwrap_or_default(),
    )
}

/// The composition rule behind [`client_identity`], split out so it can be
/// tested directly: `set_var` races sibling test threads (#37), so the env
/// read stays a one-liner and the logic worth asserting lives here.
fn compose_client_identity(relay_key: &str, nonce: &str) -> String {
    if relay_key.is_empty() || nonce.is_empty() {
        return String::new();
    }
    format!("{relay_key}.{nonce}")
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

    /// #229: the client identity must distinguish two terminals that opened
    /// the SAME path. The relay key cannot do that alone — it is a pure hash
    /// of the launch arg (`remote::relay_session_id`), so both launches
    /// derive it identically and the host would read the second attach as
    /// the first reconnecting and evict it: a ghost fix that kicks a real
    /// participant off. The per-process nonce is what separates them.
    #[test]
    fn two_clients_sharing_a_launch_arg_get_distinct_identities() {
        // Same relay key on purpose: this is the colliding case.
        let key = "same-launch-arg-hash";
        let a = compose_client_identity(key, &crate::remote::client_process_nonce());
        let b = compose_client_identity(key, &crate::remote::client_process_nonce());
        assert_ne!(
            a, b,
            "two client processes sharing one launch arg must not share an identity"
        );
        // Same process reconnecting: the nonce is minted once and carried,
        // so the identity must be stable — that is what evicts the ghost.
        let nonce = crate::remote::client_process_nonce();
        assert_eq!(
            compose_client_identity(key, &nonce),
            compose_client_identity(key, &nonce),
            "one client's identity must survive its own reconnects"
        );
        // Either half missing yields empty, which displaces nothing.
        assert_eq!(compose_client_identity("", "nonce"), "");
        assert_eq!(compose_client_identity(key, ""), "");
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
            client_id: String::from("relay-key-abc"),
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
            Self::connect_as(socket, name, cols, rows, "")
        }

        /// Attach with an explicit `client_id`, the stable per-client-process
        /// identity a reconnect reuses to displace its own ghost (#229).
        fn connect_as(socket: &Path, name: &str, cols: u16, rows: u16, client_id: &str) -> Self {
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
                client_id: String::from(client_id),
            }));
            c
        }

        fn send(&mut self, wire: &[u8]) {
            self.stream.write_all(wire).expect("send");
        }

        /// One best-effort read: the number of PTY bytes decoded, or None on
        /// a timeout. Unlike [`Self::read_until`] a quiet socket is a
        /// tolerable outcome, not a panic — used when the point of the test
        /// is to measure how promptly output flows.
        fn try_read_some(&mut self) -> Option<usize> {
            let mut buf = [0u8; 4096];
            let n = self.stream.read(&mut buf).ok()?;
            Some(
                self.reader
                    .push(&buf[..n])
                    .iter()
                    .map(|f| match f {
                        Frame::Bytes(b) => b.len(),
                        _ => 0,
                    })
                    .sum(),
            )
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
            loop {
                // Never block past the budget: a read armed with the full poll
                // interval a hair before the deadline would overshoot it.
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "timed out; saw {seen:?}");
                self.stream
                    .set_read_timeout(Some(remaining.min(READ_POLL_INTERVAL)))
                    .unwrap();
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
            Control::Hello {
                version,
                name,
                client_id,
                ..
            } => {
                assert_eq!(version, "", "the missing field defaults to empty");
                assert_eq!(name, "v@mac");
                // #229: the same tolerance covers client_id. An empty id
                // must never displace a seated participant, or an old
                // client attaching would kick every other old client off.
                assert_eq!(client_id, "", "the missing field defaults to empty");
            }
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    /// #229: a client reconnecting after a dead transport must displace its
    /// OWN earlier registration. The stale entry is undetectably half-open
    /// over SSH, so without this the roster shows a phantom second
    /// participant and it pins the shared winsize forever.
    #[test]
    fn a_reconnecting_client_displaces_its_own_ghost() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let _server = spawn_test_server(socket.clone());
        wait_alive(&socket);

        let mut first = TestClient::connect_as(&socket, "owner", 80, 24, "relay-key-1");
        first.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));

        // The transport dies but the socket stays open and undrained: this
        // is the ghost. The same client then reconnects with the same id.
        let mut back = TestClient::connect_as(&socket, "owner", 120, 40, "relay-key-1");
        // Take the last roster the server sends rather than waiting for a
        // specific size: without eviction the roster never reaches 1, and
        // the assertion below should be what reports that — naming the
        // phantom participant — instead of an opaque read timeout.
        let frames = back.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        let ps = roster(&frames).expect("presence");
        let names: Vec<&str> = ps.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            ps.len(),
            1,
            "the reconnect must displace its ghost, not seat a second \
             participant; roster is {names:?}"
        );
        // The survivor is the NEW connection, carrying its new size — proof
        // the ghost no longer pins the shared winsize at 80x24.
        assert_eq!(
            (ps[0].cols, ps[0].rows),
            (120, 40),
            "the reconnected client's size must win once the ghost is gone"
        );
        assert!(
            ps[0].control,
            "a reconnecting owner must not be silently demoted to read-only"
        );
        drop(first);
    }

    /// #229 guard rail: eviction keys on the client id, so two genuinely
    /// separate clients — including two terminals sharing a machine, which
    /// have the same user@host name — must both stay seated. Only a client
    /// with a MATCHING id displaces, and an empty id displaces nothing.
    #[test]
    fn distinct_clients_never_displace_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let _server = spawn_test_server(socket.clone());
        wait_alive(&socket);

        let mut a = TestClient::connect_as(&socket, "vitali@mac", 80, 24, "relay-key-a");
        a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        // Same human, same machine, different session: must NOT evict.
        let mut b = TestClient::connect_as(&socket, "vitali@mac", 80, 24, "relay-key-b");
        let frames = b.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 2)
        });
        assert_eq!(roster(&frames).unwrap().len(), 2, "distinct ids coexist");

        // Two id-less clients (pre-0.1.701, or a local attach) also coexist:
        // an empty id must never match another empty id.
        let mut c = TestClient::connect(&socket, "legacy", 80, 24);
        c.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        let mut d = TestClient::connect(&socket, "legacy", 80, 24);
        let frames = d.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 4)
        });
        assert_eq!(
            roster(&frames).unwrap().len(),
            4,
            "an empty client_id must never displace anything"
        );
    }

    /// #228 unit: the outbox admits frames until it passes the limit, then
    /// latches dead and drops its backlog rather than growing without bound.
    #[test]
    fn an_outbox_over_its_limit_latches_dead_and_frees_its_backlog() {
        let outbox = Outbox::new();
        let chunk = vec![b'x'; 64 * 1024];
        let mut accepted = 0;
        while outbox.push(&chunk) {
            accepted += 1;
            assert!(accepted < 1000, "the outbox must not accept without bound");
        }
        assert!(
            accepted >= CLIENT_QUEUE_LIMIT / chunk.len(),
            "a healthy peer must be allowed to buffer up to the limit"
        );
        assert!(outbox.is_dead(), "passing the limit must latch dead");
        assert_eq!(
            outbox.queue.lock().unwrap().bytes,
            0,
            "a dead outbox must release its backlog"
        );
        assert!(!outbox.push(&chunk), "dead latches: it never revives");
        assert!(
            outbox.pop_blocking().is_none(),
            "a dead outbox must release its writer thread"
        );
    }

    /// #228 unit: a single frame larger than the whole limit still reaches a
    /// healthy peer — one big repaint must not be mistaken for a wedge.
    #[test]
    fn an_outbox_delivers_a_frame_larger_than_its_limit() {
        let outbox = Outbox::new();
        let huge = vec![b'x'; CLIENT_QUEUE_LIMIT + 1];
        assert!(outbox.push(&huge), "the oversized frame is admitted");
        assert_eq!(
            outbox.pop_blocking().map(|f| f.len()),
            Some(huge.len()),
            "and is delivered intact"
        );
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

    /// #228: the invariant that matters is not merely that a wedged client
    /// is eventually evicted, but that the survivors NEVER STALL while it
    /// is wedged. With the queue-per-client design the pump only enqueues,
    /// so the owner's echo must keep arriving promptly from the very first
    /// round — long before the ghost's queue overflows and it is dropped.
    ///
    /// The bounded-write version failed this: every broadcast paid up to
    /// `WRITE_FRAME_DEADLINE` inline on the ghost while holding the clients
    /// lock, so the owner's echo arrived in multi-second lurches.
    #[test]
    fn a_wedged_client_never_stalls_the_other_clients() {
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

        // First SATURATE the ghost's kernel socket buffer. Until it is full
        // a write to the ghost still completes immediately, so the inline
        // bounded-write design would look healthy here and this test would
        // pass against the very bug it exists to catch. Push enough through
        // `cat`'s echo to fill that buffer (a few hundred KB on Linux and
        // macOS) while the owner drains its own side.
        let filler = vec![b'x'; 32 * 1024];
        let mut sender = owner.stream.try_clone().expect("clone owner socket");
        let warm = std::thread::spawn(move || {
            let frame = encode_bytes_frame(&filler);
            for _ in 0..64 {
                if sender.write_all(&frame).is_err() {
                    break;
                }
            }
        });
        // Drain roughly what was sent so the owner never wedges itself.
        // Reads are allowed to time out here: against a pump that IS blocked
        // on the ghost (the bug this test catches) the echo dries up
        // completely, and the timing assertions below are what should report
        // that — not a panic out of the warm-up.
        let mut drained = 0usize;
        let warm_deadline = Instant::now() + Duration::from_secs(20);
        while drained < 64 * 32 * 1024 && Instant::now() < warm_deadline {
            match owner.try_read_some() {
                Some(n) if n > 0 => drained += n,
                // A read that decoded no PTY bytes yet — a control frame, or
                // a frame still split across reads — is progress, not the
                // end. Breaking here would leave the ghost's buffer
                // unsaturated and quietly weaken the probe rounds below.
                Some(_) => continue,
                None => break,
            }
        }
        let _ = warm.join();

        // NOW the ghost is genuinely wedged: its buffer is full and it is
        // not reading. Every one of these round trips must still complete
        // quickly, because the pump only enqueues — the ghost's dead socket
        // never appears on the owner's critical path.
        for round in 0..8 {
            let probe = format!("probe-{round}\n");
            let started = Instant::now();
            owner.send(&encode_bytes_frame(probe.as_bytes()));
            // Wait for ANY output to come back. A healthy pump answers in
            // milliseconds; a pump blocked on the ghost answers only after
            // the ghost's write deadline expires, or not at all within the
            // socket's read timeout. Both show up as a large `elapsed`.
            while started.elapsed() < WRITE_FRAME_DEADLINE * 2 {
                match owner.try_read_some() {
                    Some(n) if n > 0 => break,
                    Some(_) => continue,
                    None => break,
                }
            }
            let elapsed = started.elapsed();
            // Generous next to a blocked round (which pays the full
            // WRITE_FRAME_DEADLINE, 5s) but far below it, so the two cases
            // can never be confused by a slow CI machine.
            assert!(
                elapsed < Duration::from_millis(500),
                "round {round}: the owner waited {elapsed:?} behind a wedged peer; \
                 the pump must never block on a client socket"
            );
        }
        drop(ghost);
    }

    /// A client whose queue grows past [`CLIENT_QUEUE_LIMIT`] is undeliverable
    /// and must be dropped, so a permanently wedged peer cannot pin the
    /// shared winsize or grow the server's memory without bound.
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
        // socket buffer fills, then its outbox grows past CLIENT_QUEUE_LIMIT
        // and the ghost is dropped.
        //
        // The writes happen on their own thread and are NOT interleaved with
        // the owner's reads. Alternating send/read would throttle the flood
        // to one chunk in flight — the ghost's backlog could never build,
        // because the sender would be rate-limited by its own draining. Here
        // the owner drains continuously (so it never wedges too and is never
        // itself evicted) while the flood runs ahead of the ghost.
        let chunk = vec![b'x'; 32 * 1024];
        // Several times the limit: the ghost's kernel socket buffer swallows
        // the first megabyte or so before any of it reaches the outbox.
        let rounds = (CLIENT_QUEUE_LIMIT / chunk.len()) * 4;
        // Only the owner holds write control, so only the owner's bytes
        // reach the PTY and come back as echo. Clone its socket so the flood
        // can run on its own thread while the reads continue below.
        let mut sender = owner.stream.try_clone().expect("clone owner socket");
        let flood = std::thread::spawn(move || {
            let frame = encode_bytes_frame(&chunk);
            for _ in 0..rounds {
                if sender.write_all(&frame).is_err() {
                    break;
                }
            }
        });
        let mut evicted = false;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            let frames = owner.read_until(|f| {
                matches!(f, Frame::Control(Control::Presence { .. }))
                    || matches!(f, Frame::Bytes(_))
            });
            if frames
                .iter()
                .any(|f| matches!(f, Frame::Control(Control::Presence { .. })))
                && roster(&frames).is_some_and(|ps| !ps.iter().any(|p| p.name == "ghost"))
            {
                evicted = true;
                break;
            }
        }
        let _ = flood.join();
        assert!(
            evicted,
            "the wedged ghost must be evicted once its backlog passes the limit"
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
    // A successor image adopting a predecessor's session (#238): the resume
    // plumbing must yield a fully working host over inherited raw fds - PTY
    // pump, roster, input arbitration, winsize via the raw-fd ioctl path,
    // and waitpid on an inner child this image never spawned.
    #[test]
    fn a_resumed_host_serves_the_inherited_session() {
        use std::os::fd::IntoRawFd;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("r.mux.sock");
        // Stand in for the predecessor image: spawn the inner command on a
        // real PTY and bind the socket, then strip everything down to the
        // raw fds/pid that actually ride through an exec.
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let child = pair
            .slave
            .spawn_command(CommandBuilder::new("cat"))
            .unwrap();
        drop(pair.slave);
        let master_fd = pair.master.as_raw_fd().expect("master fd");
        let child_pid = child.process_id().expect("child pid");
        // The predecessor never drops its handles across the exec.
        std::mem::forget(pair.master);
        std::mem::forget(child);
        let listener_fd = crate::session::bind_socket_0600(&socket)
            .unwrap()
            .into_raw_fd();

        let resumed = resume_from(|name| {
            [
                (RESUME_LISTENER_ENV, listener_fd.to_string()),
                (RESUME_MASTER_ENV, master_fd.to_string()),
                (RESUME_CHILD_ENV, child_pid.to_string()),
                (RESUME_TOKEN_ENV, String::from(TEST_TOKEN)),
            ]
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.clone())
        })
        .expect("resume vars must parse")
        .expect("resume vars are present");

        let sock = socket.clone();
        let server = std::thread::spawn(move || run_resumed(&sock, resumed).expect("resumed host"));
        wait_alive(&socket);

        let mut a = TestClient::connect(&socket, "owner", 120, 40);
        let frames = a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        let ps = roster(&frames).unwrap();
        assert!(
            ps[0].control,
            "the roster restarts empty after a swap, so the first re-Hello takes control"
        );
        // Input reaches the adopted PTY and its echo comes back.
        a.send(&encode_bytes_frame(b"PING\n"));
        a.read_until(|f| matches!(f, Frame::Bytes(b) if b.windows(4).any(|w| w == b"PING")));
        // Resize travels the raw-fd ioctl path (fresh hosts use portable-pty).
        a.send(&encode_control_frame(&Control::Resize {
            cols: 100,
            rows: 30,
        }));

        // The child predates this "image"; waitpid must still own its exit.
        unsafe { libc::kill(child_pid as libc::pid_t, libc::SIGTERM) };
        let frames = a.read_until(|f| matches!(f, Frame::Control(Control::Exit { .. })));
        assert!(
            frames
                .iter()
                .any(|f| matches!(f, Frame::Control(Control::Exit { code: 143 }))),
            "SIGTERM on the inner child must surface as exit 143 to clients"
        );
        assert_eq!(server.join().unwrap(), 143);
    }

    // Resume vars: absent = fresh start; present-but-broken = hard error,
    // because falling through to bind-and-spawn beside a predecessor's
    // still-open fds would run two hosts on one session.
    #[test]
    fn resume_vars_absent_start_fresh_and_broken_vars_refuse() {
        assert!(resume_from(|_| None).unwrap().is_none());
        let only_listener =
            |v: &'static str| move |n: &str| (n == RESUME_LISTENER_ENV).then(|| String::from(v));
        assert!(
            resume_from(only_listener("notanfd")).is_err(),
            "an unparseable fd must refuse, not fresh-start"
        );
        assert!(
            resume_from(only_listener("999999")).is_err(),
            "an fd that did not survive the exec must refuse"
        );
    }

    // The attach side of a host swap (#238): HostSwap followed by EOF must
    // reconnect and re-Hello on the same socket, not exit; the session ends
    // only on the successor's Exit frame.
    #[test]
    fn attach_reconnects_across_a_host_swap() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("swap.mux.sock");
        let listener = crate::session::bind_socket_0600(&socket).unwrap();
        let re_helloed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = Arc::clone(&re_helloed);
        let version = || String::from(env!("CARGO_PKG_VERSION"));
        let server = std::thread::spawn(move || {
            let read_hello = |stream: &mut UnixStream| -> bool {
                let mut reader = FrameReader::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        return false;
                    }
                    for f in reader.push(&buf[..n]) {
                        if matches!(f, Frame::Control(Control::Hello { .. })) {
                            return true;
                        }
                    }
                }
            };
            let (mut c1, _) = listener.accept().unwrap();
            assert!(read_hello(&mut c1), "first attach must Hello");
            c1.write_all(&encode_control_frame(&Control::ServerHello {
                version: version(),
            }))
            .unwrap();
            c1.write_all(&encode_bytes_frame(b"BEFORE")).unwrap();
            c1.write_all(&encode_control_frame(&Control::HostSwap))
                .unwrap();
            // The exec: this connection dies, the socket stays bound.
            drop(c1);
            let (mut c2, _) = listener.accept().unwrap();
            if read_hello(&mut c2) {
                seen.store(true, Ordering::SeqCst);
            }
            c2.write_all(&encode_control_frame(&Control::ServerHello {
                version: version(),
            }))
            .unwrap();
            c2.write_all(&encode_bytes_frame(b"AFTER")).unwrap();
            c2.write_all(&encode_control_frame(&Control::Exit { code: 7 }))
                .unwrap();
        });
        let mut stream = UnixStream::connect(&socket).unwrap();
        let outcome = attach_client_loop(&socket, &mut stream).expect("attach loop");
        assert!(
            matches!(outcome, PumpOutcome::Exit(7)),
            "the session must end on the SUCCESSOR's Exit, got {outcome:?}"
        );
        server.join().unwrap();
        assert!(
            re_helloed.load(Ordering::SeqCst),
            "the client must re-register after the swap"
        );
    }

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

    // 2026-08-22 (#234): a roster resting with no control holder froze the
    // session — read-only input is dropped at the host, the participants UI
    // lives in the inner croft behind that same gate, and set_control
    // normally requires a holder, so the state was a one-way door. The
    // escape hatch (#235): a client alone on a vacant roster claims control,
    // the same acquisition a detach/reattach already grants.
    #[test]
    fn a_sole_readonly_survivor_recovers_control_by_claiming() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let _server = spawn_test_server(socket.clone());
        wait_alive(&socket);
        let mut owner = TestClient::connect(&socket, "owner", 120, 40);
        owner.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        let mut guest = TestClient::connect(&socket, "guest", 100, 50);
        guest.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        owner.send(&encode_control_frame(&Control::Detach));
        let frames = guest.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants }) if participants.len() == 1)
        });
        let ps = roster(&frames).unwrap();
        assert!(
            sole_participant_lacks_control(&ps),
            "precondition: the survivor rests read-only (control never moves implicitly)"
        );
        guest.send(&encode_control_frame(&Control::Claim));
        let frames = guest.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants })
                if participants.iter().any(|p| p.control))
        });
        let ps = roster(&frames).unwrap();
        assert_eq!(ps.len(), 1);
        assert!(
            ps[0].control,
            "a sole read-only survivor's claim must be granted"
        );
    }

    // Revoking the last holder still legally rests the roster vacant
    // (control never moves implicitly); the claim is the recovery there too.
    #[test]
    fn a_self_revoked_holder_can_reclaim_a_vacant_roster() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let _server = spawn_test_server(socket.clone());
        wait_alive(&socket);
        let mut owner = TestClient::connect(&socket, "owner", 120, 40);
        let frames = owner.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        let owner_id = roster(&frames).unwrap()[0].id;
        owner.send(&encode_control_frame(&Control::Revoke { id: owner_id }));
        let frames = owner.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants })
                if participants.iter().all(|p| !p.control))
        });
        assert!(sole_participant_lacks_control(&roster(&frames).unwrap()));
        owner.send(&encode_control_frame(&Control::Claim));
        let frames = owner.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants })
                if participants.iter().any(|p| p.control))
        });
        assert!(roster(&frames).unwrap()[0].control);
    }

    // The #235 escape hatch must not become a privilege escalation: a claim
    // while someone holds control is refused. The resize after the claim is
    // a sync barrier — it forces a Presence broadcast that reflects every
    // frame the host processed before it.
    #[test]
    fn a_claim_while_control_is_held_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let _server = spawn_test_server(socket.clone());
        wait_alive(&socket);
        let mut owner = TestClient::connect(&socket, "owner", 120, 40);
        owner.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        let mut guest = TestClient::connect(&socket, "guest", 100, 50);
        guest.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        guest.send(&encode_control_frame(&Control::Claim));
        guest.send(&encode_control_frame(&Control::Resize {
            cols: 101,
            rows: 50,
        }));
        let frames = guest.read_until(|f| {
            matches!(f, Frame::Control(Control::Presence { participants })
                if participants.iter().any(|p| p.cols == 101))
        });
        let ps = roster(&frames).unwrap();
        assert!(
            !ps.iter().find(|p| p.name == "guest").unwrap().control,
            "a claim against a held roster must be refused"
        );
        assert!(ps.iter().find(|p| p.name == "owner").unwrap().control);
    }

    // The set_control permission rule, exhaustively. The vacant-claim arm's
    // precondition is unreachable through the public protocol once
    // restore_control_holder guards every mutation path, so it is pinned
    // here at the function level.
    #[test]
    fn control_change_permission_rules() {
        let held = [(1u64, true), (2u64, false)];
        let vacant = [(1u64, false), (2u64, false)];
        // The privileged inner channel may do anything.
        assert!(control_change_allowed(
            true,
            None,
            2,
            true,
            held.iter().copied()
        ));
        // A holder may grant others.
        assert!(control_change_allowed(
            false,
            Some(1),
            2,
            true,
            held.iter().copied()
        ));
        // A read-only guest may not grant itself while control is held...
        assert!(!control_change_allowed(
            false,
            Some(2),
            2,
            true,
            held.iter().copied()
        ));
        // ...nor grant OTHERS from a vacant floor...
        assert!(!control_change_allowed(
            false,
            Some(2),
            1,
            true,
            vacant.iter().copied()
        ));
        // ...nor revoke anything from a vacant floor.
        assert!(!control_change_allowed(
            false,
            Some(2),
            1,
            false,
            vacant.iter().copied()
        ));
        // The #235 escape hatch: self-grant when nobody holds control.
        assert!(control_change_allowed(
            false,
            Some(2),
            2,
            true,
            vacant.iter().copied()
        ));
    }

    // The pump claims only when ALONE on a vacant roster: with others
    // attached, control transfer stays explicit (a guest's pump must never
    // outrace a returning owner), and an empty roster has nothing to
    // recover.
    #[test]
    fn the_pump_claims_only_as_the_sole_readonly_participant() {
        let p = |control| Participant {
            id: 1,
            name: String::from("x"),
            cols: 80,
            rows: 24,
            control,
        };
        assert!(sole_participant_lacks_control(&[p(false)]));
        assert!(!sole_participant_lacks_control(&[p(true)]));
        assert!(!sole_participant_lacks_control(&[p(false), p(false)]));
        assert!(!sole_participant_lacks_control(&[p(false), p(true)]));
        assert!(!sole_participant_lacks_control(&[]));
    }

    // #238: the marker sits next to the socket like the presence sidecar,
    // and the update signal is the binary's inode moving — a plain touch of
    // the same file must not read as an update.
    #[test]
    fn stale_marker_sits_next_to_socket_and_replacement_moves_the_identity() {
        let p = stale_marker_path(Path::new("/x/sessions/ab.mux.sock"));
        assert_eq!(p, Path::new("/x/sessions/ab.mux.sock.host-stale"));

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("croft");
        std::fs::write(&bin, b"v1").unwrap();
        let start = image_identity(&bin).unwrap();
        // touch-equivalent: rewriting in place keeps the inode
        std::fs::write(&bin, b"v1-touched").unwrap();
        assert_eq!(
            image_identity(&bin).unwrap(),
            start,
            "an in-place rewrite is not a replacement"
        );
        // an install replaces: new file renamed over the old path
        let staged = dir.path().join("croft.new");
        std::fs::write(&staged, b"v2").unwrap();
        std::fs::rename(&staged, &bin).unwrap();
        assert_ne!(
            image_identity(&bin).unwrap(),
            start,
            "a rename-over is a replacement and must change the identity"
        );
    }

    /// A host wired to nothing: enough of one to drive [`client_thread`]
    /// directly, which is the only way to observe the mid-swap window
    /// without a real `exec` racing the assertions.
    fn bare_host(socket: &Path) -> Arc<Host> {
        Arc::new(Host {
            clients: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
            pty_input: Mutex::new(Box::new(Vec::new())),
            master: Mutex::new(MasterHandle::Adopted(-1)),
            last_size: Mutex::new((80, 24)),
            socket: socket.to_path_buf(),
            token: String::from(TEST_TOKEN),
            privileged: Mutex::new(Vec::new()),
            last_writer: Mutex::new(None),
            swapping: AtomicBool::new(false),
            farewells: Mutex::new(Vec::new()),
        })
    }

    // #321: a client that arrives while the host is swapping into an updated
    // image must be sent round to the successor, never seated. Seating it
    // hands it a session that ends at the exec, and the EOF that follows is
    // indistinguishable from "the session is over", which is how a
    // background update kicked every attached client out of a live remote
    // session instead of handing it over.
    #[test]
    fn a_client_arriving_mid_swap_is_told_to_reconnect_not_seated() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("swap.mux.sock");
        let listener = crate::session::bind_socket_0600(&socket).unwrap();
        let host = bare_host(&socket);
        host.swapping.store(true, Ordering::SeqCst);
        let served = Arc::clone(&host);
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            client_thread(&served, stream);
        });

        let mut c = TestClient::connect(&socket, "owner", 80, 24);
        let frames = c.read_until(|f| matches!(f, Frame::Control(Control::HostSwap)));
        assert!(
            frames
                .iter()
                .any(|f| matches!(f, Frame::Control(Control::ServerHello { .. }))),
            "the goodbye must not cost the client its version handshake; saw {frames:?}"
        );
        assert!(
            host.clients.lock().unwrap().is_empty(),
            "a refused client must never join the roster"
        );
        // ...and its goodbye is enrolled in the swap's delivery fence. Without
        // this the exec can close the socket while HostSwap is still queued,
        // handing the refused client the bare EOF the refusal exists to spare
        // it - the roster snapshot the fence starts from cannot know about a
        // connection that arrived after the latch.
        let enrolled = host.farewells.lock().unwrap();
        assert_eq!(
            enrolled.len(),
            1,
            "the refusal must enrol its goodbye for the swap to wait on"
        );
        let (outbox, seq) = &enrolled[0];
        assert!(
            outbox.delivered_through(*seq),
            "the client read the goodbye, so the fence must see it delivered"
        );
        server.join().unwrap();
        // And the connection is actually let go: the writer thread delivers
        // the goodbye and exits rather than parking on a socket nobody owns.
        c.stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut buf = [0u8; 32];
        assert_eq!(
            c.stream.read(&mut buf).unwrap_or(0),
            0,
            "a refused connection must be closed, not left hanging"
        );
    }

    // Frames are numbered as they are accepted, and only the writer's
    // successful write counts as delivery: the distinction is the whole basis
    // of the swap barrier (#321).
    #[test]
    fn an_outbox_numbers_frames_and_counts_only_what_the_writer_wrote() {
        let o = Outbox::new();
        assert_eq!(o.push_seq(b"a"), Some(1));
        assert_eq!(o.push_seq(b"b"), Some(2));
        assert!(!o.delivered_through(1), "queued is not delivered");
        o.pop_blocking();
        o.mark_delivered();
        assert!(o.delivered_through(1));
        assert!(
            !o.delivered_through(2),
            "one write does not deliver two frames"
        );
        o.pop_blocking();
        o.mark_delivered();
        assert!(o.delivered_through(2));
    }

    // #321: the host must WAIT for its HostSwap invitation to land rather
    // than sleep a hopeful beat past it. A writer parked on an earlier frame
    // against a peer that stopped reading holds the invitation behind it for
    // up to WRITE_FRAME_DEADLINE, and a client that never sees the invitation
    // reads the exec's EOF as the session ending. The wait is still bounded:
    // one wedged peer must not strand the update for everyone.
    #[test]
    fn the_swap_barrier_waits_for_delivery_and_still_bounds_a_parked_writer() {
        // A writer parked mid-frame is modelled by an outbox nothing drains:
        // the invitation is queued and never delivered.
        let parked = Outbox::new();
        let seq = parked.push_seq(b"invitation").expect("queued");
        let budget = Duration::from_millis(300);
        let started = Instant::now();
        await_delivery(&[(Arc::clone(&parked), seq)], Instant::now() + budget);
        assert!(
            started.elapsed() >= budget,
            "the barrier must actually wait for the invitation, not wave it through"
        );
        assert!(
            started.elapsed() < budget * 10,
            "but it must give up, not strand the update on one wedged peer"
        );

        // Delivery releases it at once, well inside the deadline.
        let live = Outbox::new();
        let seq = live.push_seq(b"invitation").expect("queued");
        let writer = {
            let o = Arc::clone(&live);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                o.pop_blocking();
                o.mark_delivered();
            })
        };
        let started = Instant::now();
        await_delivery(
            &[(Arc::clone(&live), seq)],
            Instant::now() + Duration::from_secs(5),
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a delivered invitation must release the barrier immediately"
        );
        writer.join().unwrap();

        // A peer that died mid-swap can never deliver anything, so it must
        // not hold the barrier for its full deadline either.
        let dead = Outbox::new();
        let seq = dead.push_seq(b"invitation").expect("queued");
        dead.kill();
        let started = Instant::now();
        await_delivery(&[(dead, seq)], Instant::now() + Duration::from_secs(5));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a dead peer must not hold the swap open"
        );
    }

    // A swap that cannot happen must reopen the door it closed. The host
    // serves on, stale and marked so; a host that kept refusing every
    // reconnect would be a worse failure than the staleness it reports.
    #[test]
    fn a_swap_that_cannot_exec_seats_clients_again() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("failed.mux.sock");
        let host = bare_host(&socket);
        let devnull = std::fs::File::open("/dev/null").unwrap();
        let handoff = HandoffFds {
            listener: devnull.as_raw_fd(),
            master: devnull.as_raw_fd(),
            child_pid: std::process::id(),
        };
        let err = swap_to_new_image(&host, Path::new("/nonexistent/croft-successor"), &handoff);
        assert!(
            format!("{err:#}").contains("exec of updated binary"),
            "the error must name the failed exec; got {err:#}"
        );
        assert!(
            !host.swapping.load(Ordering::SeqCst),
            "a host that could not swap must seat clients again"
        );
    }

    // The gate is the swap latch and nothing else: with no swap in flight the
    // same path seats the client exactly as before.
    #[test]
    fn a_client_arriving_outside_a_swap_is_seated() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("noswap.mux.sock");
        let listener = crate::session::bind_socket_0600(&socket).unwrap();
        let host = bare_host(&socket);
        let served = Arc::clone(&host);
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            client_thread(&served, stream);
        });

        let mut c = TestClient::connect(&socket, "owner", 80, 24);
        c.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        assert_eq!(
            host.clients.lock().unwrap().len(),
            1,
            "an ordinary attach must still be seated"
        );
        drop(c);
        server.join().unwrap();
    }

    // The client half of the same handover: a reconnect that lands on the
    // host still finishing its swap is answered with ServerHello + HostSwap,
    // and must reconnect AGAIN rather than treating the goodbye (or the
    // close behind it) as the end of the session.
    #[test]
    fn attach_retries_a_reconnect_the_dying_host_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("refuse.mux.sock");
        let listener = crate::session::bind_socket_0600(&socket).unwrap();
        let version = || String::from(env!("CARGO_PKG_VERSION"));
        let attaches = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&attaches);
        let server = std::thread::spawn(move || {
            let read_hello = |stream: &mut UnixStream| -> bool {
                let mut reader = FrameReader::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        return false;
                    }
                    for f in reader.push(&buf[..n]) {
                        if matches!(f, Frame::Control(Control::Hello { .. })) {
                            return true;
                        }
                    }
                }
            };
            // 1: the pre-swap host. 2: the same host, now latched, refusing
            // the reconnect it invited. 3: the successor, serving normally.
            for round in 1..=3 {
                let (mut c, _) = listener.accept().unwrap();
                assert!(read_hello(&mut c), "attach {round} must Hello");
                counted.fetch_add(1, Ordering::SeqCst);
                c.write_all(&encode_control_frame(&Control::ServerHello {
                    version: version(),
                }))
                .unwrap();
                match round {
                    1 | 2 => {
                        c.write_all(&encode_control_frame(&Control::HostSwap))
                            .unwrap();
                        // The exec: this connection dies, the socket stays
                        // bound and the client is expected to come back.
                        drop(c);
                    }
                    _ => {
                        c.write_all(&encode_bytes_frame(b"AFTER")).unwrap();
                        c.write_all(&encode_control_frame(&Control::Exit { code: 7 }))
                            .unwrap();
                    }
                }
            }
        });

        let mut stream = UnixStream::connect(&socket).unwrap();
        let outcome = attach_client_loop(&socket, &mut stream).expect("attach loop");
        assert!(
            matches!(outcome, PumpOutcome::Exit(7)),
            "a refused reconnect must be retried, not read as session end; got {outcome:?}"
        );
        server.join().unwrap();
        assert_eq!(
            attaches.load(Ordering::SeqCst),
            3,
            "the client must attach three times: original, refused, successor"
        );
    }

    // A fresh host must clear a predecessor's stale marker: the new host IS
    // the update the marker pointed at.
    #[test]
    fn a_fresh_host_clears_a_predecessors_stale_marker() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        std::fs::write(stale_marker_path(&socket), b"").unwrap();
        let server = spawn_test_server(socket.clone());
        wait_alive(&socket);
        assert!(
            !stale_marker_path(&socket).exists(),
            "the marker must not outlive the host it described"
        );
        let mut a = TestClient::connect(&socket, "owner", 80, 24);
        a.read_until(|f| matches!(f, Frame::Control(Control::Presence { .. })));
        a.send(&encode_bytes_frame(&[0x04]));
        a.read_until(|f| matches!(f, Frame::Control(Control::Exit { .. })));
        let _ = server.join();
    }
}
