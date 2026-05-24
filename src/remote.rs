use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::hash::Hasher;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTarget {
    pub alias: String,
    pub host_name: Option<String>,
    pub user: Option<String>,
}

impl RemoteTarget {
    pub fn detail(&self) -> String {
        match (&self.user, &self.host_name) {
            (Some(user), Some(host)) => format!("{user}@{host}"),
            (Some(user), None) => format!("{user}@{}", self.alias),
            (None, Some(host)) if host != &self.alias => host.clone(),
            _ => String::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshConfigState {
    entries: Vec<SshConfigEntryState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SshConfigEntryState {
    path: PathBuf,
    modified: Option<std::time::SystemTime>,
    len: Option<u64>,
}

pub fn ssh_config_state() -> SshConfigState {
    SshConfigState {
        entries: ssh_config_paths()
            .into_iter()
            .map(|path| {
                let meta = std::fs::metadata(&path).ok();
                SshConfigEntryState {
                    path,
                    modified: meta.as_ref().and_then(|m| m.modified().ok()),
                    len: meta.map(|m| m.len()),
                }
            })
            .collect(),
    }
}

#[derive(Default)]
struct HostBlock {
    aliases: Vec<String>,
    host_name: Option<String>,
    user: Option<String>,
}

pub fn discover_ssh_targets() -> Vec<RemoteTarget> {
    ssh_config_paths()
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .flat_map(|content| parse_ssh_config(&content))
        .fold(
            (BTreeSet::new(), Vec::new()),
            |(mut seen, mut targets), target| {
                if seen.insert(target.alias.clone()) {
                    targets.push(target);
                }
                (seen, targets)
            },
        )
        .1
}

fn ssh_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".ssh/config"));
    }
    paths
}

pub fn primary_ssh_config_path() -> Option<PathBuf> {
    ssh_config_paths().into_iter().next()
}

pub fn parse_ssh_config(input: &str) -> Vec<RemoteTarget> {
    let mut targets = Vec::new();
    let mut block = HostBlock::default();

    for raw in input.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let key = key.to_ascii_lowercase();
        if key == "host" {
            flush_block(&mut targets, &mut block);
            block.aliases = parts
                .filter(|alias| is_explicit_host_alias(alias))
                .map(ToString::to_string)
                .collect();
            continue;
        }
        if block.aliases.is_empty() {
            continue;
        }
        match key.as_str() {
            "hostname" => block.host_name = parts.next().map(ToString::to_string),
            "user" => block.user = parts.next().map(ToString::to_string),
            _ => {}
        }
    }
    flush_block(&mut targets, &mut block);
    targets
}

fn flush_block(targets: &mut Vec<RemoteTarget>, block: &mut HostBlock) {
    for alias in block.aliases.drain(..) {
        targets.push(RemoteTarget {
            alias,
            host_name: block.host_name.clone(),
            user: block.user.clone(),
        });
    }
    block.host_name = None;
    block.user = None;
}

fn strip_comment(line: &str) -> &str {
    line.split_once('#').map(|(head, _)| head).unwrap_or(line)
}

fn is_explicit_host_alias(alias: &str) -> bool {
    !alias.starts_with('!') && !alias.contains('*') && !alias.contains('?')
}

pub fn launch_croft(host: &str, path: Option<&str>) -> Result<()> {
    launch_croft_with(host, path, None)
}

/// Run any required remote-install work inside the current process,
/// streaming all subprocess stdout/stderr lines through `log_tx` so the
/// in-TUI connect dialog can render them in its log panel. Returns the
/// adopted master untouched so the caller can hand it to
/// `launch_only` once it has surrendered the alt-screen.
pub fn install_only_streaming(
    adopted: AdoptedMaster,
    log_tx: std::sync::mpsc::Sender<String>,
) -> Result<AdoptedMaster> {
    let host_label = adopted.host.clone();
    let _ = log_tx.send(format!("Adopting control socket for {host_label}…"));
    let ssh = SshControl::adopt(adopted.clone());
    let _ = log_tx.send("Hashing local source tree…".to_string());
    let local_stamp = local_source_stamp()?;
    let _ = log_tx.send(format!("Local source stamp: {local_stamp}"));
    let _ = log_tx.send(format!("Checking installed croft version on {host_label}…"));
    if !remote_install_needed(&ssh, &local_stamp)? {
        let _ = log_tx.send(format!("Croft on {host_label} is already up to date."));
        std::mem::forget(ssh);
        return Ok(adopted);
    }
    let _ = log_tx.send(format!("Installing/updating Croft on {host_label}…"));
    install_remote_croft_streaming(&ssh, &local_stamp, &log_tx)?;
    let _ = log_tx.send("Install complete.".to_string());
    std::mem::forget(ssh);
    Ok(adopted)
}

