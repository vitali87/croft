use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::hash::Hasher;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

mod clipboard;
mod control;
mod forward;
mod hosts;
mod launch_command;
mod offer;
mod pull;
mod relay;
mod session;
pub(crate) use clipboard::MAX_COPY_PAYLOAD_BYTES;
use clipboard::*;
pub use control::AdoptedMaster;
pub use control::ssh_control_dir;
use control::*;
use forward::*;
pub use hosts::RemoteTarget;
pub use hosts::SshConfigState;
pub use hosts::discover_ssh_targets;
pub use hosts::primary_ssh_config_path;
pub use hosts::ssh_config_state;
use hosts::*;
pub(crate) use launch_command::client_process_nonce;
use launch_command::*;
pub use offer::CwdReport;
#[cfg(test)]
pub use offer::REFUSED_HOST_TTL;
pub use offer::RefusedHosts;
pub use offer::forget_refused_host;
pub use offer::load_refused_hosts;
pub use offer::offer_allowed;
pub use offer::refused_hosts_path;
pub use offer::remember_refused_host;
pub use offer::remote_workspace_path;
pub use offer::ssh_pane_samples;
pub use offer::ssh_reroot_decision;
#[cfg(test)]
use offer::*;
use pull::*;
use relay::*;
pub use session::DROP_TO_LOCAL_EXIT_CODE;
pub use session::RemoteOutcome;
pub use session::launch_croft;
pub use session::launch_croft_with;
pub use session::launch_only;
pub use session::running_over_ssh;
#[cfg(test)]
use session::*;

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
fn install_only_streaming_over(
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

fn install_log_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("croft").join("install.log")
}

