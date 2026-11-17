//! The install as the connect dialog runs it, each step's output streamed to the
//! dialog and kept in the install log, and the background install beside an attached
//! session.

use super::*;

/// Run any required remote-install work inside the current process,
/// streaming all subprocess stdout/stderr lines through `log_tx` so the
/// in-TUI connect dialog can render them in its log panel. Returns the
/// adopted master untouched so the caller can hand it to
/// `launch_only` once it has surrendered the alt-screen.
pub fn install_only_streaming(
    adopted: AdoptedMaster,
    log_tx: std::sync::mpsc::Sender<String>,
    can_launch_tx: std::sync::mpsc::Sender<()>,
    confirm_fallback: &mut dyn FnMut(&str) -> bool,
) -> Result<AdoptedMaster> {
    let host_label = adopted.host.clone();
    // Mirror every line into ~/.cache/croft/install.log so the install
    // remains diagnosable after the connect dialog is gone.
    let log_tx = spawn_log_tee(install_log_path(), log_tx);
    let _ = log_tx.send(format!(
        "Install session for {host_label} (croft {})",
        env!("CARGO_PKG_VERSION")
    ));
    let _ = log_tx.send(format!("Adopting control socket for {host_label}"));
    let ssh = SshControl::adopt(adopted.clone());
    let result =
        install_only_streaming_over(&ssh, &host_label, &log_tx, &can_launch_tx, confirm_fallback);
    // Never drop `ssh`: its Drop kills the shared control master, and by now
    // the user may already be attached through it (the launch signal fires
    // before any fallible work).
    std::mem::forget(ssh);
    result.map(|_| adopted)
}

/// The body of `install_only_streaming`, split out so the SSH control
/// master can be leaked on every exit path rather than dropped (its `Drop`
/// kills the shared master the user may already be attached through).
///
/// Probes the remote first: an already-installed croft releases
/// `can_launch_tx` within one SSH roundtrip and the (re)install continues
/// behind the user's session, so `confirm_fallback` is bypassed in favour
/// of an automatic decline — there is nobody left to ask, and an
/// unrequested on-box compile would load a live session.
pub(super) fn install_only_streaming_over(
    ssh: &SshControl,
    host_label: &str,
    log_tx: &std::sync::mpsc::Sender<String>,
    can_launch_tx: &std::sync::mpsc::Sender<()>,
    confirm_fallback: &mut dyn FnMut(&str) -> bool,
) -> Result<()> {
    // If a croft is already on the remote, the user gets dropped into it
    // immediately and the (re)install proceeds in the background. The
    // running croft re-execs into the new binary once the stamp advances.
    // This probe runs BEFORE any local work so the launch signal fires
    // within one SSH roundtrip of connecting.
    let _ = log_tx.send(format!("Checking installed croft on {host_label}"));
    let present = remote_croft_present(ssh).unwrap_or(false);
    if present {
        let _ = can_launch_tx.send(());
    }
    let _ = log_tx.send("Hashing local source tree".to_string());
    let local_stamp = local_source_stamp()?;
    let _ = log_tx.send(format!(
        "Local source stamp: {local_stamp} (source: {})",
        env!("CARGO_MANIFEST_DIR")
    ));
    if let Some(warning) = source_snapshot_warning() {
        let _ = log_tx.send(warning);
    }
    if !remote_install_needed(ssh, &local_stamp)? {
        let _ = log_tx.send(format!("Croft on {host_label} is already up to date."));
        if !present {
            let _ = can_launch_tx.send(());
        }
        return Ok(());
    }
    // Mark the remote as updating before the local cross-compile so the
    // running croft shows "Updating" for the whole build+ship, not just
    // the final remote activation.
    mark_remote_updating(ssh);
    // The user may already be inside the remote croft over the interactive
    // master. Route every bulk byte of this install through a bulk lane so
    // the transfer never queues ahead of their keystrokes in the shared
    // TCP stream (SSH multiplexing is head-of-line blocking).
    let bulk = crate::remote_bulk::establish(&ssh.host, &ssh.socket_path, |msg| {
        let _ = log_tx.send(msg);
    });
    let _ = log_tx.send(format!("Installing/updating Croft on {host_label}"));
    // `present` means the user was already dropped into the running remote
    // croft: there is nobody left to ask, and an unasked-for compile on the
    // box would load their live session. Decline the slow path outright and
    // log how to get the update fast; a fresh install (dialog still up)
    // forwards the question to the caller's prompt.
    let mut gate = |reason: &str| {
        if present {
            let _ = log_tx.send(format!(
                "Update NOT installed: cross-build unavailable ({reason}). Run `croft setup-cross` on this machine, then reconnect — updates then ship a prebuilt binary in seconds."
            ));
            false
        } else {
            confirm_fallback(reason)
        }
    };
    if let Err(e) = install_remote_croft_streaming(ssh, &bulk.lane, &local_stamp, log_tx, &mut gate)
    {
        clear_remote_updating(ssh);
        return Err(e);
    }
    let _ = log_tx.send("Install complete.".to_string());
    Ok(())
}