/// Counterpart to `install_only_streaming`: skips the install check and
/// runs the actual remote croft. Must be called only after the terminal
/// has been returned to cooked mode and the alt-screen surrendered, since
/// the spawned ssh shares stdin/stdout/stderr with the user's terminal.
pub fn launch_only(adopted: AdoptedMaster, path: Option<&str>) -> Result<()> {
    let ssh = SshControl::adopt(adopted);
    let pump = match DropPump::start(&ssh) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("Drag-drop relay disabled: {e}");
            None
        }
    };
    let env = pump.as_ref().map(DropPump::remote_env).unwrap_or_default();
    let status = run_remote_croft(&ssh, path, &env)?;
    if status.success() {
        return Ok(());
    }
    if status.code() == Some(127) {
        eprintln!("Croft is not installed on {}; bootstrapping...", ssh.host);
        let stamp = local_source_stamp()?;
        install_remote_croft(&ssh, &stamp)?;
        let status = run_remote_croft(&ssh, path, &env)?;
        if status.success() {
            return Ok(());
        }
        anyhow::bail!("ssh exited with {status}");
    }
    anyhow::bail!("ssh exited with {status}");
}

fn run_command_streaming(
    mut cmd: Command,
    log_tx: &std::sync::mpsc::Sender<String>,
) -> Result<std::process::ExitStatus> {
    use std::io::{BufRead, BufReader};
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
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

fn install_remote_croft_streaming(
    ssh: &SshControl,
    source_stamp: &str,
    log_tx: &std::sync::mpsc::Sender<String>,
) -> Result<()> {
    match try_local_cross_install_streaming(ssh, source_stamp, log_tx) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(e) => {
            let _ = log_tx.send(format!(
                "Local cross-build skipped ({e}); falling back to remote cargo install"
            ));
        }
    }
    let _ = log_tx.send("Syncing source tree to remote…".to_string());
    sync_local_source_to_remote_streaming(ssh, log_tx)?;
    let _ = log_tx.send(
        "Running cargo install on remote (first time can take several minutes)…".to_string(),
    );
    let mut cmd = ssh.command();
    cmd.arg(&ssh.host).arg(remote_install_command(source_stamp));
    let status =
        run_command_streaming(cmd, log_tx).context("installing croft on remote")?;
    if !status.success() {
        anyhow::bail!("remote croft install failed with {status}");
    }
    Ok(())
}

fn try_local_cross_install_streaming(
    ssh: &SshControl,
    source_stamp: &str,
    log_tx: &std::sync::mpsc::Sender<String>,
) -> Result<bool> {
    if !cross_compile_available() {
        return Ok(false);
    }
    let Some(triple) = remote_target_triple(ssh)? else {
        return Ok(false);
    };
    if !rust_target_installed(triple) {
        let _ = log_tx.send(format!(
            "Local cross-build skipped: rustup target `{triple}` missing (run `rustup target add {triple}` once to enable the fast path)"
        ));
        return Ok(false);
    }

    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let _ = log_tx.send(format!("Cross-compiling croft locally for {triple}…"));
    let mut zigbuild = Command::new("cargo");
    zigbuild
        .args([
            "zigbuild",
            "--profile",
            "remote-fast",
            "--locked",
            "--bin",
            "croft",
            "--target",
            triple,
        ])
        .current_dir(&source);
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

    let mut mkdir = ssh.command();
    mkdir
        .arg(&ssh.host)
        .arg("mkdir -p \"$HOME/.cargo/bin\" \"$HOME/.cache/croft\"");
    let mkdir_status =
        run_command_streaming(mkdir, log_tx).context("creating remote install dirs")?;
    if !mkdir_status.success() {
        anyhow::bail!("remote mkdir exited with {mkdir_status}");
    }

    let ssh_e = format!(
        "ssh -S {} -o ControlMaster=no",
        shell_quote_for_e_arg(&ssh.socket_path),
    );
    let dest = format!("{}:.cargo/bin/croft.new", ssh.host);
    let _ = log_tx.send(format!("Rsyncing binary to {dest}…"));
    let mut rsync = Command::new("rsync");
    rsync
        .args(["-az", "--checksum", "-e"])
        .arg(&ssh_e)
        .arg(&binary)
        .arg(&dest);
    let rsync_status =
        run_command_streaming(rsync, log_tx).context("rsyncing croft binary to remote")?;
    if !rsync_status.success() {
        anyhow::bail!("rsync exited with {rsync_status}");
    }

    let activate = format!(
        "chmod 755 \"$HOME/.cargo/bin/croft.new\" && mv \"$HOME/.cargo/bin/croft.new\" \"$HOME/.cargo/bin/croft\" && printf %s {} > \"$HOME/.cache/croft/install-stamp\"",
        shell_quote(source_stamp)
    );
    let mut act = ssh.command();
    act.arg(&ssh.host).arg(activate);
    let act_status =
        run_command_streaming(act, log_tx).context("activating remote croft binary")?;
    if !act_status.success() {
        anyhow::bail!("remote activation exited with {act_status}");
    }
    let _ = log_tx.send("Installed croft on remote via local cross-build.".to_string());
    Ok(true)
}