fn run_command_streaming(
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
fn install_remote_croft_streaming(
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

/// Why a local static build for `triple` can't run, or `None` when it can.
/// Shared by the remote installer and dev containers (#617).
pub(crate) fn cross_build_unavailable(triple: &str) -> Option<String> {
    if let Some(reason) = cross_compile_unavailable_reason() {
        return Some(reason);
    }
    if !rust_target_installed(triple) {
        return Some(format!(
            "rustup target `{triple}` missing (run `rustup target add {triple}` once to enable the fast path)"
        ));
    }
    None
}

/// Cross-build a static croft for `triple` from this checkout and return the
/// binary's path.
pub(crate) fn cross_build_static(
    triple: &str,
    log_tx: &std::sync::mpsc::Sender<String>,
) -> Result<PathBuf> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // This build runs *concurrently* with the live remote session, whose
    // keystrokes are relayed by this same local machine. A default `-j N`
    // rustc build pins every core and starves the ssh relay + terminal
    // render, dropping the user's input. Run it niced and capped to half
    // the cores so the interactive session always preempts the compile -
    // the build is background, input latency is not. `nice` exists on both
    // macOS and Linux, so this stays identical on both launch targets.
    let jobs = std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(1)
        .to_string();
    sync_workspace_lock(&source, |msg| {
        let _ = log_tx.send(msg);
    });
    // Belt and braces with the startup raise: the zig linker dies with
    // ProcessFdQuotaExceeded under macOS's default 256-fd soft limit, and
    // the resulting silent fallback compiles on the remote box instead.
    let fd_limit = raise_fd_limit();
    let _ = log_tx.send(format!(
        "Cross-compiling croft locally for {triple} (niced, {jobs} jobs, fd limit {fd_limit})"
    ));
    let zigbuild = cargo_zigbuild_command(&source, triple, &jobs);
    let status = run_command_streaming(zigbuild, log_tx).context("running cargo zigbuild")?;
    if !status.success() {
        anyhow::bail!("cargo zigbuild exited with {status}");
    }

    let binary = source
        .join("target")
        .join(triple)
        .join("remote-fast")
        .join("croft");
    if !binary.exists() {
        anyhow::bail!(
            "cargo zigbuild reported success but {} is missing",
            binary.display()
        );
    }
    Ok(binary)
}

/// `Ok(None)` = binary shipped via the fast path; `Ok(Some(reason))` = the
/// fast path is unavailable (the reason feeds the fallback confirmation);
/// `Err` = the fast path was attempted and broke.
fn try_local_cross_install_streaming(
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
fn sync_local_source_to_remote_streaming(
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
fn spawn_background_install(ssh: &SshControl) {
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

pub struct SshControl {
    host: String,
    socket_dir: PathBuf,
    socket_path: PathBuf,
}

#[cfg(test)]
thread_local! {
    /// A stand-in for `ssh` in this test thread, so the commands built here
    /// can run against a local shim.
    static TEST_SSH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// The remote's config-file hashes, or `None` when they can't be read.
fn read_remote_hashes(mut command: Command) -> Option<std::collections::BTreeMap<String, String>> {
    // Waits out "Text file busy": ssh just installed or updated, or a
    // test's stand-in ssh written a moment ago while another test forked.
    let out = crate::review_ops::spawn_retrying_busy(&mut command)
        .and_then(std::process::Child::wait_with_output)
        .ok()?;
    if !out.status.success() {
        return None;
    }
    crate::config_sync::parse_remote_listing(&String::from_utf8_lossy(&out.stdout))
}

fn install_remote_croft(ssh: &SshControl, source_stamp: &str) -> Result<()> {
    // Fast path: cross-compile a static musl binary on the local Mac and
    // rsync it directly into the remote's ~/.cargo/bin. Skips the
    // crates.io index update, the dependency walk, and the release-mode
    // codegen+link of the croft crate on the remote. The legacy
    // source-rsync + `cargo install` fallback still exists for when the
    // tooling isn't present (zig + cargo-zigbuild + the matching rust
    // target), the remote arch can't be detected, or the build fails —
    // but it never engages silently: the user confirms it on the tty
    // first, because quitting to run `croft setup-cross` once is almost
    // always the better deal.
    //
    // Before either: the release binary for this exact version (#261), when
    // this croft IS that release.
    let mut ship = |binary: &Path| -> Result<()> {
        let ssh_e = format!(
            "ssh -S {} -o ControlMaster=no",
            shell_quote_for_e_arg(&ssh.socket_path),
        );
        let dest = format!("{}:.cargo/bin/croft.new", ssh.host);
        let status = Command::new("rsync")
            .args(["-az", "--checksum", "-e"])
            .arg(&ssh_e)
            .arg(binary)
            .arg(&dest)
            .status()
            .context("rsyncing the prebuilt croft to the remote")?;
        anyhow::ensure!(status.success(), "rsync exited with {status}");
        Ok(())
    };
    match try_prebuilt_install(ssh, source_stamp, &mut ship, &mut |m| println!("{m}")) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(e) => eprintln!("Prebuilt install failed ({e:#}); building instead"),
    }
    let reason = match try_local_cross_install(ssh, source_stamp) {
        Ok(None) => return Ok(()),
        Ok(Some(reason)) => reason,
        Err(e) => {
            eprintln!("Local cross-build failed ({e:#})");
            format!("local cross-build failed: {e:#}")
        }
    };
    if !confirm_remote_compile_on_tty(&ssh.host, &reason) {
        anyhow::bail!(
            "cross-build unavailable ({reason}); remote compile declined — run `croft setup-cross`, then `croft remote {}` again for the fast install",
            ssh.host
        );
    }
    sync_local_source_to_remote(ssh)?;
    let status = ssh
        .command()
        .arg(&ssh.host)
        .arg(remote_install_command(source_stamp))
        .status()
        .context("installing croft on remote")?;
    if !status.success() {
        anyhow::bail!("remote croft install failed with {status}");
    }
    Ok(())
}

/// Raise this process's soft RLIMIT_NOFILE as far as the hard limit
/// allows (capped at 1M). macOS launchd hands GUI/login processes a soft
/// limit of 256; croft's cross-link opens ~250 rlibs at once, so the zig
/// linker spawned under that default dies with ProcessFdQuotaExceeded and
/// the installer silently falls back to compiling on the remote box —
/// the 2026-06-12 "horrendous latency on various" root cause. Children
/// inherit the raised limit. Returns the resulting soft limit.
pub(crate) fn raise_fd_limit() -> u64 {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return 0;
    }
    let ceiling: libc::rlim_t = 1 << 20;
    let target = if lim.rlim_max == libc::RLIM_INFINITY {
        ceiling
    } else {
        lim.rlim_max.min(ceiling)
    };
    // macOS rejects soft limits above kern.maxfilesperproc even when the
    // hard limit reads as unlimited, so step down through sane sizes
    // until one sticks. Never lower an already-high limit.
    for candidate in [target, 65536, 10240] {
        if candidate <= lim.rlim_cur {
            break;
        }
        let raised = libc::rlimit {
            rlim_cur: candidate,
            rlim_max: lim.rlim_max,
        };
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } == 0 {
            return candidate;
        }
    }
    lim.rlim_cur
}

/// Reason the local cross-build fast path can't run, or `None` if it can. Each
/// tool is located via [`cross_tool_program`] (PATH plus standard toolchain
/// dirs) and then probed, so a GUI launch with a stripped launchd PATH still
/// finds `~/.cargo/bin` and Homebrew. Returning a reason (instead of a bare
/// bool) lets the caller log *why* it fell back to the slow from-scratch build.
fn cross_compile_unavailable_reason() -> Option<String> {
    // `cargo zigbuild --version` is rejected by cargo-zigbuild >=0.22 (the
    // `zigbuild` subcommand has no `--version`, exit 2); probe each binary
    // directly with the arg it accepts.
    for (tool, version_arg, install_hint) in [
        ("cargo", "--version", "install Rust via https://rustup.rs"),
        ("cargo-zigbuild", "--version", "run `croft setup-cross`"),
        ("zig", "version", "run `croft setup-cross`"),
    ] {
        if cross_tool_program(tool).is_none() {
            return Some(format!(
                "`{tool}` not found on PATH or standard toolchain locations ({install_hint})"
            ));
        }
        if !probe_cross_tool(tool, version_arg) {
            return Some(format!("`{tool} {version_arg}` failed ({install_hint})"));
        }
    }
    None
}

fn probe_cross_tool(tool: &str, version_arg: &str) -> bool {
    cross_tool_command(tool)
        .arg(version_arg)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Directories searched for the cross-build toolchain. A Croft.app / Ghostty
/// launch on macOS inherits launchd's stripped PATH, so `~/.cargo/bin` and
/// Homebrew are absent even though the same tools resolve in an interactive
/// shell. PATH entries come first (an explicit override wins), then the
/// standard rustup/Homebrew locations.
fn cross_tool_search_dirs_from(path: Option<&OsStr>, home: Option<&OsStr>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(path) = path {
        for dir in std::env::split_paths(path) {
            push_unique_path(&mut dirs, dir);
        }
    }
    if let Some(home) = home {
        let home = PathBuf::from(home);
        push_unique_path(&mut dirs, home.join(".cargo").join("bin"));
        push_unique_path(&mut dirs, home.join(".local").join("bin"));
    }
    push_unique_path(&mut dirs, PathBuf::from("/opt/homebrew/bin"));
    push_unique_path(&mut dirs, PathBuf::from("/usr/local/bin"));
    dirs
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn cross_tool_search_dirs() -> Vec<PathBuf> {
    let path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME");
    cross_tool_search_dirs_from(path.as_deref(), home.as_deref())
}

/// A PATH that includes the standard toolchain dirs, handed to every spawned
/// cross-build tool so child processes (e.g. `cargo` invoking `cargo-zigbuild`
/// invoking `zig`) resolve each other even under a stripped launchd PATH.
fn cross_tool_path() -> OsString {
    let path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME");
    cross_tool_path_from(path.as_deref(), home.as_deref())
}

fn cross_tool_path_from(path: Option<&OsStr>, home: Option<&OsStr>) -> OsString {
    std::env::join_paths(cross_tool_search_dirs_from(path, home))
        .unwrap_or_else(|_| path.map(OsString::from).unwrap_or_default())
}

fn cross_tool_program(tool: &str) -> Option<PathBuf> {
    find_executable_in_dirs(tool, &cross_tool_search_dirs())
}

fn find_executable_in_dirs(tool: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(tool))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// A `Command` for a cross-build tool, resolved to its absolute path and run
/// with the augmented PATH so its own child processes resolve too.
fn cross_tool_command(tool: &str) -> Command {
    let program = cross_tool_program(tool).unwrap_or_else(|| PathBuf::from(tool));
    let mut command = Command::new(program);
    command.env("PATH", cross_tool_path());
    command
}

/// The checkout, when this binary still has one. A croft installed from a
/// release (or built in a Nix sandbox) has a `CARGO_MANIFEST_DIR` that does
/// not exist at runtime, and handing a missing directory to `current_dir`
/// fails the spawn with ENOENT before the tool ever runs - which would turn
/// "cargo is not installed" into the diagnosis on a machine where cargo works
/// fine. No checkout also means no cross-build, so the fast path is
/// unavailable either way and the existing reason reporting covers it.
fn checkout_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.is_dir().then_some(dir)
}

/// A cross-build tool run FROM THE CHECKOUT, for the calls whose ANSWER
/// depends on which toolchain is resolved. `rust-toolchain.toml` lives there
/// and rustup reads the channel from its working directory, so a target query
/// run anywhere else answers for the default toolchain while `cargo zigbuild`
/// (which sets `current_dir(source)`) answers for the pinned one. A target
/// present on only one of them makes the guard report success immediately
/// before the build dies with E0463 - four days of silent fallback to a
/// remote `cargo install` after the 1.95.0 to 1.97.1 bump.
///
/// Deliberately NOT used for the mere "is this binary present" probes: those
/// have the same answer in any directory, and running them in the checkout
/// would make an availability check trigger a multi-hundred-MB rustup
/// auto-install of the pinned channel.
fn cross_tool_command_in_checkout(tool: &str) -> Command {
    let mut command = cross_tool_command(tool);
    if let Some(dir) = checkout_dir() {
        command.current_dir(dir);
    }
    command
}

/// The niced, job-capped `cargo zigbuild` invocation, with `cargo` resolved to
/// an absolute path and the augmented PATH exported so it finds cargo-zigbuild
/// and zig.
fn cargo_zigbuild_command(source: &Path, triple: &str, jobs: &str) -> Command {
    let cargo = cross_tool_program("cargo").unwrap_or_else(|| PathBuf::from("cargo"));
    let mut zigbuild = Command::new("nice");
    zigbuild
        .args(["-n", "19"])
        .arg(cargo)
        .args([
            "zigbuild",
            "--profile",
            "remote-fast",
            "--locked",
            "--jobs",
            jobs,
            "--bin",
            "croft",
            "--target",
            triple,
        ])
        .env("PATH", cross_tool_path())
        .current_dir(source);
    zigbuild
}

/// Re-pin the croft workspace member in `Cargo.lock` to whatever `Cargo.toml`
/// now declares, immediately before a `--locked` cross-build.
///
/// Every behavioural change bumps the patch version in `Cargo.toml`, but
/// `cargo install --path .` never rewrites the on-disk lockfile - so the lock
/// drifts exactly one patch behind. `cargo zigbuild --locked` then refuses to
/// build, and the installer silently falls back to a minutes-long
/// from-scratch `cargo install` *on the remote host* (the thing the fast path
/// exists to avoid). `cargo update -p croft-software --offline` rewrites only croft's
/// own version line - it touches no dependency, needs no network, and so keeps
/// `--locked`'s real guarantee (a reproducible dependency graph) fully intact.
///
/// Best-effort: if the sync itself fails we log and still attempt the locked
/// build, preserving the old fall-back behaviour rather than blocking install.
fn sync_workspace_lock(source: &Path, log: impl Fn(String)) {
    match cross_tool_command("cargo")
        .args(["update", "-p", "croft-software", "--offline"])
        .current_dir(source)
        .output()
    {
        Ok(out) if out.status.success() => {
            log(
                "Synced Cargo.lock to the bumped croft version before the locked cross-build"
                    .to_string(),
            );
        }
        Ok(out) => log(format!(
            "Cargo.lock sync skipped (cargo update exited {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => log(format!("Cargo.lock sync skipped ({e})")),
    }
}

/// Push the user's syncable config to the remote (#262), so keybindings,
/// snippets, triggers and matchers follow the binary that already follows
/// them.
///
/// Called from `run_croft_session`'s prologue rather than off the back of
/// the install: when croft is already present the install runs on a
/// detached thread and the session opens without waiting for it, so an
/// install-completion hook would skip the common case entirely.
///
/// Best-effort by construction. A remote whose config cannot be written is
/// a remote that still opens with defaults, which is exactly today's
/// behaviour — failing the connect over it would be a regression. Every
/// outcome is reported so a silent no-op is distinguishable from a silent
/// success.
fn push_config_files(ssh: &SshControl, resolution: &SyncResolution, log: &mut dyn FnMut(String)) {
    // User layers only: a workspace must not decide what leaves the laptop.
    let prefs = crate::config_layers::load_merged(None).prefs;
    if crate::config_sync::host_excluded(&ssh.host, &prefs.config_sync_excluded_hosts) {
        log(format!(
            "Config sync: off for {} (config_sync_excluded_hosts)",
            ssh.host
        ));
        return;
    }
    let (files, skipped) = crate::config_sync::apply_exclusions(
        crate::config_sync::local_files(),
        &prefs.config_sync_excluded_files,
    );
    if !skipped.is_empty() {
        log(format!(
            "Config sync: not pushing {} (config_sync_excluded_files)",
            skipped.join(", ")
        ));
    }
    if files.is_empty() {
        return;
    }
    // `mkdir -p` first: rsync will not create the remote parent, and a
    // fresh box has no ~/.config/croft until croft has run there once.
    // `background_command` for its `-n`: this runs before the session
    // takes the terminal, and a command that inherits stdin would race the
    // shell for the user's keystrokes.
    let mut mk = ssh.background_shell("mkdir -p ~/.config/croft");
    if !matches!(mk.status(), Ok(st) if st.success()) {
        log(String::from(
            "Config sync: could not create ~/.config/croft on the remote; skipping",
        ));
        return;
    }

    // What is there now, so a copy edited on the remote is never replaced
    // (#262). Unreadable means unknown, and unknown is not permission.
    let Some(remote) = read_remote_hashes(ssh_socket_read(
        &ssh.socket_path,
        &ssh.host,
        &crate::config_sync::remote_hash_script(),
    )) else {
        log(String::from(
            "Config sync: could not read the remote's copies; pushing nothing rather than risk overwriting them",
        ));
        return;
    };
    let state_path = crate::config_sync::SyncState::path();
    let mut state = crate::config_sync::SyncState::load(&state_path);

    let bulk = crate::remote_bulk::establish(&ssh.host, &ssh.socket_path, |_| {});
    let mut pushed = Vec::new();
    let mut failed = Vec::new();
    let mut kept = Vec::new();
    let mut conflicts = Vec::new();
    for (syncable, local) in &files {
        let Ok(bytes) = std::fs::read(local) else {
            failed.push(syncable.name);
            continue;
        };
        let local_hash = crate::config_sync::content_hash(&bytes);
        let there = remote.get(syncable.name).map(String::as_str);
        let chosen = |list: &[String]| list.iter().any(|n| n == syncable.name);
        let plan = if chosen(&resolution.take_local) {
            crate::config_sync::Plan::Push
        } else if chosen(&resolution.keep_remote) && there.is_some_and(|r| r != local_hash) {
            let entry = state.file_mut(&ssh.host, syncable.name);
            entry.kept = there.map(|r| (r.to_string(), local_hash.clone()));
            crate::config_sync::Plan::Kept
        } else {
            crate::config_sync::plan(&local_hash, there, state.file(&ssh.host, syncable.name))
        };
        match plan {
            crate::config_sync::Plan::Push => {
                let dest = crate::config_sync::remote_dest(&ssh.host, syncable.name);
                let mut rsync = ship_file_rsync_command(&bulk.lane, &ssh.socket_path, local, &dest);
                match rsync.status() {
                    Ok(st) if st.success() => {
                        let entry = state.file_mut(&ssh.host, syncable.name);
                        entry.pushed = Some(local_hash);
                        entry.kept = None;
                        pushed.push(syncable);
                    }
                    _ => failed.push(syncable.name),
                }
            }
            crate::config_sync::Plan::UpToDate => {
                let entry = state.file_mut(&ssh.host, syncable.name);
                entry.pushed = Some(local_hash);
                entry.kept = None;
            }
            crate::config_sync::Plan::Kept => kept.push(syncable.name),
            crate::config_sync::Plan::Conflict => conflicts.push(syncable.name),
        }
    }
    if let Err(e) = state.save(&state_path) {
        log(format!(
            "Config sync: could not record what was pushed ({e})"
        ));
    }
    if !kept.is_empty() {
        log(format!(
            "Config sync: keeping the remote's own {} (as you chose)",
            kept.join(", ")
        ));
    }
    for name in &conflicts {
        log(format!(
            "Config sync: {name} was changed on {host} since croft last pushed it, so it was left alone. \
             `croft sync-config {host} --diff {name}` compares the two; `--take-local {name}` pushes yours, \
             `--keep-remote {name}` keeps theirs",
            host = ssh.host
        ));
    }

    if !pushed.is_empty() {
        // Name the files rather than counting them: "4 files" tells a user
        // nothing about whether the one they just edited went.
        let names: Vec<&str> = pushed.iter().map(|s| s.name).collect();
        log(format!("Config sync: pushed {}", names.join(", ")));
        // A croft already running on the remote applies them within its
        // config watch's interval (`ConfigWatch`, #262); one started later
        // reads them at launch.
        log(format!(
            "Config sync: {} applies on the remote within a few seconds",
            names.join(", ")
        ));
    }
    if !failed.is_empty() {
        log(format!(
            "Config sync: could not push {} (the remote keeps its own copy)",
            failed.join(", ")
        ));
    }
}

/// How the user settled config files that differ on the remote (#262):
/// push these anyway, or keep the remote's copy of these.
#[derive(Debug, Default, Clone)]
pub struct SyncResolution {
    pub take_local: Vec<String>,
    pub keep_remote: Vec<String>,
}

/// `croft sync-config <host>` (#262): push the syncable config now, the
/// same way a connect does, or with `diff` set show how the remote's copy
/// of that file differs from the local one.
pub fn sync_config_now(host: &str, resolution: &SyncResolution, diff: Option<&str>) -> Result<()> {
    for name in resolution
        .take_local
        .iter()
        .chain(&resolution.keep_remote)
        .map(String::as_str)
        .chain(diff)
    {
        if !crate::config_sync::SYNCABLE.iter().any(|s| s.name == name) {
            let names: Vec<&str> = crate::config_sync::SYNCABLE
                .iter()
                .map(|s| s.name)
                .collect();
            anyhow::bail!("{name} is not a synced file ({})", names.join(", "));
        }
    }
    let ssh = SshControl::start(host)?;
    match diff {
        Some(name) => print_config_diff(&ssh, name),
        None => {
            push_config_files(&ssh, resolution, &mut |msg| println!("{msg}"));
            Ok(())
        }
    }
}

/// Re-pushes a config file edited on this machine while a remote session
/// runs (#262), so a keybinding changed mid-session reaches the remote
/// croft, which applies it on arrival, instead of waiting for the next
/// connect. A thread polls the files with [`crate::config_sync::ConfigWatch`]
/// and ships each change over the bulk lane; it logs to
/// `~/.cache/croft/config-sync.log`, never to the terminal, which belongs to
/// the remote croft. Dropping it stops the thread.
struct ConfigRepush {
    /// Host and control socket to push through; swapped on a reconnect.
    target: std::sync::Arc<std::sync::Mutex<(String, PathBuf)>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Joined on drop, so no push starts after the session has ended.
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ConfigRepush {
    /// Start re-pushing changes relative to `watch`, whose baseline the
    /// caller took before the connect-time push.
    fn start(ssh: &SshControl, watch: crate::config_sync::ConfigWatch) -> Self {
        let target = std::sync::Arc::new(std::sync::Mutex::new((
            ssh.host.clone(),
            ssh.socket_path.clone(),
        )));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (t, s) = (target.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            let dir = crate::paths::config_dir();
            let mut state = RepushState::new(watch);
            let mut exclusions = SyncExclusions::load();
            let mut log = config_sync_log();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                // Checked after the sleep too, so a session that ended
                // during it starts no push.
                if s.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let (host, socket) = match t.lock() {
                    Ok(g) => g.clone(),
                    Err(_) => return,
                };
                repush_tick(
                    &mut state,
                    &dir,
                    std::time::Instant::now(),
                    &mut || match exclusions.current() {
                        None => RepushGate::Unknown,
                        Some((hosts, _)) if crate::config_sync::host_excluded(&host, &hosts) => {
                            RepushGate::HostExcluded
                        }
                        Some((_, files)) => RepushGate::Push(files),
                    },
                    &mut |files| push_over_bulk_lane(&host, &socket, files),
                    &mut log,
                );
            }
        });
        ConfigRepush {
            target,
            stop,
            worker: Some(worker),
        }
    }

    /// Push through the connection a reconnect just opened.
    fn retarget(&self, ssh: &SshControl) {
        if let Ok(mut g) = self.target.lock() {
            *g = (ssh.host.clone(), ssh.socket_path.clone());
        }
    }
}

/// Print a unified diff of the remote's copy of `name` against the local
/// one, remote first, so `+` lines are what a push would bring.
fn print_config_diff(ssh: &SshControl, name: &str) -> Result<()> {
    let local = crate::paths::config_dir().join(name);
    let out = ssh_socket_read(
        &ssh.socket_path,
        &ssh.host,
        &format!("cat ~/.config/croft/{name} 2>/dev/null"),
    )
    .output()?;
    let tmp = std::env::temp_dir().join(format!("croft-sync-{}-{name}", std::process::id()));
    std::fs::write(&tmp, &out.stdout)?;
    let local_arg = if local.is_file() {
        local
    } else {
        PathBuf::from("/dev/null")
    };
    let status = Command::new("diff")
        .args(["-u", "--label"])
        .arg(format!("{}:{name}", ssh.host))
        .arg("--label")
        .arg(format!("local:{name}"))
        .arg(&tmp)
        .arg(&local_arg)
        .status();
    let _ = std::fs::remove_file(&tmp);
    let status = status?;
    if status.code() == Some(0) {
        println!("{name} is the same on {} and here", ssh.host);
    }
    Ok(())
}

impl Drop for ConfigRepush {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        // At most one sleep plus a push in flight: the teardown waits for a
        // transfer already started rather than cutting it off.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The user's config-sync exclusions (#603) as a live re-push reads them.
///
/// Read again whenever a file changes, so an edited exclusion applies
/// without a reconnect. Only a clean read counts: one that skipped a layer
/// (a config file half-written or invalid at that moment) would report the
/// default, empty lists, and pushing on those would send a file to a host
/// the user had excluded. So a bad read keeps the last clean one, and until
/// there has been a clean read at all the exclusions are unknown and
/// nothing is pushed.
struct SyncExclusions {
    /// `(config_sync_excluded_hosts, config_sync_excluded_files)` from the
    /// last clean read; `None` before the first.
    known: Option<(Vec<String>, Vec<String>)>,
}

impl SyncExclusions {
    /// The exclusions as of the session start, if they read cleanly.
    fn load() -> Self {
        let mut ex = SyncExclusions { known: None };
        ex.update(crate::config_layers::load_merged(None));
        ex
    }

    /// Re-read the exclusions: the latest clean read, or `None` while no
    /// read has been clean. User layers only, as at connect: a workspace
    /// must not decide what leaves the laptop.
    fn current(&mut self) -> Option<(Vec<String>, Vec<String>)> {
        self.update(crate::config_layers::load_merged(None));
        self.known.clone()
    }

    fn update(&mut self, merged: crate::config_layers::MergedConfig) {
        if merged.warnings.is_empty() {
            self.known = Some((
                merged.prefs.config_sync_excluded_hosts,
                merged.prefs.config_sync_excluded_files,
            ));
        }
    }
}

/// What the exclusions say about a pending re-push.
enum RepushGate {
    /// Push, leaving out these files (`config_sync_excluded_files`).
    Push(Vec<String>),
    /// The host is in `config_sync_excluded_hosts`: push nothing, now or later.
    HostExcluded,
    /// The exclusions have not read cleanly: hold the files and try again.
    Unknown,
}

/// How long a failed re-push waits before it is tried again: long enough
/// not to hammer a connection that is down, short enough that an edit lands
/// soon after a reconnect.
const REPUSH_RETRY: std::time::Duration = std::time::Duration::from_secs(10);

/// What a live re-push carries between polls.
struct RepushState {
    watch: crate::config_sync::ConfigWatch,
    /// Changed files not yet pushed: a failed push stays here and is
    /// retried, so an edit made while the connection was down still lands
    /// after the reconnect.
    pending: std::collections::BTreeSet<PathBuf>,
    retry_at: Option<std::time::Instant>,
}

impl RepushState {
    fn new(watch: crate::config_sync::ConfigWatch) -> Self {
        RepushState {
            watch,
            pending: std::collections::BTreeSet::new(),
            retry_at: None,
        }
    }
}

/// Config files to push, each with the local path it is read from.
type Pushes = [(crate::config_sync::Syncable, PathBuf)];

/// What a re-push did not send: files that failed to transfer (tried
/// again), and files held because they were edited on the remote since
/// croft last pushed them (#262), which wait for the user to settle.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct PushResult {
    failed: Vec<&'static str>,
    held: Vec<&'static str>,
}

/// One poll of a live re-push: push the syncable files that changed and
/// exist, and log what went and what did not. `push` returns the names it
/// could not ship.
fn repush_tick(
    state: &mut RepushState,
    dir: &Path,
    now: std::time::Instant,
    exclusions: &mut dyn FnMut() -> RepushGate,
    push: &mut dyn FnMut(&Pushes) -> PushResult,
    log: &mut dyn FnMut(String),
) {
    let changed = state.watch.poll(now);
    let retry_due = state.retry_at.is_some_and(|t| now >= t);
    if changed.is_empty() && !retry_due {
        return;
    }
    state.pending.extend(changed);
    // A pending file deleted since, or no longer syncable, drops out.
    let pending: Vec<PathBuf> = state.pending.iter().cloned().collect();
    let files = crate::config_sync::repush_targets(&pending, dir);
    state.pending = files.iter().map(|(_, p)| p.clone()).collect();
    state.retry_at = None;
    if files.is_empty() {
        return;
    }
    // The same rules as the push at connect (#603), read when a file
    // changes so an edited exclusion applies without a reconnect.
    let excluded = match exclusions() {
        RepushGate::Push(excluded) => excluded,
        RepushGate::HostExcluded => {
            state.pending.clear();
            return;
        }
        RepushGate::Unknown => {
            // Nothing leaves until the exclusions can be read; the files
            // stay pending and are tried again.
            state.retry_at = Some(now + REPUSH_RETRY);
            log(String::from(
                "Config sync: holding changes until the config-sync exclusions read cleanly",
            ));
            return;
        }
    };
    let (files, _) = crate::config_sync::apply_exclusions(files, &excluded);
    if files.is_empty() {
        state.pending.clear();
        return;
    }
    let PushResult { failed, held } = push(&files);
    state.pending = files
        .iter()
        .filter(|(s, _)| failed.contains(&s.name))
        .map(|(_, p)| p.clone())
        .collect();
    if !state.pending.is_empty() {
        state.retry_at = Some(now + REPUSH_RETRY);
    }
    let pushed: Vec<&str> = files
        .iter()
        .map(|(s, _)| s.name)
        .filter(|n| !failed.contains(n) && !held.contains(n))
        .collect();
    if !pushed.is_empty() {
        log(format!("Config sync: re-pushed {}", pushed.join(", ")));
    }
    for name in &held {
        log(format!(
            "Config sync: left {name} alone: it was changed on the remote since croft last pushed it \
             (`croft sync-config <host> --diff {name}`)"
        ));
    }
    if !failed.is_empty() {
        log(format!(
            "Config sync: could not re-push {} (the remote keeps its copy)",
            failed.join(", ")
        ));
    }
}

/// Ship `files` over a bulk lane opened for this batch; the names that
/// failed.
fn push_over_bulk_lane(
    host: &str,
    socket: &Path,
    files: &[(crate::config_sync::Syncable, PathBuf)],
) -> PushResult {
    // The connect-time push creates this only when it had files to send, so
    // a host that had none has no ~/.config/croft for the first file made
    // mid-session to land in.
    let mut mk = ssh_socket_command(socket, true);
    mk.arg(host).arg("mkdir -p ~/.config/croft");
    let all_failed = || PushResult {
        failed: files.iter().map(|(s, _)| s.name).collect(),
        held: Vec::new(),
    };
    if !matches!(mk.status(), Ok(st) if st.success()) {
        return all_failed();
    }
    // The connect-time rule (#262): never over a copy edited there since.
    let Some(remote) = read_remote_hashes(ssh_socket_read(
        socket,
        host,
        &crate::config_sync::remote_hash_script(),
    )) else {
        return all_failed();
    };
    let state_path = crate::config_sync::SyncState::path();
    let mut state = crate::config_sync::SyncState::load(&state_path);
    let bulk = crate::remote_bulk::establish(host, socket, |_| {});
    let mut result = PushResult::default();
    for (syncable, local) in files {
        let Ok(bytes) = std::fs::read(local) else {
            result.failed.push(syncable.name);
            continue;
        };
        let hash = crate::config_sync::content_hash(&bytes);
        let there = remote.get(syncable.name).map(String::as_str);
        match crate::config_sync::plan(&hash, there, state.file(host, syncable.name)) {
            crate::config_sync::Plan::Push => {
                let dest = crate::config_sync::remote_dest(host, syncable.name);
                let ok = matches!(
                    ship_file_rsync_command(&bulk.lane, socket, local, &dest).status(),
                    Ok(st) if st.success()
                );
                if ok {
                    let entry = state.file_mut(host, syncable.name);
                    entry.pushed = Some(hash);
                    entry.kept = None;
                } else {
                    result.failed.push(syncable.name);
                }
            }
            crate::config_sync::Plan::UpToDate => {
                let entry = state.file_mut(host, syncable.name);
                entry.pushed = Some(hash);
                entry.kept = None;
            }
            crate::config_sync::Plan::Kept | crate::config_sync::Plan::Conflict => {
                result.held.push(syncable.name)
            }
        }
    }
    let _ = state.save(&state_path);
    result
}

/// Appends timestamped lines to `~/.cache/croft/config-sync.log`.
fn config_sync_log() -> impl FnMut(String) {
    let path = install_log_path().with_file_name("config-sync.log");
    move |line: String| {
        use std::io::Write as _;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(f, "{secs} {line}");
        }
    }
}

/// Build the rsync that ships one local file to `dest`, routed and paced by
/// the bulk lane so it never queues ahead of the live session's keystrokes.
///
/// Nothing here is binary-specific: `--checksum` makes rsync compare content
/// rather than mtime, so a caller shipping config files (#262) gets
/// push-only-if-different for free and needs no stamp of its own.
fn ship_file_rsync_command(
    lane: &crate::remote_bulk::BulkLane,
    interactive_socket: &Path,
    local: &Path,
    dest: &str,
) -> Command {
    let mut rsync = Command::new("rsync");
    rsync.args(["-az", "--checksum"]);
    rsync.args(lane.rsync_throttle_args());
    rsync.arg("-e").arg(lane.rsync_ssh_arg(interactive_socket));
    rsync.arg(local).arg(dest);
    rsync
}

/// Prepare the remote source dir: clear every stale top-level entry, keeping
/// only `target/` (the incremental build cache the install depends on) and
/// the [`SOURCE_STAMP_INPUTS`] actually present in the local checkout.
///
/// Neither transfer can be trusted to clean up after a previous croft: the
/// allow-list's `--exclude=*` PROTECTS every unlisted path from rsync's
/// `--delete` (which is how the old deny-list's shipped `target.noindex`
/// trees, 23 to 27 GB per host, would have outlived the allow-list fix), and
/// the tar fallback never deletes at all, so an input since removed from the
/// checkout or a stale `.cargo/config.toml` - which would silently
/// reconfigure every later remote build - persists forever. Inputs the local
/// checkout still contains are deliberately KEPT: rsync deletes staleness
/// inside them itself, wiping them would re-ship the whole tree every sync,
/// and two croft processes updating the same host concurrently must never
/// yank the source out from under the other's running `cargo install`
/// (identical files are a no-op to `rsync -a`, so a concurrent sync of the
/// same checkout touches nothing).
fn remote_source_dir_prep(source: &Path) -> String {
    let keeps: String = std::iter::once("target")
        .chain(
            SOURCE_STAMP_INPUTS
                .iter()
                .copied()
                .filter(|name| source.join(name).exists()),
        )
        .map(|name| format!("! -name '{name}' "))
        .collect();
    // The `/.` dereferences a symlinked source dir (a relocated cache on a
    // small-root VPS): find does not follow a symlink given as its start
    // point, and would otherwise clean nothing while exiting 0.
    format!(
        "mkdir -p \"$HOME/.cache/croft/source\" && \
         find \"$HOME/.cache/croft/source/.\" -mindepth 1 -maxdepth 1 \
         {keeps}-exec rm -rf {{}} +"
    )
}

/// rsync filter rules that ship exactly [`SOURCE_STAMP_INPUTS`] and nothing
/// else.
///
/// An ALLOW-list, deliberately. The deny-list this replaces excluded `target`,
/// which never matched this repo's real build directory `target.noindex`, so
/// every fallback install pushed the whole 176 GB artifact tree to the remote
/// box at the bulk lane's couple of MB/s. A deny-list has to be updated every
/// time the tree grows a new directory; an allow-list derived from the very
/// list the stamp hashes cannot drift, and the two can never disagree about
/// what "the source" is.
fn source_sync_filter_args() -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    // First match wins in rsync, so the every-level denials go first: a root
    // allow-list alone would re-admit `assets/.DS_Store` and any nested
    // `target/`, which the stamp walk skips and would therefore ship without
    // ever being a reason to reinstall.
    for name in SOURCE_SKIP_NAMES {
        args.push(format!("--exclude={name}"));
    }
    for name in SOURCE_STAMP_INPUTS {
        // `/name` admits the entry itself; `/name/***` admits a directory's
        // whole subtree without needing a rule per level.
        args.push(format!("--include=/{name}"));
        args.push(format!("--include=/{name}/***"));
    }
    args.push(String::from("--exclude=*"));
    args
}

/// The build inputs that actually exist under `source`, as relative paths.
/// The tar fallback has no filter language worth the name, so it names the
/// members instead - the same allow-list, expressed the way tar accepts it.
fn source_sync_tar_members(source: &Path) -> Vec<&'static str> {
    SOURCE_STAMP_INPUTS
        .iter()
        .copied()
        .filter(|name| source.join(name).exists())
        .collect()
}

fn source_sync_rsync_command(
    lane: &crate::remote_bulk::BulkLane,
    interactive_socket: &Path,
    source_arg: &std::ffi::OsStr,
    dest: &str,
) -> Command {
    let mut rsync = Command::new("rsync");
    rsync.args(["-a", "-z", "--delete"]);
    rsync.args(source_sync_filter_args());
    rsync.args(lane.rsync_throttle_args());
    rsync.arg("-e").arg(lane.rsync_ssh_arg(interactive_socket));
    rsync.arg(source_arg).arg(dest);
    rsync
}

/// Set by `croft remote --build` (#261): skip the prebuilt release binary
/// and build from source, for a source tree ahead of the latest release.
pub(crate) static FORCE_SOURCE_BUILD: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The prebuilt fast path (#261): when this croft was installed from
/// crates.io, ship the release binary for its exact version and the remote's
/// target instead of building one. `Ok(true)` = installed; `Ok(false)` = not
/// applicable (said why); `Err` = attempted and broke. The caller falls back
/// to the cross-build either way. `ship` copies a local binary to the
/// remote's `~/.cargo/bin/croft.new` over whichever transport the caller has.
fn try_prebuilt_install(
    ssh: &SshControl,
    source_stamp: &str,
    ship: &mut dyn FnMut(&Path) -> Result<()>,
    log: &mut dyn FnMut(String),
) -> Result<bool> {
    if FORCE_SOURCE_BUILD.load(std::sync::atomic::Ordering::Relaxed) {
        log(String::from(
            "Prebuilt install skipped: --build asked for a source build",
        ));
        return Ok(false);
    }
    if !crate::remote_prebuilt::eligible(env!("CARGO_MANIFEST_DIR")) {
        log(String::from(
            "Prebuilt install skipped: this croft is a source build, so the remote gets the same source",
        ));
        return Ok(false);
    }
    let Some(triple) = remote_target_triple(ssh)? else {
        log(String::from(
            "Prebuilt install skipped: no release for the remote's architecture",
        ));
        return Ok(false);
    };
    let version = env!("CARGO_PKG_VERSION");
    let cache = crate::session_state::dirs_cache_croft().join("prebuilt");
    let binary = match crate::remote_prebuilt::prepare(
        version,
        triple,
        &cache,
        &crate::remote_prebuilt::http_get,
        &crate::remote_prebuilt::cosign_verify,
    ) {
        Ok((b, crate::remote_prebuilt::Signature::Verified)) => {
            log(format!(
                "Release signature verified: v{version}'s SHA256SUMS was signed by its release workflow"
            ));
            b
        }
        Ok((b, crate::remote_prebuilt::Signature::Unchecked)) => {
            log(format!(
                "Release signature NOT checked: cosign is not installed here, so v{version}'s SHA256SUMS rests on HTTPS to GitHub; install cosign to verify it"
            ));
            b
        }
        // Not every version has release artifacts; that is a plain skip,
        // not a failure to shout about on every connect.
        Err(e) if crate::remote_prebuilt::is_not_published(&e) => {
            log(format!(
                "Prebuilt install skipped: v{version} has no release binary for {triple}"
            ));
            return Ok(false);
        }
        Err(e) => return Err(e),
    };
    let size_mb = std::fs::metadata(&binary).map(|m| m.len()).unwrap_or(0) as f64 / 1e6;
    log(format!(
        "Installing prebuilt croft v{version} for {triple} ({size_mb:.0} MB) over SSH"
    ));
    let mkdir = ssh
        .command()
        .arg(&ssh.host)
        .arg("mkdir -p \"$HOME/.cargo/bin\" \"$HOME/.cache/croft\"")
        .status()
        .context("creating remote install dirs")?;
    anyhow::ensure!(mkdir.success(), "remote mkdir exited with {mkdir}");
    ship(&binary)?;
    let activate = ssh
        .command()
        .arg(&ssh.host)
        .arg(activate_command(source_stamp))
        .status()
        .context("activating the prebuilt croft on the remote")?;
    anyhow::ensure!(
        activate.success(),
        "remote activation exited with {activate}"
    );
    log(format!("Installed prebuilt croft v{version} on the remote"));
    Ok(true)
}

fn remote_target_triple(ssh: &SshControl) -> Result<Option<&'static str>> {
    let output = ssh
        .command()
        .arg(&ssh.host)
        .arg("uname -m")
        .output()
        .context("probing remote architecture")?;
    if !output.status.success() {
        return Ok(None);
    }
    let arch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(arch_to_musl_triple(&arch))
}

pub fn arch_to_musl_triple(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" | "amd64" => Some("x86_64-unknown-linux-musl"),
        "aarch64" | "arm64" => Some("aarch64-unknown-linux-musl"),
        _ => None,
    }
}

