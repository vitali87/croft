use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::hash::Hasher;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

pub fn launch_croft(host: &str, path: Option<&str>) -> Result<RemoteOutcome> {
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
    can_launch_tx: std::sync::mpsc::Sender<()>,
) -> Result<AdoptedMaster> {
    let host_label = adopted.host.clone();
    // Mirror every line into ~/.cache/croft/install.log so the install
    // remains diagnosable after the connect dialog is gone.
    let log_tx = spawn_log_tee(install_log_path(), log_tx);
    let _ = log_tx.send(format!(
        "Install session for {host_label} (croft {})",
        env!("CARGO_PKG_VERSION")
    ));
    let _ = log_tx.send(format!("Adopting control socket for {host_label}…"));
    let ssh = SshControl::adopt(adopted.clone());
    let _ = log_tx.send("Hashing local source tree…".to_string());
    let local_stamp = local_source_stamp()?;
    let _ = log_tx.send(format!("Local source stamp: {local_stamp}"));
    let _ = log_tx.send(format!("Checking installed croft version on {host_label}…"));
    // If a croft is already on the remote, the user gets dropped into it
    // immediately and the (re)install proceeds in the background. The
    // running croft re-execs into the new binary once the stamp advances.
    let present = remote_croft_present(&ssh).unwrap_or(false);
    if present {
        let _ = can_launch_tx.send(());
    }
    if !remote_install_needed(&ssh, &local_stamp)? {
        let _ = log_tx.send(format!("Croft on {host_label} is already up to date."));
        if !present {
            let _ = can_launch_tx.send(());
        }
        std::mem::forget(ssh);
        return Ok(adopted);
    }
    // Mark the remote as updating before the local cross-compile so the
    // running croft shows "Updating…" for the whole build+ship, not just
    // the final remote activation.
    mark_remote_updating(&ssh);
    // The user may already be inside the remote croft over the interactive
    // master. Route every bulk byte of this install through a bulk lane so
    // the transfer never queues ahead of their keystrokes in the shared
    // TCP stream (SSH multiplexing is head-of-line blocking).
    let bulk = crate::remote_bulk::establish(&ssh.host, &ssh.socket_path, |msg| {
        let _ = log_tx.send(msg);
    });
    let _ = log_tx.send(format!("Installing/updating Croft on {host_label}…"));
    if let Err(e) = install_remote_croft_streaming(&ssh, &bulk.lane, &local_stamp, &log_tx) {
        clear_remote_updating(&ssh);
        std::mem::forget(ssh);
        return Err(e);
    }
    let _ = log_tx.send("Install complete.".to_string());
    std::mem::forget(ssh);
    Ok(adopted)
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

pub const DROP_TO_LOCAL_EXIT_CODE: i32 = 88;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteOutcome {
    ReturnToLocal,
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteStatusClass {
    ReturnToLocal,
    Exited,
    NotInstalled,
    Failed,
}

fn classify_remote_status(code: Option<i32>) -> RemoteStatusClass {
    match code {
        Some(DROP_TO_LOCAL_EXIT_CODE) => RemoteStatusClass::ReturnToLocal,
        Some(0) => RemoteStatusClass::Exited,
        Some(127) => RemoteStatusClass::NotInstalled,
        _ => RemoteStatusClass::Failed,
    }
}

fn outcome_or_bail(status: ExitStatus) -> Result<RemoteOutcome> {
    match classify_remote_status(status.code()) {
        RemoteStatusClass::ReturnToLocal => Ok(RemoteOutcome::ReturnToLocal),
        RemoteStatusClass::Exited => Ok(RemoteOutcome::Exited),
        _ => anyhow::bail!("ssh exited with {status}"),
    }
}

/// Counterpart to `install_only_streaming`: skips the install check and
/// runs the actual remote croft. Must be called only after the terminal
/// has been returned to cooked mode and the alt-screen surrendered, since
/// the spawned ssh shares stdin/stdout/stderr with the user's terminal.
pub fn launch_only(adopted: AdoptedMaster, path: Option<&str>) -> Result<RemoteOutcome> {
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
    if classify_remote_status(status.code()) == RemoteStatusClass::NotInstalled {
        eprintln!("Croft is not installed on {}; bootstrapping...", ssh.host);
        let stamp = local_source_stamp()?;
        install_remote_croft(&ssh, &stamp)?;
        let status = run_remote_croft(&ssh, path, &env)?;
        return outcome_or_bail(status);
    }
    outcome_or_bail(status)
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
    lane: &crate::remote_bulk::BulkLane,
    source_stamp: &str,
    log_tx: &std::sync::mpsc::Sender<String>,
) -> Result<()> {
    match try_local_cross_install_streaming(ssh, lane, source_stamp, log_tx) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(e) => {
            let _ = log_tx.send(format!(
                "Local cross-build skipped ({e}); falling back to remote cargo install"
            ));
        }
    }
    let _ = log_tx.send("Syncing source tree to remote…".to_string());
    sync_local_source_to_remote_streaming(ssh, lane, log_tx)?;
    let _ = log_tx
        .send("Running cargo install on remote (first time can take several minutes)…".to_string());
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

fn try_local_cross_install_streaming(
    ssh: &SshControl,
    lane: &crate::remote_bulk::BulkLane,
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
        "Cross-compiling croft locally for {triple} (niced, {jobs} jobs, fd limit {fd_limit})…"
    ));
    let mut zigbuild = Command::new("nice");
    zigbuild
        .args([
            "-n",
            "19",
            "cargo",
            "zigbuild",
            "--profile",
            "remote-fast",
            "--locked",
            "--jobs",
            &jobs,
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

    let dest = format!("{}:.cargo/bin/croft.new", ssh.host);
    let _ = log_tx.send(format!(
        "Rsyncing binary to {dest} (bulk lane, {} KB/s cap)…",
        lane.bwlimit_kbps()
    ));
    let rsync = ship_binary_rsync_command(lane, &ssh.socket_path, &binary, &dest);
    let rsync_status =
        run_command_streaming(rsync, log_tx).context("rsyncing croft binary to remote")?;
    if !rsync_status.success() {
        anyhow::bail!("rsync exited with {rsync_status}");
    }

    let mut act = ssh.command();
    act.arg(&ssh.host).arg(activate_command(source_stamp));
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
    lane: &crate::remote_bulk::BulkLane,
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

/// Same as `launch_croft` but reuses an already-established SSH ControlMaster
/// socket so the in-TUI auth flow (PTY-driven password dialog) doesn't need
/// to prompt the user a second time when the post-quit path hands off the
/// connection.
pub fn launch_croft_with(
    host: &str,
    path: Option<&str>,
    adopted: Option<AdoptedMaster>,
) -> Result<RemoteOutcome> {
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
    if classify_remote_status(status.code()) == RemoteStatusClass::NotInstalled {
        println!("Croft is not installed on {host}; bootstrapping from local source...");
        install_remote_croft(&ssh, &local_stamp)?;
        println!("Reconnecting to {host}...");
        let status = run_remote_croft(&ssh, path, &env)?;
        return outcome_or_bail(status);
    }
    outcome_or_bail(status)
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
            (
                String::from("CROFT_DROP_RELAY_LOG"),
                self.requests_log.clone(),
            ),
            (
                String::from("CROFT_DROP_RELAY_INBOX"),
                self.inbox_dir.clone(),
            ),
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
            write_relay_err(
                host,
                socket,
                inbox_dir,
                request_id,
                "local clipboard unavailable",
            );
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
    let lower: String = url
        .chars()
        .take(16)
        .collect::<String>()
        .to_ascii_lowercase();
    let scheme_ok = lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:");
    if !scheme_ok {
        return false;
    }
    !url.chars()
        .any(|c| c == '\0' || c == '\n' || c == '\r' || c == '\t')
}

fn handle_pull_request(host: &str, socket: &Path, inbox_dir: &str, request_id: &str, src: &str) {
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
        write_relay_err(
            host,
            socket,
            inbox_dir,
            request_id,
            "source has no basename",
        );
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

fn handle_open_request(host: &str, socket: &Path, inbox_dir: &str, request_id: &str, url: &str) {
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

fn write_relay_err(host: &str, socket: &Path, inbox_dir: &str, request_id: &str, message: &str) {
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
    Ok(std::env::temp_dir().join(format!("croft-ssh-{}-{now}", std::process::id())))
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
            eprintln!("Local cross-build failed ({e}); falling back to remote `cargo install`");
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

fn cross_compile_available() -> bool {
    // Probe the cargo-zigbuild binary directly: `cargo zigbuild --version`
    // is rejected by cargo-zigbuild >=0.22 (the `zigbuild` subcommand has
    // no `--version`, exit 2), which silently disabled the fast path and
    // forced every remote install onto the slow from-scratch `cargo
    // install`. `cargo-zigbuild --version` exits 0 when installed.
    Command::new("cargo-zigbuild")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Re-pin the croft workspace member in `Cargo.lock` to whatever `Cargo.toml`
/// now declares, immediately before a `--locked` cross-build.
///
/// Every behavioural change bumps the patch version in `Cargo.toml`, but
/// `cargo install --path .` never rewrites the on-disk lockfile - so the lock
/// drifts exactly one patch behind. `cargo zigbuild --locked` then refuses to
/// build, and the installer silently falls back to a minutes-long
/// from-scratch `cargo install` *on the remote host* (the thing the fast path
/// exists to avoid). `cargo update -p croft --offline` rewrites only croft's
/// own version line - it touches no dependency, needs no network, and so keeps
/// `--locked`'s real guarantee (a reproducible dependency graph) fully intact.
///
/// Best-effort: if the sync itself fails we log and still attempt the locked
/// build, preserving the old fall-back behaviour rather than blocking install.
fn sync_workspace_lock(source: &Path, log: impl Fn(String)) {
    match Command::new("cargo")
        .args(["update", "-p", "croft", "--offline"])
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

/// Build the rsync that ships the cross-built binary, routed and paced by
/// the bulk lane so it never queues ahead of the live session's keystrokes.
fn ship_binary_rsync_command(
    lane: &crate::remote_bulk::BulkLane,
    interactive_socket: &Path,
    binary: &Path,
    dest: &str,
) -> Command {
    let mut rsync = Command::new("rsync");
    rsync.args(["-az", "--checksum"]);
    rsync.args(lane.rsync_throttle_args());
    rsync.arg("-e").arg(lane.rsync_ssh_arg(interactive_socket));
    rsync.arg(binary).arg(dest);
    rsync
}

/// Build the rsync that mirrors the source tree to the remote for the
/// `cargo install` fallback, routed and paced by the bulk lane.
fn source_sync_rsync_command(
    lane: &crate::remote_bulk::BulkLane,
    interactive_socket: &Path,
    source_arg: &std::ffi::OsStr,
    dest: &str,
) -> Command {
    let mut rsync = Command::new("rsync");
    rsync.args([
        "-a",
        "-z",
        "--delete",
        "--exclude=target",
        "--exclude=.git",
        "--exclude=.DS_Store",
        "--exclude=assets/.DS_Store",
    ]);
    rsync.args(lane.rsync_throttle_args());
    rsync.arg("-e").arg(lane.rsync_ssh_arg(interactive_socket));
    rsync.arg(source_arg).arg(dest);
    rsync
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
    let Ok(output) = Command::new("rustup")
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
    sync_workspace_lock(&source, |msg| println!("{msg}"));
    raise_fd_limit();
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
    Ok(true)
}

pub fn remote_croft_command(path: Option<&str>, env: &[(String, String)]) -> String {
    remote_croft_command_for_terminal(
        path,
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        crate::iterm2_inline::detect_osk_auto(),
        env,
    )
}

fn remote_croft_command_for_terminal(
    path: Option<&str>,
    term_program: Option<&str>,
    term: Option<&str>,
    osk: bool,
    env: &[(String, String)],
) -> String {
    use crate::iterm2_inline::InlineImageProtocol;
    let mut prefix = String::from("export CROFT_REMOTE_AUTOUPDATE=1; ");
    // SSH from Termux does not forward TERMUX_VERSION, so the remote croft
    // can't detect the touch environment itself; carry the local on-screen
    // keyboard detection across the hop like the TERM_PROGRAM hint below.
    if osk {
        prefix.push_str("export CROFT_FORCE_OSK=1; ");
    }
    // The remote croft renders into *this* terminal over the SSH PTY, so it has
    // to use whichever inline-image protocol the local terminal speaks. SSH
    // forwards `TERM` but not `TERM_PROGRAM` (absent an `AcceptEnv` opt-in), so
    // export the hint the remote detector needs explicitly rather than relying
    // on the environment surviving the hop.
    match crate::iterm2_inline::inline_image_protocol_for(term_program, term) {
        InlineImageProtocol::ITerm2 => {
            // iTerm2/WezTerm implement OSC 1337 inline images + `SetColors`;
            // force them on and carry `TERM_PROGRAM` so the remote paints the
            // session background too.
            let tp = term_program.unwrap_or("iTerm.app");
            prefix.push_str("export CROFT_FORCE_INLINE_IMAGES=1 TERM_PROGRAM=");
            prefix.push_str(&shell_quote(tp));
            prefix.push_str("; ");
        }
        InlineImageProtocol::Kitty => {
            // Ghostty/Kitty render via the Kitty graphics protocol and paint the
            // background with explicit truecolor (no `SetColors`), so export
            // `TERM_PROGRAM=ghostty` only; do NOT force the OSC-1337 path.
            prefix.push_str("export TERM_PROGRAM=ghostty; ");
        }
        // Sixel is never returned by `inline_image_protocol_for` (it has no env
        // signal); the remote croft discovers it by running its own DA1 probe
        // against this same terminal over the SSH PTY, so no hint is forwarded.
        InlineImageProtocol::Sixel => {}
        InlineImageProtocol::None => {}
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

# Package installs need root. Run them directly when already root, otherwise
# via sudo if available. CROFT_SUDO is empty in both the root and the
# no-sudo case so the install commands stay identical.
if [ "$(id -u)" = "0" ]; then
  CROFT_SUDO=""
elif command -v sudo >/dev/null 2>&1; then
  CROFT_SUDO="sudo"
else
  CROFT_SUDO=""
fi

# Install system packages with whatever package manager the box ships.
croft_pkg_install() {{
  if command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    $CROFT_SUDO apt-get update
    $CROFT_SUDO apt-get install -y "$@"
  elif command -v dnf >/dev/null 2>&1; then
    $CROFT_SUDO dnf install -y "$@"
  elif command -v yum >/dev/null 2>&1; then
    $CROFT_SUDO yum install -y "$@"
  elif command -v apk >/dev/null 2>&1; then
    $CROFT_SUDO apk add "$@"
  elif command -v pacman >/dev/null 2>&1; then
    $CROFT_SUDO pacman -Sy --noconfirm "$@"
  elif command -v zypper >/dev/null 2>&1; then
    $CROFT_SUDO zypper install -y "$@"
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
    $CROFT_SUDO apt-get update
    $CROFT_SUDO apt-get install -y build-essential pkg-config
  elif command -v dnf >/dev/null 2>&1; then
    $CROFT_SUDO dnf install -y gcc make pkgconf-pkg-config
  elif command -v yum >/dev/null 2>&1; then
    $CROFT_SUDO yum install -y gcc make pkgconfig
  elif command -v apk >/dev/null 2>&1; then
    $CROFT_SUDO apk add build-base pkgconf
  elif command -v pacman >/dev/null 2>&1; then
    $CROFT_SUDO pacman -Sy --noconfirm base-devel pkgconf
  elif command -v zypper >/dev/null 2>&1; then
    $CROFT_SUDO zypper install -y gcc make pkg-config
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
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  . "$HOME/.cargo/env"
fi
export PATH="$HOME/.cargo/bin:$PATH"
# Ensure the C toolchain before compiling, regardless of whether cargo was
# already installed: a box can have rustup/cargo but no cc.
croft_ensure_build_toolchain
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
if pgrep -x croft >/dev/null 2>&1; then
  CROFT_JOBS=1
fi
CROFT_NICE=""
if command -v nice >/dev/null 2>&1; then CROFT_NICE="nice -n 19"; fi
CROFT_IONICE=""
if command -v ionice >/dev/null 2>&1; then CROFT_IONICE="ionice -c3"; fi
$CROFT_NICE $CROFT_IONICE cargo install --path "$HOME/.cache/croft/source" --jobs "$CROFT_JOBS" --force --locked
mkdir -p "$HOME/.cache/croft"
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
/// "Updating…" indicator for the whole build+ship, not just the final
/// remote-side activation. Written before the (long) local cross-compile.
fn mark_remote_updating(ssh: &SshControl) {
    let _ = ssh
        .command()
        .arg(&ssh.host)
        .arg("mkdir -p \"$HOME/.cache/croft\" && : > \"$HOME/.cache/croft/updating\"")
        .status();
}

/// Clear the marker after a failed install so the indicator resolves to a
/// brief "update failed" rather than hanging on "Updating…" forever.
fn clear_remote_updating(ssh: &SshControl) {
    let _ = ssh
        .command()
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
fn ssh_control_socket_path_for_test(dir: &Path) -> PathBuf {
    dir.join("ctl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_to_local_exit_code_maps_to_return_to_local() {
        assert_eq!(
            classify_remote_status(Some(DROP_TO_LOCAL_EXIT_CODE)),
            RemoteStatusClass::ReturnToLocal
        );
        assert_eq!(classify_remote_status(Some(0)), RemoteStatusClass::Exited);
        assert_eq!(
            classify_remote_status(Some(127)),
            RemoteStatusClass::NotInstalled
        );
        assert_eq!(classify_remote_status(Some(1)), RemoteStatusClass::Failed);
        assert_eq!(classify_remote_status(None), RemoteStatusClass::Failed);
    }

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
            remote_croft_command_for_terminal(Some("/tmp/it's here"), None, None, false, &[]),
            "export CROFT_REMOTE_AUTOUPDATE=1; export PATH=\"$HOME/.cargo/bin:$PATH\"; command -v croft >/dev/null 2>&1 || { echo 'croft not found on remote PATH' >&2; exit 127; }; exec croft '/tmp/it'\"'\"'s here'"
        );
    }

    #[test]
    fn remote_croft_command_forwards_supported_terminal_program() {
        assert_eq!(
            remote_croft_command_for_terminal(None, Some("iTerm.app"), None, false, &[]),
            "export CROFT_REMOTE_AUTOUPDATE=1; export CROFT_FORCE_INLINE_IMAGES=1 TERM_PROGRAM='iTerm.app'; export PATH=\"$HOME/.cargo/bin:$PATH\"; command -v croft >/dev/null 2>&1 || { echo 'croft not found on remote PATH' >&2; exit 127; }; exec croft"
        );
    }

    #[test]
    fn remote_croft_command_exports_kitty_hint_for_ghostty() {
        // Ghostty: TERM_PROGRAM=ghostty locally, but SSH does not forward it, so
        // croft must export the hint itself. It must NOT force the OSC-1337 path
        // (Ghostty parses but ignores it); the remote resolves the Kitty
        // protocol from the exported TERM_PROGRAM.
        let command = remote_croft_command_for_terminal(None, Some("ghostty"), None, false, &[]);
        assert!(command.contains("export TERM_PROGRAM=ghostty;"));
        assert!(!command.contains("CROFT_FORCE_INLINE_IMAGES"));
    }

    #[test]
    fn remote_croft_command_exports_kitty_hint_from_term_only() {
        // A bare `kitty` terminal sets no TERM_PROGRAM; detection keys off TERM.
        let command =
            remote_croft_command_for_terminal(None, None, Some("xterm-kitty"), false, &[]);
        assert!(command.contains("export TERM_PROGRAM=ghostty;"));
    }

    #[test]
    fn remote_croft_command_exports_drop_relay_env() {
        let env = vec![
            (
                String::from("CROFT_DROP_RELAY_LOG"),
                String::from("/tmp/r/log"),
            ),
            (
                String::from("CROFT_DROP_RELAY_INBOX"),
                String::from("/tmp/r/inbox"),
            ),
        ];
        let command = remote_croft_command_for_terminal(None, None, None, false, &env);
        assert!(command.contains("export CROFT_DROP_RELAY_LOG='/tmp/r/log'"));
        assert!(command.contains("export CROFT_DROP_RELAY_INBOX='/tmp/r/inbox'"));
    }

    #[test]
    fn remote_croft_command_forwards_osk_flag_when_local_osk_is_armed() {
        // SSH from Termux does not forward TERMUX_VERSION, so a remote croft
        // would never auto-arm the on-screen keyboard; the launcher carries
        // the local detection across the hop explicitly.
        let command = remote_croft_command_for_terminal(None, None, None, true, &[]);
        assert!(command.contains("export CROFT_FORCE_OSK=1;"));
        let command = remote_croft_command_for_terminal(None, None, None, false, &[]);
        assert!(!command.contains("CROFT_FORCE_OSK"));
    }

    #[test]
    fn parse_relay_request_pulls_have_id_and_path() {
        match super::parse_relay_request("pull\tabc-123\t/Users/v/foo.txt") {
            Some(super::RelayRequest::Pull { id, src }) => {
                assert_eq!(id, "abc-123");
                assert_eq!(src, "/Users/v/foo.txt");
            }
            other => panic!("expected Pull, got {:?}", other.is_some()),
        }
    }

    #[test]
    fn activate_command_writes_stamp_then_clears_updating_marker() {
        let command = super::activate_command("deadbeef");
        assert!(command.contains("mv \"$HOME/.cargo/bin/croft.new\" \"$HOME/.cargo/bin/croft\""));
        let stamp_at = command
            .find("printf %s 'deadbeef' > \"$HOME/.cache/croft/install-stamp\"")
            .expect("stamp write present");
        let clear_at = command
            .find("rm -f \"$HOME/.cache/croft/updating\"")
            .expect("marker clear present");
        assert!(
            stamp_at < clear_at,
            "stamp must be written before marker clear"
        );
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

    // The 2026-06-12 "horrendous typing latency on various" report: the
    // fast cross-build silently fell back to `cargo install` ON the remote
    // box, and even niced, a rustc compile on a small VPS wrecks the live
    // session sharing it. When a croft session is running on the box, the
    // compile must yield everything: one job and idle-class IO.
    #[test]
    fn remote_install_compile_yields_to_a_live_croft_session() {
        let command = remote_install_command("abc123");
        assert!(
            command.contains("pgrep -x croft"),
            "must detect a live croft session on the box"
        );
        assert!(
            command.contains("CROFT_JOBS=1"),
            "a live session drops the compile to a single job"
        );
        assert!(
            command.contains("ionice") && command.contains("-c3"),
            "codegen writes must run in the idle IO class so the session's PTY never waits on them"
        );
        let gate = command.find("pgrep -x croft").unwrap();
        let install = command.find("cargo install --path").unwrap();
        assert!(gate < install, "the session check must precede the compile");
    }

    // A backgrounded install's log lines died with the connect dialog,
    // which is why the various fallback went undiagnosed. Every line must
    // also land in a persistent local file.
    #[test]
    fn install_log_tee_writes_lines_to_file_and_forwards_them_unchanged() {
        let dir = std::env::temp_dir().join(format!("croft-log-tee-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("install.log");
        let (down_tx, down_rx) = std::sync::mpsc::channel::<String>();
        let tee = spawn_log_tee(path.clone(), down_tx);
        tee.send(String::from("Cross-compiling croft locally…"))
            .unwrap();
        tee.send(String::from("Local cross-build skipped (zig missing)"))
            .unwrap();
        let timeout = std::time::Duration::from_secs(2);
        assert_eq!(
            down_rx.recv_timeout(timeout).unwrap(),
            "Cross-compiling croft locally…",
            "downstream consumers must still receive every line"
        );
        assert_eq!(
            down_rx.recv_timeout(timeout).unwrap(),
            "Local cross-build skipped (zig missing)"
        );
        drop(tee);
        let deadline = std::time::Instant::now() + timeout;
        let mut contents = String::new();
        while std::time::Instant::now() < deadline {
            contents = std::fs::read_to_string(&path).unwrap_or_default();
            if contents.contains("zig missing") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(contents.contains("Cross-compiling croft locally…"));
        assert!(
            contents.contains("Local cross-build skipped (zig missing)"),
            "the skip REASON is the whole point of the persistent log"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_install_command_ensures_c_toolchain_unconditionally() {
        // Regression: cargo can be present on a box that still lacks `cc`, so a
        // bare `cargo install` dies with `linker `cc` not found`. The toolchain
        // ensure must run on its own, not only when cargo+curl are both absent,
        // and it must precede the compile.
        let command = remote_install_command("abc123");
        assert!(command.contains("croft_ensure_build_toolchain"));
        assert!(command.contains("build-essential"));
        let ensure_call = command
            .rfind("\ncroft_ensure_build_toolchain")
            .expect("toolchain ensure must be invoked");
        let cargo_install = command
            .find("cargo install --path")
            .expect("cargo install must be present");
        assert!(
            ensure_call < cargo_install,
            "the C toolchain must be ensured before `cargo install` runs"
        );
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
        let path_pos = command
            .find("export PATH=\"$HOME/.cargo/bin:$PATH\"")
            .unwrap();
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

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    // The launch-now bug class: a background install shipping bytes through
    // the SAME multiplexed TCP connection as the live remote session queues
    // megabytes ahead of every keystroke (SSH head-of-line blocking) and
    // makes the running croft unusable. Every bulk rsync must honor the
    // bulk lane's routing and pacing.
    #[test]
    fn binary_ship_never_rides_the_interactive_master_when_lane_is_dedicated() {
        let lane = crate::remote_bulk::BulkLane::new(
            crate::remote_bulk::LaneMode::Dedicated {
                socket_path: PathBuf::from("/tmp/bulk/ctl"),
            },
            512,
        );
        let cmd = ship_binary_rsync_command(
            &lane,
            Path::new("/tmp/interactive/ctl"),
            Path::new("/tmp/target/croft"),
            "host:.cargo/bin/croft.new",
        );
        let args = args_of(&cmd);
        let e = args
            .iter()
            .position(|a| a == "-e")
            .map(|i| args[i + 1].clone())
            .expect("rsync must use an explicit -e remote shell");
        assert!(e.contains("/tmp/bulk/ctl"));
        assert!(!e.contains("/tmp/interactive/ctl"));
        assert!(
            args.iter().any(|a| a == "--bwlimit=512"),
            "unpaced bulk saturates the uplink queue and lags the session even on its own connection"
        );
    }

    #[test]
    fn binary_ship_throttles_on_the_shared_mux_when_no_lane_is_available() {
        let lane = crate::remote_bulk::BulkLane::new(crate::remote_bulk::LaneMode::SharedMux, 300);
        let cmd = ship_binary_rsync_command(
            &lane,
            Path::new("/tmp/interactive/ctl"),
            Path::new("/tmp/target/croft"),
            "host:.cargo/bin/croft.new",
        );
        let args = args_of(&cmd);
        let e = args
            .iter()
            .position(|a| a == "-e")
            .map(|i| args[i + 1].clone())
            .expect("rsync must use an explicit -e remote shell");
        assert!(e.contains("/tmp/interactive/ctl"));
        assert!(args.iter().any(|a| a == "--bwlimit=300"));
        assert!(args.iter().any(|a| a == "--checksum"));
    }

    #[test]
    fn source_sync_honors_the_bulk_lane_and_keeps_the_incremental_excludes() {
        let lane = crate::remote_bulk::BulkLane::new(
            crate::remote_bulk::LaneMode::Dedicated {
                socket_path: PathBuf::from("/tmp/bulk/ctl"),
            },
            700,
        );
        let cmd = source_sync_rsync_command(
            &lane,
            Path::new("/tmp/interactive/ctl"),
            std::ffi::OsStr::new("/Users/v/croft/"),
            "host:.cache/croft/source/",
        );
        let args = args_of(&cmd);
        let e = args
            .iter()
            .position(|a| a == "-e")
            .map(|i| args[i + 1].clone())
            .expect("rsync must use an explicit -e remote shell");
        assert!(e.contains("/tmp/bulk/ctl"));
        assert!(!e.contains("/tmp/interactive/ctl"));
        assert!(args.iter().any(|a| a == "--bwlimit=700"));
        assert!(
            args.iter().any(|a| a == "--exclude=target"),
            "shipping target/ would be gigabytes through the lane"
        );
        assert!(args.iter().any(|a| a == "--exclude=.git"));
        assert!(args.iter().any(|a| a == "--delete"));
    }

    // The 2026-06-12 root cause on various: croft inherited macOS's default
    // 256-fd soft limit, the zig linker needed ~250 rlibs open at once and
    // died with ProcessFdQuotaExceeded, and the installer silently fell
    // back to compiling on the remote box under the live session.
    #[test]
    fn raise_fd_limit_lifts_the_soft_limit_beyond_the_linkers_needs() {
        let soft = raise_fd_limit();
        assert!(
            soft >= 4096,
            "croft's cross-link opens ~250 rlibs plus zig's own files; a {soft}-fd soft limit reproduces ProcessFdQuotaExceeded"
        );
    }

    // Guards the exact bug class `sync_workspace_lock` exists to fix: a version
    // bump in Cargo.toml that leaves Cargo.lock one patch behind makes the
    // `--locked` remote cross-build fail and silently host-compile. A drifted
    // lock should never reach a commit, so fail the suite if the two disagree.
    #[test]
    fn cargo_lock_croft_version_matches_cargo_toml_so_locked_cross_build_never_falls_back() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let toml = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let toml_version = toml
            .lines()
            .find_map(|l| {
                l.strip_prefix("version = \"")
                    .and_then(|r| r.strip_suffix('"'))
            })
            .expect("Cargo.toml has a package version");

        let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap();
        let lock_version = lock
            .split("name = \"croft\"")
            .nth(1)
            .and_then(|after| {
                after.lines().find_map(|l| {
                    l.trim()
                        .strip_prefix("version = \"")
                        .and_then(|r| r.strip_suffix('"'))
                })
            })
            .expect("Cargo.lock has a croft entry");

        assert_eq!(
            lock_version, toml_version,
            "Cargo.lock croft version {lock_version} drifted from Cargo.toml {toml_version}; run `cargo update -p croft`"
        );
    }
}
