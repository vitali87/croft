//! A remote croft session: launching it, its outcome, and keeping it alive across a
//! dropped connection by reconnecting the master and handing the terminal back.

use super::*;

pub fn launch_croft(host: &str, path: Option<&str>, solo: bool) -> Result<RemoteOutcome> {
    launch_croft_with(host, path, None, solo)
}

pub const DROP_TO_LOCAL_EXIT_CODE: i32 = 88;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteOutcome {
    ReturnToLocal,
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteStatusClass {
    ReturnToLocal,
    Exited,
    NotInstalled,
    Failed,
}

pub(super) fn classify_remote_status(code: Option<i32>) -> RemoteStatusClass {
    match code {
        Some(DROP_TO_LOCAL_EXIT_CODE) => RemoteStatusClass::ReturnToLocal,
        Some(0) => RemoteStatusClass::Exited,
        Some(127) => RemoteStatusClass::NotInstalled,
        _ => RemoteStatusClass::Failed,
    }
}

pub(super) fn outcome_or_bail(status: ExitStatus) -> Result<RemoteOutcome> {
    match classify_remote_status(status.code()) {
        RemoteStatusClass::ReturnToLocal => Ok(RemoteOutcome::ReturnToLocal),
        RemoteStatusClass::Exited => Ok(RemoteOutcome::Exited),
        _ => anyhow::bail!("ssh exited with {status}"),
    }
}

/// True when the remote host has a supervisor croft launches its session
/// under for persistence across an SSH transport drop: croft's own
/// `session-host`, or `dtach` (#831). Best-effort over the existing control
/// master; any error (host unreachable, neither supervisor) reports `false`
/// so croft simply runs without persistence.
pub(super) fn remote_has_session_supervisor(ssh: &SshControl) -> bool {
    ssh.command()
        .arg(&ssh.host)
        .arg(session_supervisor_check_command())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The remote check behind [`remote_has_session_supervisor`]: the same
/// supervisors, probed the same way, as [`remote_croft_command`] launches
/// the session under, so the two never disagree about persistence.
pub(super) fn session_supervisor_check_command() -> &'static str {
    "export PATH=\"$HOME/.cargo/bin:$PATH\"; croft session-host --probe >/dev/null 2>&1 || command -v dtach >/dev/null 2>&1"
}

/// The bytes that hand the terminal back when the remote croft lost it
/// mid-session (#1460). A dropped connection ends the session's ssh without
/// the remote croft ever running its teardown, so the terminal stays on that
/// croft's alt screen with mouse tracking on, while ssh puts the tty back in
/// cooked mode. Each line printed next (the reconnect progress, ssh's own
/// errors) then scrolled croft's screen up, and every mouse move was echoed
/// as `^[[<35;…M` text.
///
/// The kitty keyboard flags go first, while the alt screen that holds them
/// is still showing, and are also set to none: a terminal that keeps one
/// stack for both screens would otherwise send Ctrl+C as an escape code, not
/// an interrupt. The alt screen goes last. The DECSC in front of it makes
/// leaving safe when the main screen already shows (the session dropped
/// before croft drew): leaving restores the main screen's saved cursor, and
/// that is then where the cursor already is. On the alt screen it saves into
/// the alt screen's own slot, which the main screen never reads.
///
/// The terminal's default colours are reset as well, the same as croft's own
/// exit does. Nothing runs after a session that is given up (Ctrl+C during
/// the reconnect, the attempts running out, a host with no session to
/// resume), so this is the last chance to put the shell's colours back. A
/// resumed croft sets its theme's colours again with its modes
/// (`reattach_reassert_seq` in app/mod.rs).
pub(super) fn lost_session_release_seq() -> String {
    let mut seq = String::from(concat!(
        "\x1b[<u\x1b[=0;1u",
        "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l",
        "\x1b[?2004l\x1b[?1004l",
        "\x1b[0m\x1b[0 q\x1b[?25h",
    ));
    seq.push_str(crate::app::reset_host_colors_seq());
    if crate::iterm2_inline::detect_iterm2_inline_support() {
        seq.push_str(&crate::app::reset_session_bg_seq());
    }
    seq.push_str("\x1b7\x1b[?1049l");
    seq
}

/// Give the terminal back after the remote croft lost it (#1460), before
/// anything else is printed: see [`lost_session_release_seq`]. Mouse reports
/// already on their way are then dropped from the tty's input, so they reach
/// neither the shell nor the resumed croft as typed text.
pub(super) fn release_lost_session_terminal() {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(lost_session_release_seq().as_bytes());
    let _ = out.flush();
    #[cfg(unix)]
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
        // SAFETY: tcflush only discards queued input on fd 0, and fails
        // harmlessly when stdin is not a terminal.
        unsafe {
            libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH);
        }
    }
}