fn rust_target_installed(triple: &str) -> bool {
    let Ok(output) = cross_tool_command_in_checkout("rustup")
        .args(["target", "list", "--installed"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.trim() == triple)
}

/// `Ok(None)` = binary shipped via the fast path; `Ok(Some(reason))` = the
/// fast path is unavailable (the reason feeds the fallback confirmation);
/// `Err` = the fast path was attempted and broke.
fn try_local_cross_install(ssh: &SshControl, source_stamp: &str) -> Result<Option<String>> {
    if let Some(reason) = cross_compile_unavailable_reason() {
        eprintln!("Local cross-build skipped: {reason}");
        return Ok(Some(reason));
    }
    let Some(triple) = remote_target_triple(ssh)? else {
        let reason = String::from("could not detect the remote architecture");
        eprintln!("Local cross-build skipped: {reason}");
        return Ok(Some(reason));
    };
    if !rust_target_installed(triple) {
        let reason = format!(
            "rustup target `{triple}` not installed (run `rustup target add {triple}` once to enable the fast path)"
        );
        eprintln!("Local cross-build skipped: {reason}");
        return Ok(Some(reason));
    }

    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    sync_workspace_lock(&source, |msg| println!("{msg}"));
    raise_fd_limit();
    let jobs = std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(1)
        .to_string();
    println!("Cross-compiling croft locally for {triple} (niced, {jobs} jobs)");
    let status = cargo_zigbuild_command(&source, triple, &jobs)
        .status()
        .context("running cargo zigbuild")?;
    if !status.success() {
        anyhow::bail!("cargo zigbuild exited with {status}");
    }

    let binary = source
        .join("target")
        .join(triple)
        .join("remote-fast")
        .join("croft");
    if !binary.exists() {
        anyhow::bail!(
            "cargo zigbuild reported success but {} is missing",
            binary.display()
        );
    }

    let mkdir = ssh
        .command()
        .arg(&ssh.host)
        .arg("mkdir -p \"$HOME/.cargo/bin\" \"$HOME/.cache/croft\"")
        .status()
        .context("creating remote install dirs")?;
    if !mkdir.success() {
        anyhow::bail!("remote mkdir exited with {mkdir}");
    }

    let ssh_e = format!(
        "ssh -S {} -o ControlMaster=no",
        shell_quote_for_e_arg(&ssh.socket_path),
    );
    let dest = format!("{}:.cargo/bin/croft.new", ssh.host);
    let rsync_status = Command::new("rsync")
        .args(["-az", "--checksum", "-e"])
        .arg(&ssh_e)
        .arg(&binary)
        .arg(&dest)
        .status()
        .context("rsyncing croft binary to remote")?;
    if !rsync_status.success() {
        anyhow::bail!("rsync exited with {rsync_status}");
    }

    // Atomic-swap the freshly-shipped binary into place. `mv` on the
    // same filesystem is the standard way to avoid the race where a
    // mid-flight process opens a half-written executable.
    let activate_status = ssh
        .command()
        .arg(&ssh.host)
        .arg(activate_command(source_stamp))
        .status()
        .context("activating remote croft binary")?;
    if !activate_status.success() {
        anyhow::bail!("remote activation exited with {activate_status}");
    }
    println!("Installed croft on remote via local cross-build.");
    Ok(None)
}

/// What the on-box compile changes on the remote besides building croft,
/// said before asking (#950): a "compile anyway?" that installs a toolchain
/// and system packages without naming them is not consent.
/// [`remote_install_command`] is the script these describe.
pub const REMOTE_COMPILE_SIDE_EFFECTS: [&str; 2] = [
    "If Rust is missing there, rustup installs it into ~/.rustup and ~/.cargo (about 1 GB); shell startup files are left alone.",
    "If a C compiler, pkg-config or dtach is missing, or curl and ca-certificates to fetch Rust, the system package manager installs them, through `sudo -n` when not root.",
];

/// Ask on the controlling terminal before the slow on-box compile. The
/// answer defaults to "no": quitting to run `croft setup-cross` once turns
/// every future install into a seconds-long prebuilt-binary ship, so the
/// slow path must be an explicit choice, never a silent fallback.
/// Non-interactive stdin (scripts, CI) declines for the same reason.
fn confirm_remote_compile_on_tty(host: &str, reason: &str) -> bool {
    use std::io::{IsTerminal as _, Write as _};
    eprintln!("Fast install unavailable: {reason}");
    eprintln!(
        "Croft can compile itself on {host} instead; the first build can take several minutes and loads the box."
    );
    for line in REMOTE_COMPILE_SIDE_EFFECTS {
        eprintln!("  - {line}");
    }
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "stdin is not a terminal; declining the remote compile. Run `croft setup-cross`, then retry."
        );
        return false;
    }
    eprint!(
        "Compile on {host} anyway? [y/N] (N quits; run `croft setup-cross` once and reconnect — much faster) "
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn remote_install_needed(ssh: &SshControl, local_stamp: &str) -> Result<bool> {
    let output = ssh
        .command()
        .arg(&ssh.host)
        .arg(remote_install_check_command())
        .output()
        .context("checking remote croft install")?;
    if !output.status.success() {
        return Ok(true);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() != local_stamp)
}

fn remote_install_check_command() -> &'static str {
    r#"if [ -f "$HOME/.cargo/env" ]; then
  . "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"
command -v croft >/dev/null 2>&1 && cat "$HOME/.cache/croft/install-stamp" 2>/dev/null"#
}