fn sync_local_source_to_remote_streaming(
    ssh: &SshControl,
    log_tx: &std::sync::mpsc::Sender<String>,
) -> Result<()> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut mkdir = ssh.command();
    mkdir
        .arg(&ssh.host)
        .arg("mkdir -p \"$HOME/.cache/croft/source\"");
    let mkdir_status =
        run_command_streaming(mkdir, log_tx).context("creating remote source dir")?;
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
    let mut rsync = Command::new("rsync");
    rsync
        .args([
            "-a",
            "-z",
            "--delete",
            "--exclude=target",
            "--exclude=.git",
            "--exclude=.DS_Store",
            "--exclude=assets/.DS_Store",
            "-e",
        ])
        .arg(&ssh_e)
        .arg(&source_arg)
        .arg(&dest);
    let status = run_command_streaming(rsync, log_tx).context("running rsync to remote")?;
    if !status.success() {
        anyhow::bail!("rsync exited with {status}");
    }
    Ok(())
}

/// Same as `launch_croft` but reuses an already-established SSH ControlMaster
/// socket so the in-TUI auth flow (PTY-driven password dialog) doesn't need
/// to prompt the user a second time when the post-quit path hands off the
/// connection.
pub fn launch_croft_with(
    host: &str,
    path: Option<&str>,
    adopted: Option<AdoptedMaster>,
) -> Result<()> {
    println!("Connecting to {host}...");
    let ssh = match adopted {
        Some(a) => SshControl::adopt(a),
        None => SshControl::start(host)?,
    };
    let local_stamp = local_source_stamp()?;
    if remote_install_needed(&ssh, &local_stamp)? {
        println!("Installing/updating Croft on {host}...");
        install_remote_croft(&ssh, &local_stamp)?;
    }
    let pump = match DropPump::start(&ssh) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("Drag-drop relay disabled: {e}");
            None
        }
    };
    let env = pump.as_ref().map(DropPump::remote_env).unwrap_or_default();
    let status = run_remote_croft(&ssh, path, &env)?;
    if status.success() {
        return Ok(());
    }
    if status.code() == Some(127) {
        println!("Croft is not installed on {host}; bootstrapping from local source...");
        install_remote_croft(&ssh, &local_stamp)?;
        println!("Reconnecting to {host}...");
        let status = run_remote_croft(&ssh, path, &env)?;
        if status.success() {
            return Ok(());
        }
        anyhow::bail!("ssh exited with {status}");
    }
    anyhow::bail!("ssh exited with {status}");
}

pub struct SshControl {
    host: String,
    socket_dir: PathBuf,
    socket_path: PathBuf,
}

/// Hand-off bundle for an already-established SSH ControlMaster, used by
/// the in-TUI password dialog to avoid re-prompting the user when the
/// post-quit flow takes over the connection.
#[derive(Clone)]
pub struct AdoptedMaster {
    pub host: String,
    pub socket_dir: PathBuf,
    pub socket_path: PathBuf,
}

impl SshControl {
    pub fn adopt(a: AdoptedMaster) -> Self {
        Self {
            host: a.host,
            socket_dir: a.socket_dir,
            socket_path: a.socket_path,
        }
    }

    fn start(host: &str) -> Result<Self> {
        let socket_dir = ssh_control_dir()?;
        std::fs::create_dir_all(&socket_dir)
            .with_context(|| format!("creating {}", socket_dir.display()))?;
        let socket_path = socket_dir.join("ctl");
        let status = Command::new("ssh")
            .arg("-M")
            .arg("-S")
            .arg(&socket_path)
            .arg("-f")
            .arg("-N")
            .arg("-T")
            .arg(host)
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

    fn command(&self) -> Command {
        let mut command = Command::new("ssh");
        command
            .arg("-S")
            .arg(&self.socket_path)
            .arg("-o")
            .arg("ControlMaster=no")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-o")
            .arg("ServerAliveInterval=10")
            .arg("-o")
            .arg("ServerAliveCountMax=3");
        command
    }
}

impl Drop for SshControl {
    fn drop(&mut self) {
        let _ = Command::new("ssh")
            .arg("-S")
            .arg(&self.socket_path)
            .arg("-O")
            .arg("exit")
            .arg(&self.host)
            .status();
        let _ = std::fs::remove_dir_all(&self.socket_dir);
    }
}

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
struct DropPump {
    inbox_dir: String,
    requests_log: String,
    stop: Arc<AtomicBool>,
    tail: Option<Child>,
    handle: Option<JoinHandle<()>>,
}

impl DropPump {
    fn start(ssh: &SshControl) -> Result<Self> {
        let id = relay_session_id();
        // The child croft process will see CROFT_DROP_RELAY_LOG /
        // CROFT_DROP_RELAY_INBOX as plain string env vars and call
        // open() on them directly, so the path must be absolute and
        // already-expanded by the remote shell. Resolve $HOME once on
        // the remote and capture the literal absolute path.
        let resolve = format!(
            "set -e; \
             RELAY=\"$HOME/.cache/croft/relay-{id}\"; \
             INBOX=\"$RELAY/inbox\"; \
             LOG=\"$RELAY/requests.log\"; \
             mkdir -p \"$INBOX\"; \
             : > \"$LOG\"; \
             printf '%s\\n%s\\n' \"$INBOX\" \"$LOG\""
        );
        let output = ssh
            .command()
            .arg(&ssh.host)
            .arg(&resolve)
            .stderr(Stdio::inherit())
            .output()
            .context("preparing remote drop relay")?;
        if !output.status.success() {
            anyhow::bail!("remote relay setup failed with {}", output.status);
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
        let mut tail = Command::new("ssh")
            .arg("-S")
            .arg(&ssh.socket_path)
            .arg("-o")
            .arg("ControlMaster=no")
            .arg(&ssh.host)
            .arg(format!(
                "exec tail -F -n 0 {} 2>/dev/null",
                shell_quote(&requests_log)
            ))
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
            inbox_dir,
            requests_log,
            stop,
            tail: Some(tail),
            handle: Some(handle),
        })
    }