/// Re-establish the SSH control master after a transport drop (laptop sleep,
/// network change), printing progress. Each attempt is bounded by the master's
/// `ConnectTimeout`, then backs off; the user presses Ctrl+C to stop and fall
/// back to the local shell. Returns the fresh control once the host answers.
pub(super) fn reconnect_master(host: &str) -> Option<SshControl> {
    const MAX_ATTEMPTS: u32 = 30;
    for attempt in 1..=MAX_ATTEMPTS {
        println!(
            "Connection lost. Reconnecting to {host}\u{2026} (attempt {attempt}; Ctrl+C to stop)"
        );
        match SshControl::start(host) {
            Ok(ssh) => {
                println!("Reconnected to {host}; resuming session.");
                return Some(ssh);
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_secs(2)),
        }
    }
    eprintln!("Could not reconnect to {host}; returning to the local shell.");
    None
}

/// Drive the remote croft session to completion, auto-reconnecting when the
/// SSH transport dies on a host that supports session persistence. Owns the
/// control connection so a reconnect can swap in a fresh master and reattach
/// the dtach session (which kept croft alive remotely) with its state intact.
pub(super) fn run_croft_session(
    mut ssh: SshControl,
    host: &str,
    path: Option<&str>,
    persistent: bool,
    solo: bool,
) -> Result<RemoteOutcome> {
    // Config sync (#262) before the session takes the terminal, so the
    // remote croft reads the user's keybindings on the launch below rather
    // than one launch later. Both entry points converge here — the CLI
    // `launch_croft_with` and the in-app `launch_only` — which the install
    // hook does not, since an already-installed remote installs on a
    // detached thread and never waits.
    // The re-push baseline is taken BEFORE the connect-time push: a file
    // edited while that push runs then still reads as changed, instead of
    // becoming the baseline with the remote holding the older copy.
    let repush_watch =
        crate::config_sync::ConfigWatch::new(crate::config_sync::watched_local_paths());
    push_config_files(&ssh, &SyncResolution::default(), &mut |msg| {
        println!("{msg}")
    });
    // A file edited on this machine while the session runs follows it
    // (#262); the remote croft applies it on arrival. Stops with the session.
    let repush = ConfigRepush::start(&ssh, repush_watch);
    let mut bootstrapped = false;
    // The relay rendezvous is keyed on the launch identity — the very same
    // `hash(launch arg)` the dtach socket uses — NOT the remote croft's
    // `workspace_root`. The workspace diverges from the launch identity (a
    // no-path launch opens the login dir, but the user may then open a
    // subfolder, and that opened root is also preserved across a self re-exec),
    // so a workspace-keyed relay desynced the moment the opened folder differed
    // from where the pump's `pwd` lands. The key is deterministic, so carrying
    // it in env is freeze-safe: dtach freezes it at first launch, but every
    // reconnect recomputes the identical value, so the running croft and a fresh
    // pump always agree on one log. (The original bug was the id being *random*,
    // not its being in env.)
    let relay_id = relay_session_id(path.unwrap_or(""));
    // Per-client-process nonce for the session-host roster (#229). The relay
    // key above cannot serve as a client identity on its own: it is a pure
    // hash of the launch arg, so two terminals opening the SAME path derive
    // the same key and the host would treat the second as the first
    // reconnecting and evict it. Minting this ONCE, outside the loop, gives
    // the pair the two properties the host needs — constant across every
    // reattach this loop performs, distinct in any other process.
    //
    // Unlike the relay key this one is deliberately NOT deterministic, and
    // that is safe for the opposite reason: nothing recomputes it. Only the
    // remote client sends it, and dtach freezing the first launch's value is
    // exactly the desired behavior — that frozen value IS this client's
    // identity for the life of the session.
    let client_nonce = client_process_nonce();
    loop {
        let pump = match DropPump::start(&ssh, &relay_id) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("Drag-drop relay disabled: {e}");
                None
            }
        };
        let env = vec![
            (String::from("CROFT_RELAY_KEY"), relay_id.clone()),
            (String::from("CROFT_CLIENT_NONCE"), client_nonce.clone()),
        ];
        let status = run_remote_croft(&ssh, path, &env, solo)?;
        // A session that failed may have died holding the terminal (#1460).
        if classify_remote_status(status.code()) == RemoteStatusClass::Failed {
            release_lost_session_terminal();
        }
        match classify_remote_status(status.code()) {
            RemoteStatusClass::ReturnToLocal => return Ok(RemoteOutcome::ReturnToLocal),
            RemoteStatusClass::Exited => return Ok(RemoteOutcome::Exited),
            RemoteStatusClass::NotInstalled if !bootstrapped => {
                println!("Croft is not installed on {host}; bootstrapping from local source");
                if let Some(warning) = source_snapshot_warning() {
                    eprintln!("{warning}");
                }
                install_remote_croft(&ssh, &local_source_stamp()?)?;
                bootstrapped = true;
            }
            RemoteStatusClass::Failed if persistent && is_transport_failure(status.code()) => {
                drop(pump);
                match reconnect_master(host) {
                    Some(new_ssh) => {
                        ssh = new_ssh;
                        repush.retarget(&ssh);
                    }
                    None => return Ok(RemoteOutcome::Exited),
                }
            }
            _ => return outcome_or_bail(status),
        }
    }
}