fn sync_local_source_to_remote(ssh: &SshControl) -> Result<()> {
    match sync_via_rsync(ssh) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Fallback path so first-time installs on minimal remotes
            // (alpine images, locked-down VMs) that ship without rsync
            // still succeed. The trade-off is a from-scratch dependency
            // rebuild on those hosts, same as the legacy behaviour.
            eprintln!("rsync sync failed ({e}); falling back to tar pipe");
            sync_via_tar(ssh)
        }
    }
}

/// Mirror the local source tree onto the remote with rsync, preserving
/// mtimes and skipping unchanged files. Critically, this does NOT delete
/// `target/` on the remote, so `cargo install` does an incremental rebuild
/// (typically rebuilding only the 1-3 crates that actually changed)
/// instead of starting from scratch every time.
fn sync_via_rsync(ssh: &SshControl) -> Result<()> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mkdir_status = ssh
        .command()
        .arg(&ssh.host)
        .arg(remote_source_dir_prep(&source))
        .status()
        .context("creating remote source dir")?;
    if !mkdir_status.success() {
        anyhow::bail!("remote mkdir exited with {mkdir_status}");
    }
    let ssh_e = format!(
        "ssh -S {} -o ControlMaster=no",
        shell_quote_for_e_arg(&ssh.socket_path),
    );
    let mut source_arg: std::ffi::OsString = source.clone().into_os_string();
    source_arg.push("/");
    let dest = format!("{}:.cache/croft/source/", ssh.host);
    let status = Command::new("rsync")
        .args(["-a", "-z", "--delete"])
        .args(source_sync_filter_args())
        .arg("-e")
        .arg(&ssh_e)
        .arg(&source_arg)
        .arg(&dest)
        .status()
        .context("running rsync to remote")?;
    if !status.success() {
        anyhow::bail!("rsync exited with {status}");
    }
    Ok(())
}