    fn remote_env(&self) -> Vec<(String, String)> {
        vec![
            (String::from("CROFT_DROP_RELAY_LOG"), self.requests_log.clone()),
            (String::from("CROFT_DROP_RELAY_INBOX"), self.inbox_dir.clone()),
        ]
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

fn run_pump(
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
        match parse_relay_request(&line) {
            Some(RelayRequest::Pull { id, src }) => {
                handle_pull_request(&host, &socket, &inbox_dir, &id, &src);
            }
            Some(RelayRequest::Clipboard { id }) => {
                handle_clipboard_request(&host, &socket, &inbox_dir, &id);
            }
            Some(RelayRequest::Open { id, url }) => {
                handle_open_request(&host, &socket, &inbox_dir, &id, &url);
            }
            None => {}
        }
    }
}

fn handle_clipboard_request(host: &str, socket: &Path, inbox_dir: &str, request_id: &str) {
    let dest_dir = format!("{inbox_dir}/{request_id}");
    let mkdir = format!("mkdir -p {}", shell_quote(&dest_dir));
    let mkdir_ok = Command::new("ssh")
        .arg("-S")
        .arg(socket)
        .arg("-o")
        .arg("ControlMaster=no")
        .arg(host)
        .arg(&mkdir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !mkdir_ok {
        write_relay_err(host, socket, inbox_dir, request_id, "remote mkdir failed");
        return;
    }
    let payload = match crate::clipboard::read_string() {
        Some(s) => s,
        None => {
            write_relay_err(host, socket, inbox_dir, request_id, "local clipboard unavailable");
            return;
        }
    };
    let remote_cmd = format!(
        "cat > {dst}",
        dst = shell_quote(&format!("{dest_dir}/clipboard.txt")),
    );
    let mut ssh_recv = match Command::new("ssh")
        .arg("-S")
        .arg(socket)
        .arg("-o")
        .arg("ControlMaster=no")
        .arg(host)
        .arg(&remote_cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            write_relay_err(
                host,
                socket,
                inbox_dir,
                request_id,
                &format!("clipboard relay spawn failed: {e}"),
            );
            return;
        }
    };
    if let Some(mut stdin) = ssh_recv.stdin.take() {
        use std::io::Write as _;
        let _ = stdin.write_all(payload.as_bytes());
    }
    let status = ssh_recv.wait();
    match status {
        Ok(s) if s.success() => {
            let touch = format!("touch {}/.ok", shell_quote(&dest_dir));
            let _ = Command::new("ssh")
                .arg("-S")
                .arg(socket)
                .arg("-o")
                .arg("ControlMaster=no")
                .arg(host)
                .arg(&touch)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        Ok(s) => write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            &format!("clipboard relay ssh exited {s}"),
        ),
        Err(e) => write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            &format!("clipboard relay wait failed: {e}"),
        ),
    }
}

enum RelayRequest {
    Pull { id: String, src: String },
    Clipboard { id: String },
    Open { id: String, url: String },
}

fn parse_relay_request(line: &str) -> Option<RelayRequest> {
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
        "open" => {
            let url = parts.next()?.to_string();
            if url.is_empty() {
                return None;
            }
            Some(RelayRequest::Open { id, url })
        }
        _ => None,
    }
}

/// Validate a URL we're about to hand to the local Mac's `open(1)`. We
/// only accept the schemes the welcome panel ever produces — http, https,
/// mailto — so a hostile remote can't smuggle `file://` or shell metachars
/// through the relay log.
fn url_is_safe_to_open(url: &str) -> bool {
    let lower: String = url.chars().take(16).collect::<String>().to_ascii_lowercase();
    let scheme_ok = lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:");
    if !scheme_ok {
        return false;
    }
    !url.chars().any(|c| {
        c == '\0' || c == '\n' || c == '\r' || c == '\t'
    })
}

