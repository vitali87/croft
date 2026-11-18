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

mod frame_write;
mod identity;
mod outbox;
mod protocol;
mod pty;
mod roster;
mod server;
mod sidecars;
mod swap;
pub(crate) use frame_write::WRITE_FRAME_DEADLINE;
pub(crate) use frame_write::write_frame_blocking;
use frame_write::*;
pub(crate) use identity::client_name;
use identity::*;
use outbox::*;
pub use protocol::Control;
pub use protocol::Frame;
pub use protocol::FrameReader;
pub use protocol::Participant;
pub use protocol::encode_bytes_frame;
pub use protocol::encode_control_frame;
#[cfg(test)]
use protocol::*;
use pty::*;
pub use roster::min_winsize;
pub use roster::session_version;
pub use roster::sole_detach_target;
pub use roster::stale_client_notice;
pub use roster::stale_client_notice_on_change;
use roster::*;
pub use server::serve;
#[cfg(test)]
pub(crate) use server::serve_with_token;
use server::*;
pub use sidecars::read_host_version;
pub use sidecars::read_presence;
pub use sidecars::stale_marker_path;
use sidecars::*;
use swap::*;

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

struct Client {
    id: u64,
    name: String,
    /// The version the client's Hello carried (empty when it sent none).
    version: String,
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
                    version,
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
                        // A 0x0 client is a pure observer (`croft record`,
                        // #356): it never shrinks the PTY, and it never takes
                        // control by default either, or a recorder attached
                        // first would leave the next real client read-only.
                        let pure_observer = cols == 0 && rows == 0;
                        let control = displaced.iter().any(|c| c.control)
                            || (!pure_observer && !clients.iter().any(|c| c.control));
                        if !refused {
                            clients.push(Client {
                                id,
                                name,
                                version,
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
            // attach_client reports both as Exit(0) after restoring the
            // terminal; exhaustiveness only.
            PumpOutcome::Detached | PumpOutcome::Disconnected => return Ok(0),
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
        let _ = std::fs::remove_file(host_version_path(socket));
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
    /// This client detached itself with the detach chord (#679); the
    /// session keeps running. [`attach_client`] restores the terminal and
    /// reports it as `Exit(0)`.
    Detached,
    /// The host closed the connection without an exit code: this client
    /// was disconnected (Session: Detach, Session: Participants) or the
    /// host died (#679). [`attach_client`] restores the terminal and
    /// reports it as `Exit(0)`.
    Disconnected,
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
    match result {
        // The inner croft never ran its own teardown for THIS terminal: it
        // is still running, or died without one. Undo its modes here so the
        // shell prompt comes back usable instead of stuck in the alternate
        // screen with mouse reporting and enhanced keys on (#679).
        Ok(outcome @ (PumpOutcome::Detached | PumpOutcome::Disconnected)) => {
            let alive = crate::session::is_alive(socket);
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&client_teardown_seq());
            let _ = writeln!(out, "{}", attach_end_note(&outcome, alive));
            let _ = out.flush();
            Ok(PumpOutcome::Exit(0))
        }
        other => other,
    }
}

/// The escape bytes that hand a terminal back to the shell after the inner
/// croft owned it: the same modes croft's own exit undoes (keyboard flags,
/// cursor shape, alternate screen, mouse capture, bracketed paste, host
/// colours), plus a visible cursor and plain SGR. Every sequence is a no-op
/// on a terminal that never had the mode on.
fn client_teardown_seq() -> Vec<u8> {
    let mut seq = b"\x1b[0m\x1b]110\x07\x1b]111\x07\x1b[=0;1u".to_vec();
    let _ = crossterm::execute!(
        seq,
        crossterm::event::PopKeyboardEnhancementFlags,
        crossterm::cursor::SetCursorStyle::DefaultUserShape,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
        crossterm::event::DisableFocusChange,
        crossterm::cursor::Show,
    );
    seq
}

/// The line a client prints once it has left a session (#679).
fn attach_end_note(outcome: &PumpOutcome, session_alive: bool) -> &'static str {
    match (outcome, session_alive) {
        (PumpOutcome::Detached, _) => {
            "Detached from the croft session; it keeps running. Run the same command to reattach."
        }
        (_, true) => {
            "Disconnected from the croft session; it keeps running. Run the same command to reattach."
        }
        (_, false) => "The croft session ended.",
    }
}

/// Whether stdin has input within `wait` (false on timeout or error).
fn stdin_readable_within(wait: Duration) -> bool {
    let mut pfd = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = wait.as_millis().clamp(0, i32::MAX as u128) as i32;
    (unsafe { libc::poll(&mut pfd, 1, ms) }) > 0
}

/// `Cmd+K Shift+Q` (Session: Detach, #679) spotted in a client's own
/// keystrokes, so a client can always leave: a read-only participant's
/// input never reaches the inner croft, and a wedged inner croft reads
/// nothing at all. Filtered here, before the bytes are forwarded.
///
/// The `Cmd+K` leader is WITHHELD until the next key decides it: forwarded
/// at once, it would arm the inner croft's chord leader, and the next key
/// ANY participant typed would complete a chord this client started. A
/// leader followed by anything else is forwarded unchanged, and one left
/// alone is released by [`Self::flush`] after [`DETACH_CHORD_HOLD`].
///
/// `Cmd` reaches the client only as a kitty-protocol key event (CSI 107
/// with the super bit, which croft's keyboard flags turn on, or iTerm2's
/// mapping that sends the same bytes), so Ctrl+K, the editor's kill-line,
/// is never taken.
#[derive(Default)]
struct DetachChord {
    /// The withheld leader, plus any release/repeat events of it.
    leader: Vec<u8>,
    /// An escape sequence cut off at the end of a read, completed by the
    /// next one.
    partial: Vec<u8>,
}

/// How long a lone `Cmd+K` is withheld before it goes to the inner croft
/// anyway, so its chord hint still shows for a user who pauses.
const DETACH_CHORD_HOLD: Duration = Duration::from_millis(1000);

impl DetachChord {
    /// Filter one read of stdin: the bytes to forward, and whether the
    /// detach chord was typed (everything from it on is dropped).
    fn feed(&mut self, input: &[u8]) -> (Vec<u8>, bool) {
        let mut data = std::mem::take(&mut self.partial);
        data.extend_from_slice(input);
        let mut out = Vec::with_capacity(data.len());
        let mut i = 0;
        while i < data.len() {
            let Some(len) = token_len(&data[i..]) else {
                // Only what could still become the chord is withheld: a
                // lone ESC is the Esc key, and must never wait for a key
                // that may not come.
                if may_become_chord_key(&data[i..]) {
                    self.partial = data[i..].to_vec();
                } else {
                    out.append(&mut self.leader);
                    out.extend_from_slice(&data[i..]);
                }
                break;
            };
            let token = &data[i..i + len];
            i += len;
            if !self.leader.is_empty() {
                match kitty_key(token) {
                    Some((107, mods, event)) if event != 1 && is_super_only(mods) => {
                        self.leader.extend_from_slice(token);
                        continue;
                    }
                    _ if is_detach_key(token) => {
                        self.leader.clear();
                        return (out, true);
                    }
                    _ => out.append(&mut self.leader),
                }
            }
            if matches!(kitty_key(token), Some((107, mods, 1)) if is_super_only(mods)) {
                self.leader = token.to_vec();
            } else {
                out.extend_from_slice(token);
            }
        }
        (out, false)
    }