/// Last-resort tar pipe sync used when rsync isn't available on either
/// side. This path also keeps the remote source dir in place rather than
/// blowing it away — extracting over an existing tree overwrites changed
/// files, so `target/` still survives between installs even when the user
/// is forced down this branch.
fn sync_via_tar(ssh: &SshControl) -> Result<()> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut tar = Command::new("tar")
        .env("COPYFILE_DISABLE", "1")
        .args(["-czf", "-"])
        .args(
            SOURCE_SKIP_NAMES
                .iter()
                .map(|name| format!("--exclude={name}")),
        )
        .arg("-C")
        .arg(&source)
        .args(source_sync_tar_members(&source))
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("packing {}", source.display()))?;

    let tar_stdout = tar.stdout.take().context("opening tar stdout")?;
    let mut remote = ssh
        .command()
        .arg(&ssh.host)
        .arg(format!(
            "{} && tar -xzf - -C \"$HOME/.cache/croft/source\"",
            remote_source_dir_prep(&source)
        ))
        .stdin(Stdio::from(tar_stdout))
        .spawn()
        .context("copying croft source to remote")?;

    let ssh_status = remote.wait().context("waiting for remote source copy")?;
    let tar_status = tar.wait().context("waiting for local source archive")?;
    if !tar_status.success() {
        anyhow::bail!("local source archive failed with {tar_status}");
    }
    if !ssh_status.success() {
        anyhow::bail!("remote source copy failed with {ssh_status}");
    }
    Ok(())
}