/// Tee every install log line into a persistent file so a backgrounded
/// install stays diagnosable after its dialog is gone (the line saying WHY
/// the fast cross-build was skipped used to vanish with the connect
/// dialog). Lines flow through to `downstream` unchanged; file IO is
/// best-effort and never blocks the install.
pub(crate) fn spawn_log_tee(
    path: PathBuf,
    downstream: std::sync::mpsc::Sender<String>,
) -> std::sync::mpsc::Sender<String> {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut file = std::fs::File::create(&path).ok();
        while let Ok(line) = rx.recv() {
            if let Some(f) = file.as_mut() {
                use std::io::Write as _;
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let _ = writeln!(f, "[{secs}] {line}");
            }
            // The dialog may be gone (user already inside the remote
            // croft); keep writing the file regardless.
            let _ = downstream.send(line);
        }
    });
    tx
}

pub(super) fn install_log_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("croft").join("install.log")
}

pub(super) fn run_command_streaming(
    mut cmd: Command,
    log_tx: &std::sync::mpsc::Sender<String>,
) -> Result<std::process::ExitStatus> {
    use std::io::{BufRead, BufReader};
    // stdin must be closed, not inherited: this runs on the background install
    // thread while the attached remote session owns the terminal, and rsync and
    // the bulk lane's own ssh would otherwise read the user's keystrokes for
    // the whole multi-minute transfer.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("spawning streaming subprocess")?;
    let stdout = child.stdout.take().expect("stdout pipe");
    let stderr = child.stderr.take().expect("stderr pipe");
    let tx1 = log_tx.clone();
    let h1 = std::thread::spawn(move || {
        let r = BufReader::new(stdout);
        for line in r.lines().map_while(|l| l.ok()) {
            let _ = tx1.send(line);
        }
    });
    let tx2 = log_tx.clone();
    let h2 = std::thread::spawn(move || {
        let r = BufReader::new(stderr);
        for line in r.lines().map_while(|l| l.ok()) {
            let _ = tx2.send(line);
        }
    });
    let status = child.wait().context("waiting for subprocess")?;
    let _ = h1.join();
    let _ = h2.join();
    Ok(status)
}

/// Ship croft to the remote, preferring the fast path: cross-build a
/// static binary locally and rsync it over.
///
/// When that path is unavailable, `confirm_fallback` is asked — with the
/// reason — whether to compile on the remote box instead. Declining is an
/// error, not a silent skip, so the caller can point the user at
/// `croft setup-cross`. The slow path never engages on its own.
pub(super) fn install_remote_croft_streaming(
    ssh: &SshControl,
    lane: &crate::remote_bulk::BulkLane,
    source_stamp: &str,
    log_tx: &std::sync::mpsc::Sender<String>,
    confirm_fallback: &mut dyn FnMut(&str) -> bool,
) -> Result<()> {
    // The release binary first (#261), over the same bulk lane.
    let mut ship = |binary: &Path| -> Result<()> {
        let dest = format!("{}:.cargo/bin/croft.new", ssh.host);
        let status = ship_file_rsync_command(lane, &ssh.socket_path, binary, &dest)
            .status()
            .context("rsyncing the prebuilt croft to the remote")?;
        anyhow::ensure!(status.success(), "rsync exited with {status}");
        Ok(())
    };
    let mut log = |m: String| {
        let _ = log_tx.send(m);
    };
    match try_prebuilt_install(ssh, source_stamp, &mut ship, &mut log) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(e) => log(format!("Prebuilt install failed ({e:#}); building instead")),
    }
    let reason = match try_local_cross_install_streaming(ssh, lane, source_stamp, log_tx) {
        Ok(None) => return Ok(()),
        Ok(Some(reason)) => reason,
        Err(e) => format!("local cross-build failed: {e:#}"),
    };
    // Never slide into the slow on-box compile silently: the caller decides
    // (dialog prompt, tty prompt, or an automatic decline for background
    // updates). The fix — `croft setup-cross` — is one command away and
    // turns every future install into a seconds-long prebuilt-binary ship.
    if !confirm_fallback(&reason) {
        anyhow::bail!(
            "cross-build unavailable ({reason}); remote compile declined — run `croft setup-cross`, then reconnect for the fast install"
        );
    }
    let _ = log_tx.send(format!("Falling back to remote cargo install ({reason})"));
    let _ = log_tx.send("Syncing source tree to remote".to_string());
    sync_local_source_to_remote_streaming(ssh, lane, log_tx)?;
    let _ = log_tx
        .send("Running cargo install on remote (first time can take several minutes)".to_string());
    // The compile session is long-lived; keep it (and its streamed output)
    // off the interactive master when a dedicated lane exists.
    let mut cmd = lane.ssh_command(&ssh.socket_path);
    cmd.arg(&ssh.host).arg(remote_install_command(source_stamp));
    let status = run_command_streaming(cmd, log_tx).context("installing croft on remote")?;
    if !status.success() {
        anyhow::bail!("remote croft install failed with {status}");
    }
    Ok(())
}