fn handle_pull_request(
    host: &str,
    socket: &Path,
    inbox_dir: &str,
    request_id: &str,
    src: &str,
) {
    let src_path = PathBuf::from(src);
    let dest_dir = format!("{inbox_dir}/{request_id}");
    if !src_path.exists() {
        write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            &format!("local file not found: {}", src_path.display()),
        );
        return;
    }
    let Some(parent) = src_path.parent() else {
        write_relay_err(host, socket, inbox_dir, request_id, "source has no parent");
        return;
    };
    let Some(basename) = src_path.file_name() else {
        write_relay_err(host, socket, inbox_dir, request_id, "source has no basename");
        return;
    };
    // Pipe a tar of the source through ssh into a remote tar -x. This
    // copies file or directory equivalently in one round trip and
    // sidesteps OpenSSH 9.x scp/sftp's realpath check on the
    // destination, which kept failing on freshly-mkdir'd dirs.
    let mut tar = match Command::new("tar")
        .env("COPYFILE_DISABLE", "1")
        .arg("-c")
        .arg("-f")
        .arg("-")
        .arg("-C")
        .arg(parent)
        .arg(basename)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            write_relay_err(
                host,
                socket,
                inbox_dir,
                request_id,
                &format!("local tar spawn failed: {e}"),
            );
            return;
        }
    };
    let tar_stdout = match tar.stdout.take() {
        Some(s) => s,
        None => {
            let _ = tar.wait();
            write_relay_err(host, socket, inbox_dir, request_id, "tar stdout missing");
            return;
        }
    };
    let remote_cmd = format!(
        "set -e; mkdir -p {dir} && tar -x -f - -C {dir}",
        dir = shell_quote(&dest_dir),
    );
    let ssh_recv = match Command::new("ssh")
        .arg("-S")
        .arg(socket)
        .arg("-o")
        .arg("ControlMaster=no")
        .arg(host)
        .arg(&remote_cmd)
        .stdin(Stdio::from(tar_stdout))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tar.wait();
            write_relay_err(
                host,
                socket,
                inbox_dir,
                request_id,
                &format!("ssh recv spawn failed: {e}"),
            );
            return;
        }
    };
    let ssh_out = ssh_recv.wait_with_output();
    let tar_out = tar.wait_with_output();
    match (tar_out, ssh_out) {
        (Ok(t), Ok(s)) if t.status.success() && s.status.success() => {
            let touch = format!("touch {}/.ok", shell_quote(&dest_dir));
            let _ = Command::new("ssh")
                .arg("-S")
                .arg(socket)
                .arg("-o")
                .arg("ControlMaster=no")
                .arg(host)
                .arg(&touch)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        (Ok(t), Ok(s)) => {
            let mut detail = format!("tar exited {}, ssh exited {}", t.status, s.status);
            let tar_err = String::from_utf8_lossy(&t.stderr);
            let ssh_err = String::from_utf8_lossy(&s.stderr);
            if !tar_err.trim().is_empty() {
                detail.push_str(" | tar: ");
                detail.push_str(tar_err.trim());
            }
            if !ssh_err.trim().is_empty() {
                detail.push_str(" | remote: ");
                detail.push_str(ssh_err.trim());
            }
            write_relay_err(host, socket, inbox_dir, request_id, &detail);
        }
        (Err(e), _) | (_, Err(e)) => write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            &format!("relay pipe wait failed: {e}"),
        ),
    }
}