/// Quote a path for embedding inside rsync's `-e "ssh -S <path> "`
/// argument. rsync executes the remote-shell string through `/bin/sh -c`,
/// so any spaces or shell metacharacters in the control-socket path would
/// break the invocation. The control socket lives under
/// `std::env::temp_dir()` which is normally space-free, but the socket
/// path is process-id + millisecond-stamped so quote defensively rather
/// than trust the format.
pub(crate) fn shell_quote_for_e_arg(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
    {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn remote_install_command(source_stamp: &str) -> String {
    format!(
        r#"set -e
if [ -f "$HOME/.cargo/env" ]; then
  . "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"

# Package installs need root. Run them directly when already root or when
# there is no sudo, otherwise through sudo. `sudo -n` never prompts: an
# account that needs a password fails at once with a clear message instead
# of sudo's TTY errors on a session with no terminal (#950). A function, not
# a variable holding "sudo -n": zsh, a common login shell, does not split an
# unquoted variable into words, so it would look for a command "sudo -n".
croft_sudo() {{
  if [ "$(id -u)" != "0" ] && command -v sudo >/dev/null 2>&1; then
    sudo -n "$@"
  else
    "$@"
  fi
}}

# Install system packages with whatever package manager the box ships.
croft_pkg_install() {{
  if command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    croft_sudo apt-get update
    croft_sudo apt-get install -y "$@"
  elif command -v dnf >/dev/null 2>&1; then
    croft_sudo dnf install -y "$@"
  elif command -v yum >/dev/null 2>&1; then
    croft_sudo yum install -y "$@"
  elif command -v apk >/dev/null 2>&1; then
    croft_sudo apk add "$@"
  elif command -v pacman >/dev/null 2>&1; then
    croft_sudo pacman -Sy --noconfirm "$@"
  elif command -v zypper >/dev/null 2>&1; then
    croft_sudo zypper install -y "$@"
  else
    return 1
  fi
}}

# croft compiles native crates from source, so the final link step needs a C
# compiler/linker (`cc`) and pkg-config. Stock cloud images (Ubuntu Server,
# minimal Fedora, Alpine, ...) ship without them, which is why a bare
# `cargo install` dies with `linker `cc` not found`. The user opted into a
# from-source install, so install the toolchain rather than fail. This runs
# unconditionally, even when cargo already exists, because cargo can be
# present on a box that still lacks cc. The per-manager package names differ:
# Debian bundles the compiler in build-essential; others name it separately.
croft_ensure_build_toolchain() {{
  if command -v cc >/dev/null 2>&1 && command -v pkg-config >/dev/null 2>&1; then
    return 0
  fi
  if command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    croft_sudo apt-get update
    croft_sudo apt-get install -y build-essential pkg-config
  elif command -v dnf >/dev/null 2>&1; then
    croft_sudo dnf install -y gcc make pkgconf-pkg-config
  elif command -v yum >/dev/null 2>&1; then
    croft_sudo yum install -y gcc make pkgconfig
  elif command -v apk >/dev/null 2>&1; then
    croft_sudo apk add build-base pkgconf
  elif command -v pacman >/dev/null 2>&1; then
    croft_sudo pacman -Sy --noconfirm base-devel pkgconf
  elif command -v zypper >/dev/null 2>&1; then
    croft_sudo zypper install -y gcc make pkg-config
  else
    echo 'croft: no supported package manager found to install a C toolchain (need cc + pkg-config)' >&2
    return 1
  fi
}}

if ! command -v cargo >/dev/null 2>&1; then
  if ! command -v curl >/dev/null 2>&1; then
    if ! croft_pkg_install curl ca-certificates; then
      echo 'cargo and curl are missing and no supported package manager was found to bootstrap them' >&2
      exit 127
    fi
  fi
  # --no-modify-path: rustup would otherwise append `. "$HOME/.cargo/env"`
  # to the remote user's ~/.bashrc and ~/.profile (#950). This script puts
  # ~/.cargo/bin on PATH itself, just below.
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
fi
export PATH="$HOME/.cargo/bin:$PATH"
# Ensure the C toolchain before compiling, regardless of whether cargo was
# already installed: a box can have rustup/cargo but no cc.
croft_ensure_build_toolchain
# dtach lets a remote croft session survive an SSH transport drop (laptop
# sleep, network change): croft launches itself under it for reconnect. Best
# effort and never fatal; a box without dtach just runs without persistence.
command -v dtach >/dev/null 2>&1 || croft_pkg_install dtach || true
# Cap the host build to ~half its cores and run it niced: this fallback
# compiles on the shared remote (often while other workloads run and,
# with launch-now, while a live croft session is using the box), so the
# default `cargo install -j <all cores>` would pin the machine. (nproc+1)/2
# stays >=1 even on a single core; nice is best-effort if present.
CROFT_JOBS=$(( ( $(nproc 2>/dev/null || echo 2) + 1 ) / 2 ))
# A live croft session on this box outranks the build entirely: the user
# is typing into a shell that shares these cores, RAM, and disk. Half the
# cores still wrecks a small VPS, so drop to one compile job and put all
# codegen IO in the idle class; the update simply takes longer.
CROFT_LIVE=""
if pgrep -x croft >/dev/null 2>&1; then
  CROFT_JOBS=1
  CROFT_LIVE=1
fi
# nice and ionice ration CPU and IO, not memory (#694). Each rustc job on
# croft costs well over a GB, so on a host under 16 GB two of them next to
# rust-analyzer were enough to wake the OOM killer, which then took croft or
# a terminal pane. Below that, compile one crate at a time.
CROFT_MEM_KB=$(awk '/^MemTotal:/ {{ print $2 }}' /proc/meminfo 2>/dev/null || true)
CROFT_AVAIL_KB=$(awk '/^MemAvailable:/ {{ print $2 }}' /proc/meminfo 2>/dev/null || true)
if [ -n "$CROFT_MEM_KB" ] && [ "$CROFT_MEM_KB" -lt 16777216 ]; then
  CROFT_JOBS=1
fi
# Little memory free right now counts too, whatever the total: a big host
# can still be mostly spoken for by the time the update runs.
if [ -n "$CROFT_AVAIL_KB" ] && [ "$CROFT_AVAIL_KB" -lt 2097152 ]; then
  CROFT_JOBS=1
  echo "croft: only $(( CROFT_AVAIL_KB / 1024 )) MB of memory available; building with one job" >&2
fi
# And cap the build's memory outright where systemd can: the compile runs
# in its own scope limited to 60% of RAM with no swap, so if it outgrows
# that it is the BUILD the kernel kills, inside its scope, and never croft
# or a shell. Probed first, because a user manager or a delegated memory
# controller is not a given on a server; without one the build runs uncapped
# but still single-job on a small host.
CROFT_MEMCAP=""
# The probe reads the limit back from inside a test scope: systemd accepts
# MemoryMax without complaint where the memory controller is not delegated
# to the user manager (cgroup v1 and hybrid hosts), and enforces nothing.
if [ -n "$CROFT_MEM_KB" ] && command -v systemd-run >/dev/null 2>&1 \
  && [ "$(systemd-run --user --scope --quiet -p MemoryMax=64M -p MemorySwapMax=0 \
    sh -c 'cat "/sys/fs/cgroup$(sed -n "s/^0:://p" /proc/self/cgroup)/memory.max"' 2>/dev/null)" = 67108864 ]; then
  CROFT_MEMCAP="systemd-run --user --scope --quiet -p MemoryMax=$(( CROFT_MEM_KB * 6 / 10 ))K -p MemorySwapMax=0"
fi
CROFT_NICE=""
if command -v nice >/dev/null 2>&1; then CROFT_NICE="nice -n 19"; fi
CROFT_IONICE=""
if command -v ionice >/dev/null 2>&1; then CROFT_IONICE="ionice -c3"; fi
# Tell a croft running on this box that a compile is under way, so it stops
# its own language servers (rust-analyzer alone held 2.4 GB in the OOM that
# prompted #694) and restarts them once no marker names a live build.
#
# One marker per build (`building.<this shell's pid>`), so a second install
# finishing first cannot clear the first's. It holds the pid of the COMPILE
# once it starts (systemd-run --scope, nice and ionice all exec, so `$!` is
# cargo itself): a shell killed mid-build leaves cargo running as an orphan,
# and the marker must keep naming it. Markers whose pid is gone are stale;
# they are swept here, and croft ignores them and any from before a reboot.
#
# Which boot a marker belongs to is `building.<pid>.boot`, the kernel's
# boot_id (#736): unlike boot time it does not move when the wall clock is
# stepped. A sidecar rather than a second field, so a croft from before it,
# which reads the marker as one pid, still pauses for this build.
mkdir -p "$HOME/.cache/croft"
# The script runs under the login shell, and zsh aborts on a glob that
# matches nothing (NOMATCH), which an empty cache always is (#734).
# null_glob makes it expand to nothing, as the `[ -f ]` below expects of
# every other shell. Not `emulate sh`: that also clears ERR_EXIT, and a
# failed compile would then write the install stamp.
[ -z "${{ZSH_VERSION:-}}" ] || setopt null_glob
CROFT_BOOT=$(cat /proc/sys/kernel/random/boot_id 2>/dev/null || true)
for CROFT_OLD in "$HOME/.cache/croft"/building.*; do
  [ -f "$CROFT_OLD" ] || continue
  case "$CROFT_OLD" in
    # Its marker sorts first, so a sidecar whose marker is gone is orphaned.
    *.boot) [ -e "${{CROFT_OLD%.boot}}" ] || rm -f "$CROFT_OLD"; continue ;;
  esac
  CROFT_OLD_PID=$(cat "$CROFT_OLD" 2>/dev/null || true)
  CROFT_OLD_BOOT=$(cat "$CROFT_OLD.boot" 2>/dev/null || true)
  if [ -z "$CROFT_OLD_PID" ] || ! kill -0 "$CROFT_OLD_PID" 2>/dev/null \
    || {{ [ -n "$CROFT_OLD_BOOT" ] && [ -n "$CROFT_BOOT" ] && [ "$CROFT_OLD_BOOT" != "$CROFT_BOOT" ]; }}; then
    rm -f "$CROFT_OLD" "$CROFT_OLD.boot"
  fi
done
CROFT_MARK="$HOME/.cache/croft/building.$$"
CROFT_BUILD_PID=""
if [ -n "$CROFT_BOOT" ]; then printf %s "$CROFT_BOOT" > "$CROFT_MARK.boot"; fi
printf %s "$$" > "$CROFT_MARK"
# Keep the marker while the compile outlives this shell; croft sees it go
# stale the moment the compile ends.
trap 'if [ -z "$CROFT_BUILD_PID" ] || ! kill -0 "$CROFT_BUILD_PID" 2>/dev/null; then rm -f "$CROFT_MARK" "$CROFT_MARK.boot"; fi' EXIT
if [ -n "$CROFT_LIVE" ]; then
  # croft polls the marker once a second, and a server gets up to three
  # seconds to shut down cleanly: let both finish before the first rustc.
  sleep 5
fi
# eval, because this script runs under the remote user's login shell and
# zsh does not word-split unquoted parameters: bare `$CROFT_NICE ...` would
# try to run a command literally named "nice -n 19". eval re-parses the
# assembled line, which splits correctly under both sh/bash and zsh.
# The subshell marks the build (and every rustc under it) as the kernel's
# first choice when memory runs out, without touching this shell (#694),
# then `exec`s it, so `$!` below is still the compile's own pid.
(
  echo 1000 > /proc/self/oom_score_adj 2>/dev/null || true
  eval "exec $CROFT_MEMCAP $CROFT_NICE $CROFT_IONICE"' cargo install --path "$HOME/.cache/croft/source" --jobs "$CROFT_JOBS" --force --locked'
) &
CROFT_BUILD_PID=$!
printf %s "$CROFT_BUILD_PID" > "$CROFT_MARK"
wait "$CROFT_BUILD_PID"
rm -f "$CROFT_MARK" "$CROFT_MARK.boot"
printf %s {stamp} > "$HOME/.cache/croft/install-stamp"
rm -f "$HOME/.cache/croft/updating"
"#,
        stamp = shell_quote(source_stamp)
    )
}

/// Final swap step shared by the cross-build and rsync fast paths: chmod
/// the staged binary, atomically `mv` it over the live one, write the new
/// install-stamp, then clear the `updating` marker. The stamp is written
/// only after the binary is in place, so a remote-launched croft that
/// watches the stamp never re-execs into a half-shipped binary.
fn activate_command(source_stamp: &str) -> String {
    format!(
        "chmod 755 \"$HOME/.cargo/bin/croft.new\" && mv \"$HOME/.cargo/bin/croft.new\" \"$HOME/.cargo/bin/croft\" && printf %s {stamp} > \"$HOME/.cache/croft/install-stamp\" && rm -f \"$HOME/.cache/croft/updating\"",
        stamp = shell_quote(source_stamp)
    )
}

/// Drop the `updating` marker so a remote-launched croft shows its
/// "Updating" indicator for the whole build+ship, not just the final
/// remote-side activation. Written before the (long) local cross-compile.
fn mark_remote_updating(ssh: &SshControl) {
    let _ = ssh
        .background_command()
        .arg(&ssh.host)
        .arg("mkdir -p \"$HOME/.cache/croft\" && : > \"$HOME/.cache/croft/updating\"")
        .status();
}

/// Clear the marker after a failed install so the indicator resolves to a
/// brief "update failed" rather than hanging on "Updating" forever.
fn clear_remote_updating(ssh: &SshControl) {
    let _ = ssh
        .background_command()
        .arg(&ssh.host)
        .arg("rm -f \"$HOME/.cache/croft/updating\"")
        .status();
}

/// True when a croft binary is already resolvable on the remote PATH,
/// regardless of version. Drives the launch-now path: an existing binary
/// can be launched immediately while a newer one installs underneath it.
fn remote_croft_present(ssh: &SshControl) -> Result<bool> {
    let output = ssh
        .command()
        .arg(&ssh.host)
        .arg(
            r#"if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
export PATH="$HOME/.cargo/bin:$PATH"
command -v croft >/dev/null 2>&1 && echo yes"#,
        )
        .output()
        .context("probing remote croft presence")?;
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "yes")
}