/// `Ok(None)` = binary shipped via the fast path; `Ok(Some(reason))` = the
/// fast path is unavailable (the reason feeds the fallback confirmation);
/// `Err` = the fast path was attempted and broke.
pub(super) fn try_local_cross_install_streaming(
    ssh: &SshControl,
    lane: &crate::remote_bulk::BulkLane,
    source_stamp: &str,
    log_tx: &std::sync::mpsc::Sender<String>,
) -> Result<Option<String>> {
    if let Some(reason) = cross_compile_unavailable_reason() {
        let _ = log_tx.send(format!("Local cross-build skipped: {reason}"));
        return Ok(Some(reason));
    }
    let Some(triple) = remote_target_triple(ssh)? else {
        let reason = String::from("could not detect the remote architecture");
        let _ = log_tx.send(format!("Local cross-build skipped: {reason}"));
        return Ok(Some(reason));
    };
    if !rust_target_installed(triple) {
        let reason = format!(
            "rustup target `{triple}` missing (run `rustup target add {triple}` once to enable the fast path)"
        );
        let _ = log_tx.send(format!("Local cross-build skipped: {reason}"));
        return Ok(Some(reason));
    }

    let binary = cross_build_static(triple, log_tx)?;

    let mkdir = ssh.background_shell("mkdir -p \"$HOME/.cargo/bin\" \"$HOME/.cache/croft\"");
    let mkdir_status =
        run_command_streaming(mkdir, log_tx).context("creating remote install dirs")?;
    if !mkdir_status.success() {
        anyhow::bail!("remote mkdir exited with {mkdir_status}");
    }

    let dest = format!("{}:.cargo/bin/croft.new", ssh.host);
    let _ = log_tx.send(format!(
        "Rsyncing binary to {dest} (bulk lane, {} KB/s cap)",
        lane.bwlimit_kbps()
    ));
    let rsync = ship_file_rsync_command(lane, &ssh.socket_path, &binary, &dest);
    let rsync_status =
        run_command_streaming(rsync, log_tx).context("rsyncing croft binary to remote")?;
    if !rsync_status.success() {
        anyhow::bail!("rsync exited with {rsync_status}");
    }

    let act = ssh.background_shell(&activate_command(source_stamp));
    let act_status =
        run_command_streaming(act, log_tx).context("activating remote croft binary")?;
    if !act_status.success() {
        anyhow::bail!("remote activation exited with {act_status}");
    }
    let _ = log_tx.send("Installed croft on remote via local cross-build.".to_string());
    Ok(None)
}

/// Rsync the local source tree into `~/.cache/croft/source` on the remote,
/// streaming rsync's progress through `log_tx`. Only needed for the slow
/// compile-on-remote fallback; the fast path ships a built binary instead.
/// Runs over `lane` so the bulk transfer never queues ahead of the user's
/// keystrokes on the shared interactive master.
pub(super) fn sync_local_source_to_remote_streaming(
    ssh: &SshControl,
    lane: &crate::remote_bulk::BulkLane,
    log_tx: &std::sync::mpsc::Sender<String>,
) -> Result<()> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mkdir = ssh.background_shell(&remote_source_dir_prep(&source));
    let mkdir_status =
        run_command_streaming(mkdir, log_tx).context("creating remote source dir")?;
    if !mkdir_status.success() {
        anyhow::bail!("remote mkdir exited with {mkdir_status}");
    }
    let mut source_arg: std::ffi::OsString = source.clone().into_os_string();
    source_arg.push("/");
    let dest = format!("{}:.cache/croft/source/", ssh.host);
    let rsync = source_sync_rsync_command(lane, &ssh.socket_path, &source_arg, &dest);
    let status = run_command_streaming(rsync, log_tx).context("running rsync to remote")?;
    if !status.success() {
        anyhow::bail!("rsync exited with {status}");
    }
    Ok(())
}

/// Run the update check + (re)install on a detached thread so the caller can
/// hand the terminal to the already-installed remote croft immediately. Every
/// line of progress goes to `~/.cache/croft/install.log` via the streaming
/// installer's tee; nothing here may touch stdout/stderr, which belong to the
/// remote session once the caller attaches.
pub(super) fn spawn_background_install(ssh: &SshControl) {
    let adopted = AdoptedMaster {
        host: ssh.host.clone(),
        socket_dir: ssh.socket_dir.clone(),
        socket_path: ssh.socket_path.clone(),
    };
    std::thread::spawn(move || {
        // Both receivers are dropped on purpose: every send in the streaming
        // installer is best-effort, and the log tee keeps writing the file
        // even with a disconnected downstream.
        let (log_tx, _) = std::sync::mpsc::channel();
        let (can_launch_tx, _) = std::sync::mpsc::channel();
        // Unreachable: this thread only runs when a croft is already on the
        // remote, and the installer auto-declines the slow fallback in that
        // case (see the `present` gate in `install_only_streaming_over`).
        let _ = install_only_streaming(adopted, log_tx, can_launch_tx, &mut |_| false);
    });
}
