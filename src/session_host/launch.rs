//! Attaching to a session or starting one first: the detached server's command line and
//! its spawn, and ending a stale server.

use super::*;

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
pub(super) fn kill_stale_server(socket: &Path) {
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
pub(super) fn detached_server_argv(
    socket: &Path,
    workspace: Option<&Path>,
    inner: &[String],
) -> Vec<String> {
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

pub(super) fn spawn_detached_server(
    socket: &Path,
    workspace: Option<&Path>,
    inner: &[String],
) -> Result<()> {
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