fn handle_open_request(
    host: &str,
    socket: &Path,
    inbox_dir: &str,
    request_id: &str,
    url: &str,
) {
    if !url_is_safe_to_open(url) {
        write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            "rejected: URL scheme must be http/https/mailto and contain no control chars",
        );
        return;
    }
    let dest_dir = format!("{inbox_dir}/{request_id}");
    let mkdir = format!("mkdir -p {}", shell_quote(&dest_dir));
    let mkdir_ok = Command::new("ssh")
        .arg("-S")
        .arg(socket)
        .arg("-o")
        .arg("ControlMaster=no")
        .arg(host)
        .arg(&mkdir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !mkdir_ok {
        write_relay_err(host, socket, inbox_dir, request_id, "remote mkdir failed");
        return;
    }
    let result = if cfg!(target_os = "macos") {
        Command::new("open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else if cfg!(target_os = "linux") {
        Command::new("xdg-open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        Command::new("open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    };
    match result {
        Ok(s) if s.success() => {
            let touch = format!("touch {}/.ok", shell_quote(&dest_dir));
            let _ = Command::new("ssh")
                .arg("-S")
                .arg(socket)
                .arg("-o")
                .arg("ControlMaster=no")
                .arg(host)
                .arg(&touch)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        Ok(s) => write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            &format!("local open(1) exited {s}"),
        ),
        Err(e) => write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            &format!("local open(1) spawn failed: {e}"),
        ),
    }
}

fn write_relay_err(
    host: &str,
    socket: &Path,
    inbox_dir: &str,
    request_id: &str,
    message: &str,
) {
    let dest_dir = format!("{inbox_dir}/{request_id}");
    let err_path = format!("{dest_dir}/.err");
    let cmd = format!(
        "mkdir -p {dir} && printf %s {msg} > {err}",
        dir = shell_quote(&dest_dir),
        msg = shell_quote(message),
        err = shell_quote(&err_path),
    );
    let _ = Command::new("ssh")
        .arg("-S")
        .arg(socket)
        .arg("-o")
        .arg("ControlMaster=no")
        .arg(host)
        .arg(&cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn relay_session_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{now}", std::process::id())
}

pub fn ssh_control_dir() -> Result<PathBuf> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_millis();
    Ok(std::env::temp_dir().join(format!(
        "croft-ssh-{}-{now}",
        std::process::id()
    )))
}

fn run_remote_croft(
    ssh: &SshControl,
    path: Option<&str>,
    env: &[(String, String)],
) -> Result<ExitStatus> {
    let mut command = ssh.command();
    command
        .arg("-tt")
        .arg(&ssh.host)
        .arg(remote_croft_command(path, env));
    command.status().context("starting ssh")
}

fn install_remote_croft(ssh: &SshControl, source_stamp: &str) -> Result<()> {
    // Fast path: cross-compile a static musl binary on the local Mac and
    // rsync it directly into the remote's ~/.cargo/bin. Skips the
    // crates.io index update, the dependency walk, and the release-mode
    // codegen+link of the croft crate on the remote. Falls back to the
    // legacy source-rsync + `cargo install` path when the tooling isn't
    // present (zig + cargo-zigbuild + the matching rust target), when
    // we can't detect the remote arch, or when the build fails for any
    // reason — the user sees a one-line note and the slower install
    // continues so the connect succeeds.
    match try_local_cross_install(ssh, source_stamp) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(e) => {
            eprintln!(
                "Local cross-build failed ({e}); falling back to remote `cargo install`"
            );
        }
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

fn cross_compile_available() -> bool {
    Command::new("cargo")
        .args(["zigbuild", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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
    let Ok(output) = Command::new("rustup").args(["target", "list", "--installed"]).output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.trim() == triple)
}

fn try_local_cross_install(ssh: &SshControl, source_stamp: &str) -> Result<bool> {
    if !cross_compile_available() {
        return Ok(false);
    }
    let Some(triple) = remote_target_triple(ssh)? else {
        return Ok(false);
    };
    if !rust_target_installed(triple) {
        eprintln!(
            "Local cross-build skipped: rustup target `{triple}` not installed (run `rustup target add {triple}` once to enable the fast path)"
        );
        return Ok(false);
    }

    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    println!("Cross-compiling croft locally for {triple}...");
    let status = Command::new("cargo")
        .args([
            "zigbuild",
            "--profile",
            "remote-fast",
            "--locked",
            "--bin",
            "croft",
            "--target",
            triple,
        ])
        .current_dir(&source)
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
    let activate = format!(
        "chmod 755 \"$HOME/.cargo/bin/croft.new\" && mv \"$HOME/.cargo/bin/croft.new\" \"$HOME/.cargo/bin/croft\" && printf %s {} > \"$HOME/.cache/croft/install-stamp\"",
        shell_quote(source_stamp)
    );
    let activate_status = ssh
        .command()
        .arg(&ssh.host)
        .arg(activate)
        .status()
        .context("activating remote croft binary")?;
    if !activate_status.success() {
        anyhow::bail!("remote activation exited with {activate_status}");
    }
    println!("Installed croft on remote via local cross-build.");
    Ok(true)
}

pub fn remote_croft_command(path: Option<&str>, env: &[(String, String)]) -> String {
    remote_croft_command_for_terminal(
        path,
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        env,
    )
}

fn remote_croft_command_for_terminal(
    path: Option<&str>,
    term_program: Option<&str>,
    env: &[(String, String)],
) -> String {
    let mut prefix = String::new();
    if let Some(term_program) =
        term_program.filter(|value| crate::iterm2_inline::is_iterm2_term_program(Some(value)))
    {
        prefix.push_str("export CROFT_FORCE_INLINE_IMAGES=1 TERM_PROGRAM=");
        prefix.push_str(&shell_quote(term_program));
        prefix.push_str("; ");
    }
    for (k, v) in env {
        prefix.push_str("export ");
        prefix.push_str(k);
        prefix.push('=');
        prefix.push_str(&shell_quote(v));
        prefix.push_str("; ");
    }
    prefix.push_str("export PATH=\"$HOME/.cargo/bin:$PATH\"; command -v croft >/dev/null 2>&1 || { echo 'croft not found on remote PATH' >&2; exit 127; }; exec croft");
    match path.filter(|p| !p.is_empty()) {
        Some(path) => format!("{prefix} {}", shell_quote(path)),
        None => prefix,
    }
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
        .arg("mkdir -p \"$HOME/.cache/croft/source\"")
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
        .args([
            "-a",
            "-z",
            "--delete",
            "--exclude=target",
            "--exclude=.git",
            "--exclude=.DS_Store",
            "--exclude=assets/.DS_Store",
            "-e",
        ])
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
        .args([
            "-czf",
            "-",
            "--exclude=.git",
            "--exclude=target",
            "--exclude=.DS_Store",
            "--exclude=assets/.DS_Store",
            "-C",
        ])
        .arg(&source)
        .arg(".")
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("packing {}", source.display()))?;

    let tar_stdout = tar.stdout.take().context("opening tar stdout")?;
    let mut remote = ssh
        .command()
        .arg(&ssh.host)
        .arg(
            "mkdir -p \"$HOME/.cache/croft/source\" && \
             tar -xzf - -C \"$HOME/.cache/croft/source\"",
        )
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

/// Quote a path for embedding inside rsync's `-e "ssh -S <path> ..."`
/// argument. rsync executes the remote-shell string through `/bin/sh -c`,
/// so any spaces or shell metacharacters in the control-socket path would
/// break the invocation. The control socket lives under
/// `std::env::temp_dir()` which is normally space-free, but the socket
/// path is process-id + millisecond-stamped so quote defensively rather
/// than trust the format.
fn shell_quote_for_e_arg(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    if s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-')) {
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
if ! command -v cargo >/dev/null 2>&1; then
  if ! command -v curl >/dev/null 2>&1; then
    if command -v apt-get >/dev/null 2>&1 && [ "$(id -u)" = "0" ]; then
      export DEBIAN_FRONTEND=noninteractive
      apt-get update
      apt-get install -y curl ca-certificates build-essential pkg-config
    else
      echo 'cargo and curl are missing; install Rust/Cargo on the remote and retry' >&2
      exit 127
    fi
  fi
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  . "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"
cargo install --path "$HOME/.cache/croft/source" --force --locked
mkdir -p "$HOME/.cache/croft"
printf %s {stamp} > "$HOME/.cache/croft/install-stamp"
"#,
        stamp = shell_quote(source_stamp)
    )
}

fn local_source_stamp() -> Result<String> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_source_dir(&source, &source, &mut hasher)?;
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
        if matches!(name.as_ref(), ".git" | "target" | ".DS_Store") {
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
            hasher.write(&std::fs::read(&path).with_context(|| {
                format!("reading {}", path.display())
            })?);
        }
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
fn ssh_control_socket_path_for_test(dir: &Path) -> PathBuf {
    dir.join("ctl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ssh_config_extracts_explicit_hosts() {
        let targets = parse_ssh_config(
            r#"
Host *
  User nobody

Host genesis-cloud github.com
  HostName 10.0.0.2
  User ubuntu

Host !blocked *.internal
  User root
"#,
        );
        assert_eq!(
            targets,
            vec![
                RemoteTarget {
                    alias: String::from("genesis-cloud"),
                    host_name: Some(String::from("10.0.0.2")),
                    user: Some(String::from("ubuntu")),
                },
                RemoteTarget {
                    alias: String::from("github.com"),
                    host_name: Some(String::from("10.0.0.2")),
                    user: Some(String::from("ubuntu")),
                },
            ]
        );
    }

    #[test]
    fn remote_croft_command_quotes_paths() {
        assert_eq!(
            remote_croft_command_for_terminal(Some("/tmp/it's here"), None, &[]),
            "export PATH=\"$HOME/.cargo/bin:$PATH\"; command -v croft >/dev/null 2>&1 || { echo 'croft not found on remote PATH' >&2; exit 127; }; exec croft '/tmp/it'\"'\"'s here'"
        );
    }

    #[test]
    fn remote_croft_command_forwards_supported_terminal_program() {
        assert_eq!(
            remote_croft_command_for_terminal(None, Some("iTerm.app"), &[]),
            "export CROFT_FORCE_INLINE_IMAGES=1 TERM_PROGRAM='iTerm.app'; export PATH=\"$HOME/.cargo/bin:$PATH\"; command -v croft >/dev/null 2>&1 || { echo 'croft not found on remote PATH' >&2; exit 127; }; exec croft"
        );
    }

    #[test]
    fn remote_croft_command_exports_drop_relay_env() {
        let env = vec![
            (String::from("CROFT_DROP_RELAY_LOG"), String::from("/tmp/r/log")),
            (String::from("CROFT_DROP_RELAY_INBOX"), String::from("/tmp/r/inbox")),
        ];
        let command = remote_croft_command_for_terminal(None, None, &env);
        assert!(command.contains("export CROFT_DROP_RELAY_LOG='/tmp/r/log'"));
        assert!(command.contains("export CROFT_DROP_RELAY_INBOX='/tmp/r/inbox'"));
    }

    #[test]
    fn parse_relay_request_pulls_have_id_and_path() {
        match super::parse_relay_request("pull\tabc-123\t/Users/v/foo.txt") {
            Some(super::RelayRequest::Pull { id, src }) => {
                assert_eq!(id, "abc-123");
                assert_eq!(src, "/Users/v/foo.txt");
            }
            other => panic!("expected Pull, got {:?}", matches!(other, Some(_))),
        }
    }

    #[test]
    fn parse_relay_request_open_extracts_id_and_url() {
        match super::parse_relay_request("open\topen-1\thttps://example.com/x") {
            Some(super::RelayRequest::Open { id, url }) => {
                assert_eq!(id, "open-1");
                assert_eq!(url, "https://example.com/x");
            }
            _ => panic!("expected Open variant"),
        }
    }

    #[test]
    fn url_is_safe_to_open_accepts_http_https_mailto() {
        assert!(super::url_is_safe_to_open("https://example.com/path"));
        assert!(super::url_is_safe_to_open("http://example.com"));
        assert!(super::url_is_safe_to_open("HTTPS://Example.COM"));
        assert!(super::url_is_safe_to_open("mailto:foo@example.com"));
    }

    #[test]
    fn url_is_safe_to_open_rejects_other_schemes_and_control_chars() {
        assert!(!super::url_is_safe_to_open("file:///etc/passwd"));
        assert!(!super::url_is_safe_to_open("javascript:alert(1)"));
        assert!(!super::url_is_safe_to_open("ssh://attacker"));
        assert!(!super::url_is_safe_to_open("https://example.com\nrm -rf /"));
        assert!(!super::url_is_safe_to_open("https://example.com\r\nx"));
        assert!(!super::url_is_safe_to_open("plaintext"));
    }

    #[test]
    fn parse_relay_request_clipboard_requires_id_only() {
        match super::parse_relay_request("clipboard\tclip-7") {
            Some(super::RelayRequest::Clipboard { id }) => assert_eq!(id, "clip-7"),
            _ => panic!("expected Clipboard variant"),
        }
    }

    #[test]
    fn parse_relay_request_rejects_malformed_lines() {
        assert!(super::parse_relay_request("ping\tabc\t/x").is_none());
        assert!(super::parse_relay_request("pull\t\t/x").is_none());
        assert!(super::parse_relay_request("pull\tabc\t").is_none());
        assert!(super::parse_relay_request("clipboard\t").is_none());
    }

    #[test]
    fn remote_install_command_installs_from_staged_source() {
        let command = remote_install_command("abc123");
        assert!(command.contains("cargo install --path \"$HOME/.cache/croft/source\""));
        assert!(command.contains("rustup.rs"));
        assert!(command.contains("printf %s 'abc123' > \"$HOME/.cache/croft/install-stamp\""));
    }

    #[test]
    fn arch_to_musl_triple_maps_every_arch_uname_reports_for_supported_targets() {
        assert_eq!(
            arch_to_musl_triple("x86_64"),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            arch_to_musl_triple("amd64"),
            Some("x86_64-unknown-linux-musl"),
            "BSD-style `uname -m` reports amd64 for the same machine class linux reports as x86_64; the fast-install path must accept both"
        );
        assert_eq!(
            arch_to_musl_triple("aarch64"),
            Some("aarch64-unknown-linux-musl")
        );
        assert_eq!(
            arch_to_musl_triple("arm64"),
            Some("aarch64-unknown-linux-musl"),
            "Apple Silicon Linux VMs report arm64; same triple as aarch64"
        );
    }

    #[test]
    fn arch_to_musl_triple_returns_none_for_unsupported_archs_so_caller_falls_back() {
        for arch in ["i686", "armv7l", "ppc64le", "riscv64", "mips", ""] {
            assert_eq!(
                arch_to_musl_triple(arch),
                None,
                "{arch} has no statically-known musl target in croft's bundled toolchain; caller must fall back to remote cargo install"
            );
        }
    }

    #[test]
    fn remote_install_check_uses_cargo_path_before_probe() {
        let command = remote_install_check_command();
        let path_pos = command.find("export PATH=\"$HOME/.cargo/bin:$PATH\"").unwrap();
        let probe_pos = command.find("command -v croft").unwrap();
        assert!(command.contains(". \"$HOME/.cargo/env\""));
        assert!(path_pos < probe_pos);
        assert!(command.contains("cat \"$HOME/.cache/croft/install-stamp\""));
    }

    #[test]
    fn shell_quote_for_e_arg_passes_alnum_paths_through() {
        assert_eq!(
            shell_quote_for_e_arg(Path::new("/tmp/croft-ssh-1234/ctl")),
            "/tmp/croft-ssh-1234/ctl",
        );
    }

    #[test]
    fn shell_quote_for_e_arg_quotes_paths_with_spaces() {
        let q = shell_quote_for_e_arg(Path::new("/Users/v a/croft/ctl"));
        assert_eq!(q, "'/Users/v a/croft/ctl'");
    }

    #[test]
    fn shell_quote_for_e_arg_escapes_single_quotes_inside_path() {
        let q = shell_quote_for_e_arg(Path::new("/tmp/it's/ctl"));
        // /tmp/it's → /tmp/it'\''s after the standard sh single-quote
        // escaping trick (close, escaped quote, reopen).
        assert_eq!(q, "'/tmp/it'\\''s/ctl'");
    }

    #[test]
    fn ssh_control_socket_path_is_short_and_inside_temp_dir() {
        let dir = std::env::temp_dir().join("croft-ssh-test");
        let socket = ssh_control_socket_path_for_test(&dir);
        assert!(socket.starts_with(&dir));
        assert_eq!(socket.file_name().and_then(|s| s.to_str()), Some("ctl"));
    }
}