fn local_source_stamp() -> Result<String> {
    build_stamp_for(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        env!("CARGO_PKG_VERSION"),
    )
}

/// The stamp of a croft built from `root` at `version`: the hash of its
/// source, or `release-v<version>` when `root` does not exist here. A
/// release binary bakes the CI checkout it was built in; hashing a missing
/// tree gives one constant for every version, so after the first install a
/// remote would never update again, its watcher never fire and F9 never
/// offer the relaunch (#261). A release's version names its binary exactly.
fn build_stamp_for(root: &PathBuf, version: &str) -> Result<String> {
    if !root.exists() {
        return Ok(format!("release-v{version}"));
    }
    source_stamp_for(root)
}

/// A croft installed with `cargo install` from a registry or git snapshot
/// bakes that frozen checkout as its `CARGO_MANIFEST_DIR`, so the stamp it
/// hashes can never change: "already up to date" then reports on a dead tree
/// while the user's working repo drifts, and installs silently ship nothing.
/// (2026-08-22: a session-host fix believed deployed was never shipped
/// because of exactly this.) Every install path surfaces this warning so the
/// no-op is loud instead of silent.
fn source_snapshot_warning() -> Option<String> {
    let dir = env!("CARGO_MANIFEST_DIR");
    source_dir_is_snapshot(dir).then(|| {
        format!(
            "WARNING: this croft was installed from an immutable snapshot ({dir}); remote installs can never ship local changes. Reinstall with `cargo install --path <your repo>` to make installs track your tree."
        )
    })
}

