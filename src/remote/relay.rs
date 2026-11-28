//! The drop relay: the pump that tails a remote croft's request log and runs each
//! request on this machine, the requests it understands, and how each is claimed and
//! answered.

use super::*;

/// Drag-drop relay between local Finder and a remote-launched croft.
///
/// Local croft holds an SSH ControlMaster to the remote host. We piggy-
/// back on it to expose a tiny request/response protocol so the remote
/// croft can pull files from the user's local machine without needing
/// macOS Remote Login or reverse port forwarding.
///
/// Wire format on the remote box:
///
///   `~/.cache/croft/relay-<id>/requests.log`
///       Append-only log. Remote croft writes one line per drop:
///         `pull\t<request-id>\t<absolute-local-path>\n`
///   `~/.cache/croft/relay-<id>/inbox/<request-id>/<basename>`
///       Where the file lands once scp succeeds.
///   `~/.cache/croft/relay-<id>/inbox/<request-id>/.ok`
///       Sentinel local croft writes after a successful copy.
///   `~/.cache/croft/relay-<id>/inbox/<request-id>/.err`
///       Sentinel local croft writes with the failure message.
pub(super) struct DropPump {
    stop: Arc<AtomicBool>,
    tail: Option<Child>,
    handle: Option<JoinHandle<()>>,
}

/// Failure message for the relay-setup ssh. The stderr suffix is appended
/// only when the child actually said something: a signal-killed ssh or a
/// silent `set -e` exit (full disk under `mkdir -p`/`printf`) has an empty
/// stderr, and the message must not end in a dangling ": ".
pub(super) fn relay_setup_error(status: ExitStatus, stderr: &[u8]) -> String {
    let err = String::from_utf8_lossy(stderr);
    let err = err.trim();
    if err.is_empty() {
        format!("remote relay setup failed with {status}")
    } else {
        format!("remote relay setup failed with {status}: {err}")
    }
}