/// Counterpart to `install_only_streaming`: skips the install check and
/// runs the actual remote croft. Must be called only after the terminal
/// has been returned to cooked mode and the alt-screen surrendered, since
/// the spawned ssh shares stdin/stdout/stderr with the user's terminal.
pub fn launch_only(adopted: AdoptedMaster, path: Option<&str>) -> Result<RemoteOutcome> {
    let ssh = SshControl::adopt(adopted);
    let host = ssh.host.clone();
    let persistent = remote_has_session_supervisor(&ssh);
    run_croft_session(ssh, &host, path, persistent, false)
}

/// Same as `launch_croft` but reuses an already-established SSH ControlMaster
/// socket so the in-TUI auth flow (PTY-driven password dialog) doesn't need
/// to prompt the user a second time when the post-quit path hands off the
/// connection.
pub fn launch_croft_with(
    host: &str,
    path: Option<&str>,
    adopted: Option<AdoptedMaster>,
    solo: bool,
) -> Result<RemoteOutcome> {
    println!("Connecting to {host}");
    let ssh = match adopted {
        Some(a) => SshControl::adopt(a),
        None => SshControl::start(host)?,
    };
    if remote_croft_present(&ssh).unwrap_or(false) {
        // A croft is already installed: attach to it right away. Any update
        // cross-builds and ships on a background thread, and the running
        // remote croft offers a relaunch once the new binary lands. The
        // user must never wait behind a build for a session that already
        // exists.
        spawn_background_install(&ssh);
    } else {
        let local_stamp = local_source_stamp()?;
        println!("Installing Croft on {host}");
        if let Some(warning) = source_snapshot_warning() {
            eprintln!("{warning}");
        }
        install_remote_croft(&ssh, &local_stamp)?;
    }
    let persistent = remote_has_session_supervisor(&ssh);
    run_croft_session(ssh, host, path, persistent, solo)
}

pub(super) fn run_remote_croft(
    ssh: &SshControl,
    path: Option<&str>,
    env: &[(String, String)],
    solo: bool,
) -> Result<ExitStatus> {
    let mut command = ssh.command();
    command
        .arg("-tt")
        .arg(&ssh.host)
        .arg(remote_croft_command(path, env, solo));
    command.status().context("starting ssh")
}

/// True for ssh's own connection-failure exit code (255), as opposed to any
/// status the remote croft itself returned. Only this warrants an auto-
/// reconnect; a real remote crash (e.g. 101) must surface to the user.
pub(super) fn is_transport_failure(code: Option<i32>) -> bool {
    code == Some(255)
}

/// True when croft runs inside an SSH login, which is how a remote croft is
/// launched (`croft remote` execs it over ssh).
pub fn running_over_ssh() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_TTY").is_some()
        || std::env::var_os("SSH_CLIENT").is_some()
}