/// The two places `cargo install` unpacks immutable sources: registry
/// tarballs and git checkouts. Both are subtrees of `$CARGO_HOME`, and a
/// custom `CARGO_HOME` need not contain `.cargo` anywhere in its path
/// (`CARGO_HOME=/custom/cargo-home` unpacks to
/// `/custom/cargo-home/registry/src/...`), so only the two layout-defining
/// components are matched, with no assumption about the prefix.
fn source_dir_is_snapshot(dir: &str) -> bool {
    dir.contains("/registry/src/") || dir.contains("/git/checkouts/")
}

/// Names of the root entries that shape the shipped binary: the crate source,
/// the embedded asset tree, and the build/toolchain pins. The stamp hashes
/// exactly these, so build artifacts (`target`, `target.noindex`), `.git`,
/// docs, and editor scratch dirs can never stall the hash or force a reship —
/// the old deny-list stamp read a 98 GB `target.noindex` on every connect and
/// changed after every local build.
/// Names skipped at EVERY level of the crate source, by the stamp walk and by
/// the sync filters alike. Sharing the list is what makes "the stamp and the
/// sync agree on what the source is" a fact rather than a hope.
const SOURCE_SKIP_NAMES: &[&str] = &[".git", "target", ".DS_Store"];

const SOURCE_STAMP_INPUTS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "build.rs",
    "rust-toolchain.toml",
    "src",
    "assets",
    // Dependencies croft builds from its own tree (`[patch.crates-io]` in
    // Cargo.toml): without them a remote `cargo install --locked` fails.
    "vendor",
];

fn source_stamp_for(root: &PathBuf) -> Result<String> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for name in SOURCE_STAMP_INPUTS {
        let path = root.join(name);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        hasher.write(name.as_bytes());
        if meta.is_dir() {
            hash_source_dir(root, &path, &mut hasher)?;
        } else {
            hasher.write_u64(meta.len());
            hasher.write(
                &std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
            );
        }
    }
    Ok(format!("{:016x}", hasher.finish()))
}

fn hash_source_dir(root: &PathBuf, dir: &PathBuf, hasher: &mut impl Hasher) -> Result<()> {
    let mut entries = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("reading entries in {}", dir.display()))?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if SOURCE_SKIP_NAMES.contains(&name.as_ref()) {
            continue;
        }
        let meta = entry
            .metadata()
            .with_context(|| format!("reading metadata for {}", path.display()))?;
        let rel = path.strip_prefix(root).unwrap_or(&path);
        hasher.write(rel.to_string_lossy().as_bytes());
        if meta.is_dir() {
            hasher.write_u8(b'/');
            hash_source_dir(root, &path, hasher)?;
        } else if meta.is_file() {
            hasher.write_u64(meta.len());
            hasher.write(
                &std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
            );
        }
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod offer_tests;