    /// True while bytes are withheld waiting for the next key.
    fn holding(&self) -> bool {
        !self.leader.is_empty() || !self.partial.is_empty()
    }

    /// Release whatever is withheld (the pause after a lone leader).
    fn flush(&mut self) -> Vec<u8> {
        let mut out = std::mem::take(&mut self.leader);
        out.append(&mut self.partial);
        out
    }
}

/// Whether a CSI cut off at the end of a read could still turn out to be
/// the chord's leader or second key (`CSI 107;`, `CSI 113;`, `CSI 81;`).
fn may_become_chord_key(partial: &[u8]) -> bool {
    let Some(body) = partial.strip_prefix(b"\x1b[") else {
        return false;
    };
    [&b"107;"[..], b"113;", b"81;"].iter().any(|code| {
        if body.len() <= code.len() {
            code.starts_with(body)
        } else {
            body.starts_with(code)
                && body[code.len()..]
                    .iter()
                    .all(|b| b.is_ascii_digit() || *b == b':' || *b == b';')
        }
    })
}

/// Length of the input token at the start of `data`: a whole CSI sequence,
/// an ESC-prefixed key, or one byte. `None` when a CSI is cut off.
fn token_len(data: &[u8]) -> Option<usize> {
    if data[0] != 0x1b {
        return Some(1);
    }
    match data.get(1) {
        None => None,
        Some(b'[') => data[2..]
            .iter()
            .position(|b| (0x40..=0x7e).contains(b))
            .map(|p| p + 3),
        Some(_) => Some(2),
    }
}

/// A kitty-protocol key event `CSI code ; mods [: event] u` as
/// `(code, mods, event)`, with `mods` and `event` defaulting to 1.
fn kitty_key(token: &[u8]) -> Option<(u32, u32, u32)> {
    let body = token.strip_prefix(b"\x1b[")?.strip_suffix(b"u")?;
    let body = std::str::from_utf8(body).ok()?;
    let mut fields = body.split(';');
    let code = fields.next()?.split(':').next()?.parse().ok()?;
    let (mods, event) = match fields.next() {
        None => (1, 1),
        Some(m) => {
            let mut parts = m.split(':');
            let mods = parts.next()?.parse().ok()?;
            let event = parts.next().map_or(Some(1), |e| e.parse().ok())?;
            (mods, event)
        }
    };
    Some((code, mods, event))
}

/// The kitty modifier bits held, ignoring Caps Lock and Num Lock.
fn held_mods(mods: u32) -> u32 {
    mods.saturating_sub(1) & !(64 | 128)
}

fn is_super_only(mods: u32) -> bool {
    held_mods(mods) == 8
}

/// The chord's second key: `Q`, as plain text or as a kitty key event with
/// Shift (and optionally Cmd still held).
fn is_detach_key(token: &[u8]) -> bool {
    if token == b"Q" {
        return true;
    }
    matches!(
        kitty_key(token),
        Some((113 | 81, mods, 1)) if matches!(held_mods(mods), 1 | 9)
    )
}

/// Pump the attach, reconnecting across host swaps (#238): a HostSwap frame
/// means the host is re-execing into the updated binary and the socket
/// stays bound, so the EOF that follows is a handover, not a session end.
/// The stdin/resize forwarders are spawned once (first pump) and write
/// through a shared handle that reconnection re-points at the new stream.
fn attach_client_loop(socket: &Path, stream: &mut UnixStream) -> Result<PumpOutcome> {
    let tx = Arc::new(Mutex::new(stream.try_clone().context("cloning socket")?));
    let detaching = Arc::new(AtomicBool::new(false));
    let mut first_attach = true;
    loop {
        match attach_client_pump(stream, &tx, &detaching, first_attach)? {
            // A detach that raced the swap wins: the stdin forwarder has
            // already stopped, so a successor connection would only hang.
            PumpOutcome::HostSwapped if detaching.load(Ordering::SeqCst) => {
                return Ok(PumpOutcome::Detached);
            }
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
    detaching: &Arc<AtomicBool>,
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
    // A reconnect across a host swap writes nothing even when the server
    // is now NEWER than this still-running attach client: the screen
    // belongs to the inner croft, and text printed over it survives in
    // every cell that croft's next frames leave blank (#626, #652). The
    // inner croft says it instead, from this client's version in the
    // roster (`App::poll_session_presence`). Only a first attach, before
    // the inner croft's attach repaint, may show the banner.
    if first_attach && server_version.as_deref() != Some(client_version) {
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
        let detaching = Arc::clone(detaching);
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 16384];
            let mut chord = DetachChord::default();
            // A write error is not fatal: mid-swap the socket is dead only
            // for the beat the successor host takes to adopt, and the handle
            // is re-pointed on reconnect. The dropped bytes are what any
            // dead transport would lose.
            let forward = |bytes: &[u8]| {
                if !bytes.is_empty() {
                    let _ = tx.lock().unwrap().write_all(&encode_bytes_frame(bytes));
                }
            };
            loop {
                // A withheld Cmd+K goes through after a pause, so a user
                // who stops after the leader still gets its chord hint.
                if chord.holding() && !stdin_readable_within(DETACH_CHORD_HOLD) {
                    forward(&chord.flush());
                    continue;
                }
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx
                            .lock()
                            .unwrap()
                            .write_all(&encode_control_frame(&Control::Detach));
                        break;
                    }
                    Ok(n) => {
                        let (bytes, detach) = chord.feed(&buf[..n]);
                        forward(&bytes);
                        if detach {
                            detaching.store(true, Ordering::SeqCst);
                            let mut stream = tx.lock().unwrap();
                            let _ = stream.write_all(&encode_control_frame(&Control::Detach));
                            // Leave without waiting for the host to hang up:
                            // a wedged host never would. The pump's read sees
                            // the EOF and reports the detach.
                            let _ = stream.shutdown(std::net::Shutdown::Both);
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
    // The connection closing without an Exit frame: this client detached
    // itself, was disconnected, or the host died (#679).
    let closed = || {
        if detaching.load(Ordering::SeqCst) {
            PumpOutcome::Detached
        } else {
            PumpOutcome::Disconnected
        }
    };
    let mut buf = [0u8; 65536];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) => return Ok(closed()),
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
            Err(_) => return Ok(closed()),
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
    /// The host's version sidecar (#652): the version a reattaching client
    /// gets, which a host swap can move ahead of the inner croft's own.
    pub host_version: PathBuf,
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
            host_version: host_version_path(socket),
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

#[cfg(test)]
mod tests;
