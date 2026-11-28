//! The SSH control master a remote session runs over: starting or adopting it, the
//! commands sent through its socket, and the socket's private directory.

use super::*;

/// Hand-off bundle for an already-established SSH ControlMaster, used by
/// the in-TUI password dialog to avoid re-prompting the user when the
/// post-quit flow takes over the connection.
#[derive(Clone)]
pub struct AdoptedMaster {
    pub host: String,
    pub socket_dir: PathBuf,
    pub socket_path: PathBuf,
}

/// Close a handed-off control master that nothing will use, ending its ssh
/// process and removing its socket's directory: an install that could not
/// start (#2605), or one that failed (#2610).
///
/// Both happen while croft's screen is up, so ssh's streams go nowhere:
/// inherited, its "Exit request sent." (or, with the master already gone,
/// "Control socket connect(…)") landed on top of croft's screen (#2715).
/// Dropping an [`SshControl`] keeps them, for the command-line paths that
/// run with croft's screen torn down.
pub fn close_master(a: AdoptedMaster) {
    let _ = Ssh::control(&a.socket_path, "exit")
        .arg(&a.host)
        .quiet()
        .into_command()
        .status();
    let _ = std::fs::remove_dir_all(&a.socket_dir);
}

/// `ssh -S <socket> -O exit <host>`: ask the master behind `socket` to end.
fn exit_request(socket_path: &Path, host: &str) -> Command {
    Ssh::control(socket_path, "exit").arg(host).into_command()
}

impl SshControl {
    pub fn adopt(a: AdoptedMaster) -> Self {
        Self {
            host: a.host,
            socket_dir: a.socket_dir,
            socket_path: a.socket_path,
        }
    }

    pub(super) fn start(host: &str) -> Result<Self> {
        let socket_dir = ssh_control_dir()?;
        let socket_path = socket_dir.join("ctl");
        // Each connect attempt is bounded, so a reconnect against a host that
        // is still asleep/offline fails fast instead of hanging, and the
        // master tears itself down promptly once the link goes away.
        let status = Ssh::session_master(&socket_path)
            .arg(host)
            .into_command()
            .status()
            .context("starting SSH control connection")?;
        if !status.success() {
            anyhow::bail!("ssh control connection exited with {status}");
        }
        Ok(Self {
            host: host.to_string(),
            socket_dir,
            socket_path,
        })
    }

    pub(super) fn command(&self) -> Command {
        ssh_socket_command(&self.socket_path, false)
    }

    /// A command for the background install path, which runs while the user is
    /// already attached. See [`ssh_socket_command`] for why `-n` is
    /// load-bearing here.
    pub(super) fn background_command(&self) -> Command {
        ssh_socket_command(&self.socket_path, true)
    }

    /// A background command that runs `script` on the remote, with the
    /// destination host where ssh expects it: BEFORE the command.
    ///
    /// Every caller needs that ordering and one of them omitted it, which does
    /// not fail loudly - ssh reads the first non-flag argument as the
    /// destination, so `mkdir -p ~/.config/croft` became the hostname, the
    /// connection failed, and the feature above it silently did nothing. The
    /// ordering lives here now so a caller cannot get it wrong.
    pub(super) fn background_shell(&self, script: &str) -> Command {
        let mut command = self.background_command();
        command.arg(&self.host).arg(script);
        command
    }
}

/// A background command whose output croft reads: `script` on `host` over
/// the control socket.
pub(super) fn ssh_socket_read(socket_path: &Path, host: &str, script: &str) -> Command {
    let mut command = ssh_socket_command(socket_path, true);
    // Background commands null all three streams; a reader needs stdout
    // back, or `output()` hands it an empty string (#1247).
    command.arg(host).arg(script).stdout(Stdio::piped());
    command
}

/// Build an ssh invocation over an existing control socket.
///
/// `background` marks a command that runs *while the remote session owns the
/// terminal*. Such a command must not touch any of the three standard streams:
/// ssh forwards the caller's stdin to the remote unless `-n` is given, so
/// without it two processes read() the same tty and every keystroke reaches
/// exactly one of them, and inherited stderr writes ssh diagnostics straight
/// onto the alt screen. The interactive session is the one caller that must
/// keep the terminal, so it passes `false`.
pub(super) fn ssh_socket_command(socket_path: &Path, background: bool) -> Command {
    let client = Ssh::client(socket_path);
    if background {
        client.background().into_command()
    } else {
        client.into_command()
    }
}

impl Drop for SshControl {
    fn drop(&mut self) {
        let _ = exit_request(&self.socket_path, &self.host).status();
        let _ = std::fs::remove_dir_all(&self.socket_dir);
    }
}

/// Run `cmd` on the remote over the existing master socket (no second auth, no
/// nested master). Used for the relay inbox bookkeeping (`mkdir`, sentinel
/// writes). Returns whether it exited zero.
pub(super) fn ssh_exec(host: &str, socket: &Path, cmd: &str) -> bool {
    Ssh::bare_client(socket)
        .arg(host)
        .arg(cmd)
        .quiet()
        .into_command()
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Create a fresh, owner-only directory for an SSH control socket and
/// return it. Created here, in one step, rather than named and later
/// `create_dir_all`ed: the name is predictable in a shared `/tmp`, and
/// `create_dir_all` accepted a directory another local user had made first,
/// who could then plant a `ctl` that reads as "authenticated" or swap in a
/// socket of their own. The create fails if the name already exists.
pub fn ssh_control_dir() -> Result<PathBuf> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_millis();
    // A counter too: two calls in one millisecond must not collide now that
    // an existing name is an error.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("croft-ssh-{}-{now}-{seq}", std::process::id()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
pub(super) fn ssh_control_socket_path_for_test(dir: &Path) -> PathBuf {
    dir.join("ctl")
}
