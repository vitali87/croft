//! Moving a live session onto an updated binary (#238): spotting the replaced image,
//! handing the socket, PTY and child across an exec, adopting them in the successor, and
//! the fence that lets clients reconnect.

use super::*;

/// The (device, inode) pair that identifies the file currently at `path`.
/// An updated install replaces the binary (rsync/cargo write a new file and
/// rename it in), so the inode moving is the update signal — mtime alone
/// would also fire on a plain `touch`.
pub(super) fn image_identity(path: &Path) -> Option<(u64, u64)> {
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
pub(super) fn spawn_stale_image_watcher(host: Arc<Host>, handoff: HandoffFds) {
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
pub(super) fn spawn_marker_only_watcher(socket: PathBuf) {
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
pub(super) const RESUME_LISTENER_ENV: &str = "CROFT_SESSION_RESUME_LISTENER";

pub(super) const RESUME_MASTER_ENV: &str = "CROFT_SESSION_RESUME_MASTER";

pub(super) const RESUME_CHILD_ENV: &str = "CROFT_SESSION_RESUME_CHILD";

pub(super) const RESUME_TOKEN_ENV: &str = "CROFT_SESSION_RESUME_TOKEN";

/// This process's SIGPIPE action, read without changing it.
pub(super) fn sigpipe_action() -> Option<libc::sigaction> {
    let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: a null `act` only reads the current action into `old`.
    (unsafe { libc::sigaction(libc::SIGPIPE, std::ptr::null(), &mut old) } == 0).then_some(old)
}

pub(super) fn set_cloexec(fd: std::os::fd::RawFd, on: bool) -> Result<()> {
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
pub(super) fn swap_to_new_image(host: &Host, exe: &Path, handoff: &HandoffFds) -> anyhow::Error {
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
    let started = Instant::now();
    await_delivery(&invited, started + SWAP_INVITATION_DEADLINE);
    // Then the connections refused DURING the swap. They never joined the
    // roster, so the fence above knows nothing about them, yet their goodbye
    // is the only thing standing between them and an EOF they would read as
    // the session ending. Draining until the set is empty UNDER the clients
    // lock is what seals it: the refusal path registers its goodbye while
    // holding that same lock, so an empty set observed here cannot grow
    // before the exec below replaces this image. The guard is deliberately
    // held across the exec.
    let _sealed = seal_against_new_arrivals(host, started + SWAP_FENCE_CAP);
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
    // `exec` puts SIGPIPE back to its default for the new image before it
    // calls execve, and a failed exec leaves it there: this host would then
    // die of its next write to a client that hung up.
    let sigpipe = sigpipe_action();
    // exec never returns on success; on failure re-arm CLOEXEC so a later
    // unrelated spawn cannot leak the session fds.
    let err = anyhow::Error::from(cmd.exec()).context("exec of updated binary");
    if let Some(action) = sigpipe {
        // SAFETY: reinstates the action read above, before the exec.
        unsafe { libc::sigaction(libc::SIGPIPE, &action, std::ptr::null_mut()) };
    }
    let _ = set_cloexec(handoff.listener, true);
    let _ = set_cloexec(handoff.master, true);
    // No successor is coming: this host serves on (stale, and marked so by
    // the caller), which means it must seat clients again - refusing them
    // forever would leave every reconnect looping against a live session.
    host.swapping.store(false, Ordering::SeqCst);
    err
}

/// A session inherited from a predecessor image (#238).
pub(super) struct ResumedSession {
    listener: std::os::unix::net::UnixListener,
    master: std::os::fd::RawFd,
    child_pid: u32,
    token: String,
}

/// Adopt a predecessor's session from the resume environment. `Ok(None)`
/// means a normal fresh start (no resume vars). Present-but-broken vars are
/// an error, not a fresh start: falling through to bind-and-spawn while a
/// predecessor's fds may still be open would run two hosts on one session.
pub(super) fn resume_from(var: impl Fn(&str) -> Option<String>) -> Result<Option<ResumedSession>> {
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

/// Adopt a predecessor image's live session (#238): same socket (the fd
/// never unbound), same PTY, same inner child - only the host process is
/// new. Clients were told to reconnect; their re-Hellos rebuild the roster,
/// so the predecessor's presence sidecar and stale marker are dropped as
/// obsolete.
pub(super) fn run_resumed(socket: &Path, resumed: ResumedSession) -> Result<i32> {
    let _ = std::fs::remove_file(presence_path(socket));
    let _ = std::fs::remove_file(stale_marker_path(socket));
    write_host_version(socket);
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

/// Wait out the goodbyes owed to connections refused during the swap, then
/// hold the clients lock so no further one can be registered, and return that
/// guard for the caller to hold across its `exec`.
///
/// Each batch gets its own [`FAREWELL_GRACE`] rather than a slice of one
/// shared deadline: a goodbye registered as the invitation fence expires
/// would otherwise inherit a spent budget and be waved through undelivered.
/// `hard_cap` bounds the whole thing, since a refused client reconnects
/// within ~200ms and could keep the fence fed indefinitely.
pub(super) fn seal_against_new_arrivals(
    host: &Host,
    hard_cap: Instant,
) -> std::sync::MutexGuard<'_, Vec<Client>> {
    loop {
        let pending = std::mem::take(&mut *host.farewells.lock().unwrap());
        if pending.is_empty() {
            // Observed empty while holding the lock the refusal path must
            // take to register one: from here nothing new can appear.
            let sealed = host.clients.lock().unwrap();
            if host.farewells.lock().unwrap().is_empty() {
                return sealed;
            }
            drop(sealed);
        } else {
            let grace = (Instant::now() + FAREWELL_GRACE).min(hard_cap);
            await_delivery(&pending, grace);
        }
        if Instant::now() >= hard_cap {
            return host.clients.lock().unwrap();
        }
    }
}

/// How long a swapping host waits for its HostSwap invitation to reach the
/// clients it just sent it to (#321). One [`WRITE_FRAME_DEADLINE`] plus a
/// margin: that is the longest a writer can be parked on the frame ahead of
/// the invitation, so this covers the worst honest case while still bounding
/// the swap against a peer that never drains at all.
pub(super) const SWAP_INVITATION_DEADLINE: Duration = Duration::from_secs(6);

/// Grace a goodbye gets once it is registered, measured from the moment the
/// fence notices it rather than from when the swap began. A refused
/// connection's HostSwap is two small frames onto a socket accepted moments
/// ago, so this lands in milliseconds - but it has to be its OWN window: a
/// client refused a microsecond before the invitation deadline would
/// otherwise be enrolled into a budget already spent, and handed exactly the
/// bare EOF the refusal exists to prevent.
pub(super) const FAREWELL_GRACE: Duration = Duration::from_millis(250);

/// And the whole fence is capped regardless, because clients refused during
/// the swap come straight back (a reconnect every ~200ms) and could otherwise
/// keep feeding it goodbyes for as long as they cared to.
pub(super) const SWAP_FENCE_CAP: Duration = Duration::from_secs(10);
