//! The host proper: starting a session, pumping PTY output to every client, keeping the
//! PTY at the smallest client size, and publishing the roster.

use super::*;

/// The collab socket sibling of a mux socket: same directory and hash key,
/// `.collab.sock` in place of `.mux.sock` (matches the keying in
/// `session::collab_socket_path` / `remote::collab_socket_path`).
pub(super) fn collab_socket_for_mux(mux: &Path) -> PathBuf {
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
pub(super) fn random_token() -> Result<String> {
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
    write_host_version(socket);
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

/// The host proper, agnostic of how its session came to be (freshly
/// spawned or adopted across a self-exec): pump PTY output to clients,
/// accept clients, arbitrate input, and wait on the inner command.
pub(super) fn run_host(
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
    let _ = std::fs::remove_file(host_version_path(socket));
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
pub(super) fn broadcast_tracked(host: &Host, frame: &[u8]) -> Vec<(Arc<Outbox>, u64)> {
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
pub(super) fn await_delivery(targets: &[(Arc<Outbox>, u64)], deadline: Instant) {
    while Instant::now() < deadline {
        if targets.iter().all(|(o, seq)| o.delivered_through(*seq)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn broadcast(host: &Host, frame: &[u8]) -> bool {
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
pub(super) fn apply_winsize(host: &Host, force_repaint: bool) {
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
pub(super) fn update_presence(host: &Host) {
    // Held from the snapshot through the rename: every client thread and
    // the output thread call this, and unserialized an older roster could
    // land last (a participant who left, shown as attached), or one writer
    // could truncate the shared temp file under another's rename (a torn
    // file, read as nobody attached). Not held across the broadcast, which
    // can prune a client and call back in here.
    static WRITING: Mutex<()> = Mutex::new(());
    let writing = WRITING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                version: c.version.clone(),
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
    drop(writing);
    broadcast(
        host,
        &encode_control_frame(&Control::Presence { participants }),
    );
}