impl DropPump {
    pub(super) fn start(ssh: &SshControl, relay_id: &str) -> Result<Self> {
        // `relay_id` is the deterministic `hash(launch arg)` shared with the
        // remote croft via `CROFT_RELAY_KEY` (see `run_croft_session` and
        // `App::relay_dir`). Resolve $HOME on the remote and capture the literal
        // absolute relay paths so the tail and the .ok/.err sentinels all agree
        // on one location. The remote croft creates this dir lazily too, so
        // `tail -F` tolerates it not existing yet.
        let id = relay_id;
        let resolve = format!(
            "set -e; \
             RELAY=\"$HOME/.cache/croft/relay-{id}\"; \
             INBOX=\"$RELAY/inbox\"; \
             LOG=\"$RELAY/requests.log\"; \
             mkdir -p \"$INBOX\"; \
             : > \"$LOG\"; \
             printf '%s\\n%s\\n' \"$INBOX\" \"$LOG\""
        );
        // Capture stderr so a setup failure carries ssh's own diagnosis in
        // the bail! message rather than a bare exit code. (This runs in the
        // pre-TUI cooked-mode flow, so inheriting would be safe — it just
        // produced worse errors.)
        let output = ssh
            .command()
            .arg(&ssh.host)
            .arg(&resolve)
            .stderr(Stdio::piped())
            .output()
            .context("preparing remote drop relay")?;
        if !output.status.success() {
            anyhow::bail!("{}", relay_setup_error(output.status, &output.stderr));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines();
        let inbox_dir = lines
            .next()
            .ok_or_else(|| anyhow::anyhow!("remote relay setup returned no inbox path"))?
            .trim()
            .to_string();
        let requests_log = lines
            .next()
            .ok_or_else(|| anyhow::anyhow!("remote relay setup returned no log path"))?
            .trim()
            .to_string();
        if inbox_dir.is_empty() || requests_log.is_empty() {
            anyhow::bail!("remote relay setup returned blank paths");
        }
        let mut tail = Ssh::bare_client(&ssh.socket_path)
            .arg(&ssh.host)
            .arg(format!(
                "exec tail -F -n 0 {} 2>/dev/null",
                shell_quote(&requests_log)
            ))
            .into_command()
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
            .context("spawning remote requests tail")?;
        let stdout = tail
            .stdout
            .take()
            .context("capturing remote requests tail stdout")?;
        let stop = Arc::new(AtomicBool::new(false));
        let pump_ssh_host = ssh.host.clone();
        let pump_socket = ssh.socket_path.clone();
        let pump_inbox = inbox_dir.clone();
        let pump_stop = stop.clone();
        let handle = thread::spawn(move || {
            run_pump(pump_ssh_host, pump_socket, pump_inbox, stdout, pump_stop);
        });
        Ok(Self {
            stop,
            tail: Some(tail),
            handle: Some(handle),
        })
    }
}

impl Drop for DropPump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut tail) = self.tail.take() {
            let _ = tail.kill();
            let _ = tail.wait();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// How long a window without a pull's file waits before answering it as
/// missing, so an attached window that has the file claims it first.
pub(super) const MISSING_PULL_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

pub(super) fn run_pump(
    host: String,
    socket: PathBuf,
    inbox_dir: String,
    stdout: std::process::ChildStdout,
    stop: Arc<AtomicBool>,
) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(line) = line else { break };
        let request = parse_relay_request(&line);
        // Two local windows attached to one session tail the same log. The
        // first to claim a request handles it; without this every window
        // ran every open, forward and pull, one browser tab each (#648).
        //
        // A pull names a file on ONE computer: when two computers are
        // attached, only the one that has it may claim it, or the other
        // would claim it, fail, and leave the file's owner nothing to do.
        //
        // A file on NEITHER computer still needs an answer, or the remote
        // waits out its whole timeout: after a grace period that lets the
        // owner claim it first, whoever claims it now says it is missing.
        if let Some(RelayRequest::Pull { id, src }) = &request
            && !Path::new(src).exists()
        {
            let (host, socket, inbox_dir) = (host.clone(), socket.clone(), inbox_dir.clone());
            let (id, src) = (id.clone(), src.clone());
            thread::spawn(move || {
                thread::sleep(MISSING_PULL_GRACE);
                if ssh_exec(&host, &socket, &claim_command(&inbox_dir, &id)) {
                    handle_pull_request(&host, &socket, &inbox_dir, &id, &src);
                }
            });
            continue;
        }
        if let Some(id) = request.as_ref().and_then(RelayRequest::id)
            && !ssh_exec(&host, &socket, &claim_command(&inbox_dir, id))
        {
            continue;
        }
        match request {
            Some(RelayRequest::Pull { id, src }) => {
                handle_pull_request(&host, &socket, &inbox_dir, &id, &src);
            }
            Some(RelayRequest::Clipboard { id }) => {
                handle_clipboard_request(&host, &socket, &inbox_dir, &id);
            }
            Some(RelayRequest::Copy { id }) => {
                handle_copy_request(&host, &socket, &inbox_dir, &id);
            }
            Some(RelayRequest::Open { id, url }) => {
                handle_open_request(&host, &socket, &inbox_dir, &id, &url);
            }
            Some(RelayRequest::Forward { id, port, open }) => {
                handle_forward_request(&host, &socket, &inbox_dir, &id, &port, open);
            }
            Some(RelayRequest::Unforward { local, remote }) => {
                handle_unforward_request(&host, &socket, &local, &remote);
            }
            None => {}
        }
    }
}

pub(super) enum RelayRequest {
    Pull {
        id: String,
        src: String,
    },
    Clipboard {
        id: String,
    },
    /// The remote croft copied text and wants it on THIS machine's clipboard,
    /// which is the one the user pastes from (#538).
    ///
    /// Carries only the request id: the payload lives at `<inbox>/<id>.txt`,
    /// derived by the pump from the inbox it already owns. Putting a PATH on
    /// the wire would let anything able to write the remote log name a FIFO,
    /// `/dev/zero`, or a file outside the relay entirely, and the pump reads
    /// what it is given into THIS machine's memory.
    Copy {
        id: String,
    },
    Open {
        id: String,
        url: String,
    },
    /// Forward a remote loopback port back to the local machine over the live
    /// SSH master. `open` also launches the local browser once the tunnel is up.
    Forward {
        id: String,
        port: String,
        open: bool,
    },
    /// Tear down a forward previously added by [`RelayRequest::Forward`]. Fire
    /// and forget — no inbox sentinels, since croft just drops the entry (the
    /// parsed request id is validated but unused).
    Unforward {
        local: String,
        remote: String,
    },
}

impl RelayRequest {
    /// The id a pump claims before acting. `Unforward` has none: each pump
    /// tears down only a tunnel it holds, so every pump may see it.
    pub(super) fn id(&self) -> Option<&str> {
        match self {
            Self::Pull { id, .. }
            | Self::Clipboard { id }
            | Self::Copy { id }
            | Self::Open { id, .. }
            | Self::Forward { id, .. } => Some(id),
            Self::Unforward { .. } => None,
        }
    }
}

pub(super) fn parse_relay_request(line: &str) -> Option<RelayRequest> {
    let line = line.trim();
    let mut parts = line.split('\t');
    let kind = parts.next()?;
    let id = parts.next()?.to_string();
    if id.is_empty() {
        return None;
    }
    match kind {
        "pull" => {
            let src = parts.next()?.to_string();
            if src.is_empty() {
                return None;
            }
            Some(RelayRequest::Pull { id, src })
        }
        "clipboard" => Some(RelayRequest::Clipboard { id }),
        "copy" => Some(RelayRequest::Copy { id }),
        "open" => {
            let url = parts.next()?.to_string();
            if url.is_empty() {
                return None;
            }
            Some(RelayRequest::Open { id, url })
        }
        "forward" => {
            let port = parts.next()?.to_string();
            if port.is_empty() {
                return None;
            }
            let open = parts.next() == Some("1");
            Some(RelayRequest::Forward { id, port, open })
        }
        "unforward" => {
            let _ = id;
            let local = parts.next()?.to_string();
            let remote = parts.next()?.to_string();
            if local.is_empty() || remote.is_empty() {
                return None;
            }
            Some(RelayRequest::Unforward { local, remote })
        }
        _ => None,
    }
}

/// The remote command that claims request `id` for this pump: `mkdir` is
/// atomic, so of several pumps exactly one succeeds.
pub(super) fn claim_command(inbox_dir: &str, id: &str) -> String {
    let claims = format!("{inbox_dir}/.claims");
    format!(
        "mkdir -p {} && mkdir {}",
        shell_quote(&claims),
        shell_quote(&format!("{claims}/{id}"))
    )
}

/// Answer request `request_id` with `message` in its `.err`. The message
/// goes on ssh's stdin, not its command line: a reason as long as a noisy
/// tar's warnings is past what one argument may hold, and refused there it
/// left no `.err`, so the remote waited out its whole timeout (#2795).
pub(super) fn write_relay_err(
    host: &str,
    socket: &Path,
    inbox_dir: &str,
    request_id: &str,
    message: &str,
) {
    let dest_dir = format!("{inbox_dir}/{request_id}");
    let err_path = format!("{dest_dir}/.err");
    let cmd = format!(
        "mkdir -p {dir} && cat > {err}",
        dir = shell_quote(&dest_dir),
        err = shell_quote(&err_path),
    );
    let Ok(mut ssh) = Ssh::bare_client(socket)
        .arg(host)
        .arg(&cmd)
        .into_command()
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = ssh.stdin.take() {
        // A remote that stops reading only ends this write early.
        let _ = std::io::Write::write_all(&mut stdin, message.as_bytes());
    }
    let _ = ssh.wait();
}

/// Stable id for the drop-relay directory, keyed on the launch arg — the exact
/// same `hash(launch arg)` that [`dtach_socket_path`] uses for the session
/// socket (so `relay-<id>` and `sessions/<id>.sock` share one id). The local
/// pump and the local launcher both compute it from the connection's launch
/// arg, and the launcher carries it to the long-lived remote croft as the
/// deterministic `CROFT_RELAY_KEY` env var (see `App::relay_dir`).
///
/// Two earlier schemes were wrong. A `pid-nanos` random id desynced on every
/// reconnect, because `dtach -A` reattaches the *already-running* croft whose
/// relay env froze at first launch while the pump minted a fresh id. Keying on
/// the croft's `workspace_root` desynced whenever the opened folder differed
/// from the launch identity — a no-path launch opens the login dir, the user
/// then opens a subfolder, and the pump (which only knows the launch arg) can't
/// see that divergence. The launch arg is the one identity both sides share and
/// that is invariant across an in-session workspace change and a self re-exec.
/// `DefaultHasher` has fixed SipHash keys, so the value is deterministic across
/// croft processes.
pub(crate) fn relay_session_id(launch_arg: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(launch_arg.as_bytes());
    format!("{:016x}", hasher.finish())
}
